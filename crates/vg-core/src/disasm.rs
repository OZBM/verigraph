//! VG-IR back to VG-Sym (`vg disasm`). Names come from positions: parameters `a0`, θ `t0`, bound
//! variables `v0` by depth, ports by result slot, INVARIANT labels `@i0`, node labels `@n` plus a hash
//! prefix. Assembling the output gives back the same bytes (docs/13, lens laws).

use std::collections::{HashMap, HashSet};

use crate::ast::{self, *};
use crate::diag::{Diagnostic, Span};
use crate::float;
use crate::ir::{self, Bin, Hash, Node, Operand, Term};
use crate::lower::int_value;
use crate::opcode::{self, Modes};
use crate::types::{EffectMask, Type};

type R<T> = Result<T, Diagnostic>;

fn fail(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error("VG-S0204", Span::default(), msg)
}

/// Disassembles the roots with everything they call, callees first. `get` fetches a node by the
/// hash its callers hold (a node_hash, or a SPEC's spec_hash).
pub fn disasm(module: &str, roots: &[Hash], get: &dyn Fn(&Hash) -> Option<Node>) -> R<Module> {
    // Post-order over CALLS, without recursion.
    let mut order: Vec<(Hash, Node)> = Vec::new();
    let mut seen: HashSet<Hash> = HashSet::new();
    for root in roots {
        if !seen.insert(*root) {
            continue;
        }
        let n =
            get(root).ok_or_else(|| fail(format!("node {} is not in the store", ir::hex(root))))?;
        let mut stack = vec![(*root, n, 0usize)];
        while let Some((h, n, k)) = stack.last_mut() {
            if *k < n.calls.len() {
                let c = n.calls[*k];
                *k += 1;
                if seen.insert(c) {
                    let cn = get(&c).ok_or_else(|| {
                        fail(format!("callee {} is not in the store", ir::hex(&c)))
                    })?;
                    stack.push((c, cn, 0));
                }
            } else {
                let (h, n) = (*h, n.clone());
                stack.pop();
                order.push((h, n));
            }
        }
    }
    let hashes: Vec<Hash> = order.iter().map(|(h, _)| *h).collect();
    let labels = label_names(&hashes);
    let items = order
        .iter()
        .map(|(h, n)| node(n, &labels[h], &labels).map(Item::Node))
        .collect::<R<_>>()?;
    Ok(Module {
        name: module.into(),
        items,
    })
}

/// `n` plus the shortest hash prefix of at least 12 hex digits that is unique among the nodes.
fn label_names(hashes: &[Hash]) -> HashMap<Hash, String> {
    let hex: Vec<String> = hashes.iter().map(ir::hex).collect();
    let mut out = HashMap::new();
    for (i, h) in hex.iter().enumerate() {
        let mut len = 12;
        while hex
            .iter()
            .enumerate()
            .any(|(j, o)| j != i && o[..len] == h[..len])
        {
            len += 1;
        }
        out.insert(hashes[i], format!("n{}", &h[..len]));
    }
    out
}

pub fn type_expr(t: &Type) -> R<TypeExpr> {
    let bx = |t: &Type| type_expr(t).map(Box::new);
    let dims = |d: &[u32], bracketed: bool| Shape {
        bracketed,
        dims: d.iter().map(|n| Dim::Int(n.to_string())).collect(),
    };
    Ok(match t {
        Type::Bool => TypeExpr::Bool,
        Type::Eff => TypeExpr::Eff,
        Type::Handle(h) => TypeExpr::Handle(*h),
        Type::Array(e, n) => TypeExpr::Array(bx(e)?, dims(&[*n], false)),
        Type::Tensor(e, d) => TypeExpr::Tensor(bx(e)?, dims(d, true)),
        Type::Slice(e) => TypeExpr::Slice(bx(e)?),
        Type::Tuple(ms) => TypeExpr::Tuple(ms.iter().map(type_expr).collect::<R<_>>()?),
        Type::Result(a, b) => TypeExpr::Result(bx(a)?, bx(b)?),
        Type::Arena(n) => TypeExpr::Arena(n.to_string()),
        Type::Int => return Err(fail("the formula type Int has no VG-Sym spelling")),
        t => TypeExpr::Scalar(t.scalar_name().expect("scalar")),
    })
}

fn snum(neg: bool, text: String) -> SNum {
    SNum { neg, text }
}

/// One scalar of a constant's bytes, as a number or a Bool.
fn scalar(t: &Type, b: &[u8]) -> R<Elem> {
    if *t == Type::Bool {
        return Ok(Elem::Bool(b[0] != 0));
    }
    let mut w = [0u8; 8];
    w[..b.len()].copy_from_slice(b);
    let bits = u64::from_le_bytes(w);
    Ok(match t {
        Type::UInt(_) => Elem::Num(snum(false, bits.to_string())),
        Type::SInt(n) => {
            let shift = 64 - u32::from(*n);
            let v = ((bits << shift) as i64) >> shift;
            Elem::Num(snum(v < 0, v.unsigned_abs().to_string()))
        }
        t => {
            let (neg, text) = float::shortest(t, bits)
                .ok_or_else(|| fail(format!("a {t} NaN or infinity has no VG-Sym literal")))?;
            Elem::Num(snum(neg, text))
        }
    })
}

pub fn literal(c: &ir::Const) -> R<Literal> {
    let t = &c.ty;
    if t.is_scalar_or_bool() {
        return Ok(match scalar(t, &c.bytes)? {
            Elem::Bool(b) => Literal::Bool(b),
            Elem::Num(n) => Literal::Num(n, t.scalar_name().expect("scalar")),
            Elem::Array(_) => unreachable!("a scalar"),
        });
    }
    let (elem, dims): (&Type, Vec<u32>) = match t {
        Type::Array(e, n) => (e, vec![*n]),
        Type::Tensor(e, d) => (e, d.clone()),
        _ => {
            return Err(fail(format!(
                "a constant of type {t} has no VG-Sym literal"
            )));
        }
    };
    let size = elem
        .byte_size()
        .ok_or_else(|| fail("constant element without a size"))? as usize;
    fn rows(b: &[u8], elem: &Type, size: usize, dims: &[u32]) -> R<Vec<Elem>> {
        if dims.len() == 1 && !matches!(elem, Type::Array(..) | Type::Tensor(..)) {
            return b.chunks(size).map(|c| scalar(elem, c)).collect();
        }
        let n = dims[0] as usize;
        let stride = b.len() / n.max(1);
        (0..n)
            .map(|i| {
                let part = &b[i * stride..(i + 1) * stride];
                if dims.len() > 1 {
                    rows(part, elem, size, &dims[1..]).map(Elem::Array)
                } else {
                    match literal(&ir::Const {
                        ty: elem.clone(),
                        bytes: part.to_vec(),
                    })? {
                        Literal::Array(es, _) => Ok(Elem::Array(es)),
                        _ => unreachable!("a nested array"),
                    }
                }
            })
            .collect()
    }
    Ok(Literal::Array(
        rows(&c.bytes, elem, size, &dims)?,
        type_expr(t)?,
    ))
}

struct Names<'a> {
    n: &'a Node,
    labels: &'a HashMap<Hash, String>,
    ports: HashMap<(u32, u8), String>,
}

impl Names<'_> {
    fn input(&self, i: u32) -> String {
        let p = self.n.params.len() as u32;
        if i < p {
            format!("a{i}")
        } else {
            format!("t{}", i - p)
        }
    }

    fn callee(&self, c: u32) -> Name {
        Name::new(self.labels[&self.n.calls[c as usize]].clone())
    }

    fn value(&self, o: Operand) -> R<Value> {
        Ok(match o {
            Operand::Input(i) => Value::Ident(Name::new(self.input(i))),
            Operand::Op(i, s) => Value::Port(Name::new(self.ports[&(i, s)].clone())),
            Operand::Const(c) => Value::Lit(literal(&self.n.consts[c as usize])?, Span::default()),
            Operand::Callee(_) | Operand::Invariant(_) => unreachable!("not a value"),
        })
    }

    fn formula(&self, t: &Term, bound: &mut Vec<String>) -> R<Formula> {
        let f = |k| Ok(Formula::new(k));
        let bx = Box::new;
        match t {
            Term::Input(i) => f(FKind::Value(Value::Ident(Name::new(self.input(*i))))),
            Term::Result(_) if self.n.results.len() == 1 => f(FKind::Result),
            Term::Result(j) => f(FKind::Proj(bx(Formula::new(FKind::Result)), j.to_string())),
            Term::Port(i, s) => f(FKind::Value(Value::Port(Name::new(
                self.ports[&(*i, *s)].clone(),
            )))),
            Term::Bound(d) => {
                let name = bound
                    .get(bound.len().wrapping_sub(1 + *d as usize))
                    .ok_or_else(|| fail("unbound variable"))?;
                f(FKind::Value(Value::Ident(Name::new(name.clone()))))
            }
            Term::Const(c) => {
                let c = &self.n.consts[*c as usize];
                if c.ty == Type::Int {
                    match int_value(&c.bytes) {
                        Some(v) if v >= 0 => f(FKind::Num(v.to_string())),
                        _ => Err(fail("a negative Int constant has no VG-Sym spelling")),
                    }
                } else {
                    f(FKind::Value(Value::Lit(literal(c)?, Span::default())))
                }
            }
            Term::Resource(r) => f(FKind::Value(Value::Ident(Name::new(r.name())))),
            Term::Not(a) => f(FKind::Unary(UnOp::Not, bx(self.formula(a, bound)?))),
            Term::Neg(a) => f(FKind::Unary(UnOp::Neg, bx(self.formula(a, bound)?))),
            Term::Bin(b, x, y) => {
                let op = match b {
                    Bin::And => BinOp::And,
                    Bin::Or => BinOp::Or,
                    Bin::Implies => BinOp::Implies,
                    Bin::Eq => BinOp::Eq,
                    Bin::Ne => BinOp::Ne,
                    Bin::Lt => BinOp::Lt,
                    Bin::Le => BinOp::Le,
                    Bin::Gt => BinOp::Gt,
                    Bin::Ge => BinOp::Ge,
                    Bin::Add => BinOp::Add,
                    Bin::Sub => BinOp::Sub,
                    Bin::Mul => BinOp::Mul,
                    Bin::Div => BinOp::Div,
                    Bin::Mod => BinOp::Mod,
                };
                f(FKind::Binary(
                    op,
                    bx(self.formula(x, bound)?),
                    bx(self.formula(y, bound)?),
                ))
            }
            Term::Index(b, idx) => f(FKind::Index(
                bx(self.formula(b, bound)?),
                idx.iter()
                    .map(|i| self.formula(i, bound))
                    .collect::<R<_>>()?,
            )),
            Term::Proj(b, j) => f(FKind::Proj(bx(self.formula(b, bound)?), j.to_string())),
            Term::Forall(rs, body) => {
                let depth0 = bound.len();
                let mut ranges = Vec::new();
                for (lo, hi) in rs {
                    let (lo, hi) = (self.formula(lo, bound)?, self.formula(hi, bound)?);
                    let var = format!("v{}", bound.len());
                    bound.push(var.clone());
                    ranges.push(ast::Range {
                        var: Name::new(var),
                        lo,
                        hi,
                    });
                }
                let body = self.formula(body, bound);
                bound.truncate(depth0);
                f(FKind::Forall {
                    ranges,
                    body: bx(body?),
                })
            }
            Term::Builtin(b, args) => f(FKind::Call {
                prefix: None,
                name: Name::new(b.name()),
                args: args
                    .iter()
                    .map(|a| self.formula(a, bound))
                    .collect::<R<_>>()?,
            }),
            Term::Spec(c, args) | Term::Fold(c, args) | Term::Apply(c, args) => f(FKind::Call {
                prefix: match t {
                    Term::Spec(..) => Some(CallPrefix::Spec),
                    Term::Fold(..) => Some(CallPrefix::Fold),
                    _ => None,
                },
                name: self.callee(*c),
                args: args
                    .iter()
                    .map(|a| self.formula(a, bound))
                    .collect::<R<_>>()?,
            }),
        }
    }

    /// A clause as one conjunction, left-nested as the parser builds it.
    fn conjunction(&self, conj: &[Term], bound: &mut Vec<String>) -> R<Formula> {
        let mut it = conj.iter();
        let mut f = self.formula(it.next().expect("non-empty"), bound)?;
        for t in it {
            f = Formula::new(FKind::Binary(
                BinOp::And,
                Box::new(f),
                Box::new(self.formula(t, bound)?),
            ));
        }
        Ok(f)
    }
}

fn node(n: &Node, label: &str, labels: &HashMap<Hash, String>) -> R<NodeDef> {
    let mut ports = HashMap::new();
    let mut next = 0u32;
    for (i, op) in n.ops.iter().enumerate() {
        for s in 0..op.results.len() {
            ports.insert((i as u32, s as u8), next.to_string());
            next += 1;
        }
    }
    let names = Names { n, labels, ports };
    let mut clauses = Vec::new();
    let c = &n.contract;
    for (kind, conj) in [
        (ClauseKind::Pre, &c.pre),
        (ClauseKind::Post, &c.post),
        (ClauseKind::Cost, &c.cost),
        (ClauseKind::Budget, &c.budget),
        (ClauseKind::Decreases, &c.decreases),
    ] {
        if !conj.is_empty() {
            clauses.push(Clause {
                kind,
                formula: names.conjunction(conj, &mut vec![])?,
                span: Span::default(),
            });
        }
    }
    for (v, conj) in c.invariants.iter().enumerate() {
        let mut bound = vec!["v0".to_string(), "v1".to_string()];
        clauses.push(Clause {
            kind: ClauseKind::Invariant {
                label: Name::new(format!("i{v}")),
                k: Name::new("v0"),
                acc: Name::new("v1"),
            },
            formula: names.conjunction(conj, &mut bound)?,
            span: Span::default(),
        });
    }
    let learnable = if n.learnable.is_empty() {
        None
    } else {
        let mut v = Vec::new();
        for (j, l) in n.learnable.iter().enumerate() {
            let bound = |c: u32| -> R<SNum> {
                match scalar(&l.ty, &n.consts[c as usize].bytes)? {
                    Elem::Num(s) => Ok(s),
                    _ => Err(fail("a LEARNABLE bound is a number")),
                }
            };
            v.push(Learn {
                name: Name::new(format!("t{j}")),
                ty: l.ty.scalar_name().expect("a float type"),
                lo: bound(l.lo)?,
                hi: bound(l.hi)?,
                loss: l.loss.map(|c| names.callee(c)),
            });
        }
        Some(v)
    };
    let mut body = Vec::new();
    for (i, op) in n.ops.iter().enumerate() {
        let info = opcode::by_code(op.opcode).expect("validated");
        let annotate = matches!(op.opcode, opcode::OK | opcode::ERR);
        let binds = (0..op.results.len())
            .map(|s| {
                Ok(Bind {
                    port: Name::new(names.ports[&(i as u32, s as u8)].clone()),
                    ty: if annotate {
                        Some(type_expr(&op.results[s])?)
                    } else {
                        None
                    },
                })
            })
            .collect::<R<Vec<_>>>()?;
        let mode = if info.modes == Modes::Cast {
            op.results[0].scalar_name().map(Name::new)
        } else {
            info.mode_name(op.mode).expect("validated").map(Name::new)
        };
        let k = op.operands.len();
        let mut args = Vec::new();
        for (j, o) in op.operands.iter().enumerate() {
            let name = match (info.shape, j + 1 == k) {
                (opcode::Shape::Fold, true) => Some(Name::new("invariant")),
                (opcode::Shape::Fold, false) if j + 2 == k => Some(Name::new("init")),
                (opcode::Shape::Guard, true) => Some(Name::new("else")),
                _ => None,
            };
            let val = match o {
                Operand::Callee(c) => ArgVal::Label(names.callee(*c)),
                Operand::Invariant(v) => ArgVal::Label(Name::new(format!("i{v}"))),
                o => ArgVal::Value(names.value(*o)?),
            };
            args.push(Arg {
                name,
                val,
                span: Span::default(),
            });
        }
        body.push(Stmt::Assign {
            binds,
            op: Name::new(info.name),
            mode,
            args,
            span: Span::default(),
        });
    }
    body.push(Stmt::Return {
        values: n
            .outputs
            .iter()
            .map(|o| names.value(*o))
            .collect::<R<_>>()?,
        span: Span::default(),
    });
    Ok(NodeDef {
        spec: n.spec,
        label: label.into(),
        attrs: vec![],
        params: n
            .params
            .iter()
            .enumerate()
            .map(|(i, t)| {
                Ok(Param {
                    name: Name::new(format!("a{i}")),
                    ty: type_expr(t)?,
                })
            })
            .collect::<R<_>>()?,
        results: n.results.iter().map(type_expr).collect::<R<_>>()?,
        effects: EffectMask(n.effects.0)
            .names()
            .into_iter()
            .map(Name::new)
            .collect(),
        clauses,
        learnable,
        body,
        span: Span::default(),
    })
}
