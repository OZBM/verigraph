//! Concrete contract formula evaluator for PRE, POST, INVARIANT, `spec:`, and `fold:`
//! (docs/13, docs/05, ADR 0007, ADR 0008, ADR 0010).

use vg_core::float;
use vg_core::ir::{Bin, Builtin, Hash, Node, Operand, Term};
use vg_core::lower::int_value;
use vg_core::opcode;
use vg_core::types::Type;
use vg_core::value::{Value, int_fits};

use crate::bigint::BigInt;
use crate::bytes::{decode_const, val_eq, val_same, validate_and_canonicalize, value_to_bytes};
use crate::eval;
use crate::float_ops;
use crate::host::{NoEffectsHost, Trap};

const MAX_CALL_DEPTH: usize = 256;

/// Which contract clause [`holds`] evaluates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clause {
    Pre,
    Post,
}

/// Concrete truth of a node's `PRE` (needs only `inputs`) or `POST` (`inputs` and `results`),
/// checking every conjunct in order and short-circuiting on `false`.
pub fn holds(
    node: &Node,
    clause: Clause,
    inputs: &[Value],
    results: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Result<bool, Trap> {
    holds_with_depth(node, clause, inputs, results, callee, 0)
}

pub(crate) fn holds_with_depth(
    node: &Node,
    clause: Clause,
    inputs: &[Value],
    results: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
    depth: usize,
) -> Result<bool, Trap> {
    if depth > MAX_CALL_DEPTH {
        return Err(Trap::formula("maximum formula call depth exceeded"));
    }
    let norm_inputs = normalize_inputs(node, inputs)?;
    let norm_results = match clause {
        Clause::Pre => None,
        Clause::Post => Some(normalize_results(node, results)?),
    };
    let consts = decode_formula_consts(node)?;
    let terms = match clause {
        Clause::Pre => &node.contract.pre,
        Clause::Post => &node.contract.post,
    };
    let mut cx = FormulaCx {
        node,
        inputs: &norm_inputs,
        results: norm_results.as_deref(),
        ports: None,
        consts: &consts,
        env: Vec::new(),
        callee,
        depth,
    };
    for t in terms {
        if !cx.eval_bool(t)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Concrete truth of `INVARIANT` clause `inv_idx` at fold step `step_k` with accumulator `acc`,
/// given the enclosing node's `inputs` and evaluated `port_values` (for `Term::Port`).
pub fn holds_invariant(
    node: &Node,
    inv_idx: u32,
    step_k: i128,
    acc: &Value,
    inputs: &[Value],
    port_values: &[Vec<Value>],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Result<bool, Trap> {
    let terms = node
        .contract
        .invariants
        .get(inv_idx as usize)
        .ok_or_else(|| Trap::formula(format!("INVARIANT index {inv_idx} out of range")))?;
    if terms.is_empty() {
        return Ok(true);
    }
    let acc_ty = node
        .ops
        .iter()
        .find(|op| {
            op.opcode == opcode::FOLD_BOUNDED
                && op.operands.last() == Some(&Operand::Invariant(inv_idx))
        })
        .and_then(|op| op.results.first())
        .ok_or_else(|| {
            Trap::formula(format!(
                "INVARIANT @{inv_idx} is not used by any FOLD_BOUNDED in this node"
            ))
        })?;
    let norm_inputs = normalize_inputs(node, inputs)?;
    let acc_canon = validate_and_canonicalize(acc_ty, acc)
        .map_err(|e| Trap::formula(format!("accumulator ({acc_ty}): {e}")))?;
    let acc_fval = FVal::from_value(acc_ty, &acc_canon)?;
    let consts = decode_formula_consts(node)?;
    let mut cx = FormulaCx {
        node,
        inputs: &norm_inputs,
        results: None,
        ports: Some(port_values),
        consts: &consts,
        // Outermost first: Bound(1) = step_k, Bound(0) = acc (ir.rs).
        env: vec![FVal::Int(BigInt::from(step_k), None), acc_fval],
        callee,
        depth: 0,
    };
    for t in terms {
        if !cx.eval_bool(t)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn normalize_inputs(node: &Node, inputs: &[Value]) -> Result<Vec<Value>, Trap> {
    if inputs.len() != node.input_count() {
        return Err(Trap::formula(format!(
            "expected {} inputs (params + learnable theta), got {}",
            node.input_count(),
            inputs.len()
        )));
    }
    let mut out = Vec::with_capacity(inputs.len());
    for (i, val) in inputs.iter().enumerate() {
        let ty = node
            .input_type(i as u32)
            .ok_or_else(|| Trap::formula(format!("input index {i} out of range")))?;
        let canon = validate_and_canonicalize(ty, val)
            .map_err(|e| Trap::formula(format!("input #{i} ({ty}): {e}")))?;
        out.push(canon);
    }
    Ok(out)
}

fn normalize_results(node: &Node, results: &[Value]) -> Result<Vec<Value>, Trap> {
    if results.len() != node.results.len() {
        return Err(Trap::formula(format!(
            "expected {} results, got {}",
            node.results.len(),
            results.len()
        )));
    }
    let mut out = Vec::with_capacity(results.len());
    for (j, val) in results.iter().enumerate() {
        let ty = &node.results[j];
        let canon = validate_and_canonicalize(ty, val)
            .map_err(|e| Trap::formula(format!("result #{j} ({ty}): {e}")))?;
        out.push(canon);
    }
    Ok(out)
}

fn decode_formula_consts(node: &Node) -> Result<Vec<FVal>, Trap> {
    let mut out = Vec::with_capacity(node.consts.len());
    for (ci, c) in node.consts.iter().enumerate() {
        if c.ty == Type::Int {
            let v = int_value(&c.bytes)
                .ok_or_else(|| Trap::formula(format!("invalid Int constant #{ci}")))?;
            out.push(FVal::Int(BigInt::from(v), None));
        } else {
            let val = decode_const(c)
                .map_err(|e| Trap::formula(format!("constant #{ci} ({}): {e}", c.ty)))?;
            out.push(FVal::from_value(&c.ty, &val)?);
        }
    }
    Ok(out)
}

#[derive(Clone, Debug)]
enum FVal {
    Bool(bool),
    /// Mathematical integer. `Some(ty)` only when originating directly from a typed value so
    /// `as_bytes` knows its byte width; `None` for formula-level integers.
    Int(BigInt, Option<Type>),
    Float(Type, u64),
    Val(Type, Value),
}

impl FVal {
    fn from_value(ty: &Type, val: &Value) -> Result<Self, Trap> {
        match (ty, val) {
            (Type::Bool, Value::Bool(b)) => Ok(Self::Bool(*b)),
            (Type::UInt(_) | Type::SInt(_), Value::Int(v)) => {
                Ok(Self::Int(BigInt::from(*v), Some(ty.clone())))
            }
            (t, Value::Float(bits)) if t.is_float() => {
                Ok(Self::Float(t.clone(), float_ops::canonicalize(t, *bits)))
            }
            _ => Ok(Self::Val(ty.clone(), val.clone())),
        }
    }

    fn to_value(&self, target_ty: &Type) -> Result<Value, Trap> {
        match (self, target_ty) {
            (Self::Bool(b), Type::Bool) => Ok(Value::Bool(*b)),
            (Self::Int(n, _), Type::UInt(_) | Type::SInt(_)) => {
                let v = n
                    .to_i128()
                    .filter(|&x| int_fits(x, target_ty))
                    .ok_or_else(|| {
                        Trap::formula(format!(
                            "integer {n} does not fit parameter type {target_ty}"
                        ))
                    })?;
                Ok(Value::Int(v))
            }
            (Self::Float(t, bits), target) if t == target => Ok(Value::Float(*bits)),
            (Self::Val(Type::Array(e1, _), Value::Seq(xs)), Type::Slice(e2)) if e1 == e2 => {
                Ok(Value::Seq(xs.clone()))
            }
            (Self::Val(t, v), target) if t == target => Ok(v.clone()),
            _ => Err(Trap::formula(format!(
                "formula value {self:?} cannot be converted to {target_ty}"
            ))),
        }
    }
}

struct FormulaCx<'a> {
    node: &'a Node,
    inputs: &'a [Value],
    results: Option<&'a [Value]>,
    ports: Option<&'a [Vec<Value>]>,
    consts: &'a [FVal],
    env: Vec<FVal>,
    callee: &'a dyn Fn(&Hash) -> Option<Node>,
    depth: usize,
}

impl<'a> FormulaCx<'a> {
    fn eval_bool(&mut self, t: &Term) -> Result<bool, Trap> {
        match self.eval(t)? {
            FVal::Bool(b) => Ok(b),
            other => Err(Trap::formula(format!(
                "expected Bool in formula, got {other:?}"
            ))),
        }
    }

    fn eval_int(&mut self, t: &Term) -> Result<BigInt, Trap> {
        match self.eval(t)? {
            FVal::Int(n, _) => Ok(n),
            other => Err(Trap::formula(format!(
                "expected integer in formula, got {other:?}"
            ))),
        }
    }

    fn eval(&mut self, t: &Term) -> Result<FVal, Trap> {
        match t {
            Term::Input(i) => {
                let ty = self
                    .node
                    .input_type(*i)
                    .ok_or_else(|| Trap::formula(format!("formula input {i} out of range")))?;
                let val = self
                    .inputs
                    .get(*i as usize)
                    .ok_or_else(|| Trap::formula(format!("formula input {i} missing")))?;
                FVal::from_value(ty, val)
            }
            Term::Result(j) => {
                let results = self
                    .results
                    .ok_or_else(|| Trap::formula("`result` referenced outside POST".to_string()))?;
                let ty = self
                    .node
                    .results
                    .get(*j as usize)
                    .ok_or_else(|| Trap::formula(format!("result.{j} out of range")))?;
                let val = results
                    .get(*j as usize)
                    .ok_or_else(|| Trap::formula(format!("result.{j} missing")))?;
                FVal::from_value(ty, val)
            }
            Term::Port(i, slot) => {
                let ports = self.ports.ok_or_else(|| {
                    Trap::formula(format!("port %{i}.{slot} referenced outside INVARIANT"))
                })?;
                let op = self
                    .node
                    .ops
                    .get(*i as usize)
                    .ok_or_else(|| Trap::formula(format!("port op {i} out of range")))?;
                let ty = op.results.get(*slot as usize).ok_or_else(|| {
                    Trap::formula(format!("port slot ({i}, {slot}) out of range"))
                })?;
                let val = ports
                    .get(*i as usize)
                    .and_then(|rs| rs.get(*slot as usize))
                    .ok_or_else(|| Trap::formula(format!("port value ({i}, {slot}) missing")))?;
                FVal::from_value(ty, val)
            }
            Term::Bound(db) => {
                let idx = self
                    .env
                    .len()
                    .checked_sub(1 + *db as usize)
                    .ok_or_else(|| Trap::formula(format!("unbound de Bruijn index {db}")))?;
                Ok(self.env[idx].clone())
            }
            Term::Const(c) => self
                .consts
                .get(*c as usize)
                .cloned()
                .ok_or_else(|| Trap::formula(format!("formula const {c} out of range"))),
            Term::Resource(r) => Err(Trap::formula(format!(
                "resource `{}` cannot be evaluated concretely",
                r.name()
            ))),
            Term::Not(a) => Ok(FVal::Bool(!self.eval_bool(a)?)),
            Term::Neg(a) => match self.eval(a)? {
                FVal::Int(n, _) => Ok(FVal::Int(-n, None)),
                FVal::Float(ty, bits) => Ok(FVal::Float(ty.clone(), float_ops::fneg(&ty, bits))),
                other => Err(Trap::formula(format!("unary `-` on {other:?}"))),
            },
            Term::Bin(op, a, b) => self.eval_bin(*op, a, b),
            Term::Index(base, idxs) => self.eval_index(base, idxs),
            Term::Proj(base, j) => match self.eval(base)? {
                FVal::Val(Type::Tuple(ts), Value::Tuple(xs)) => {
                    let ty = ts.get(*j as usize).ok_or_else(|| {
                        Trap::formula(format!("tuple projection .{j} out of range"))
                    })?;
                    let val = xs
                        .get(*j as usize)
                        .ok_or_else(|| Trap::formula(format!("tuple projection .{j} missing")))?;
                    FVal::from_value(ty, val)
                }
                other => Err(Trap::formula(format!("projection .{j} on {other:?}"))),
            },
            Term::Forall(ranges, body) => Ok(FVal::Bool(self.eval_forall(ranges, body)?)),
            Term::Builtin(b, args) => self.eval_builtin(*b, args),
            Term::Spec(c, args) => self.eval_spec(*c, args),
            Term::Fold(c, args) => self.eval_fold(*c, args),
            Term::Apply(c, args) => self.eval_apply(*c, args),
        }
    }

    fn eval_bin(&mut self, op: Bin, a: &Term, b: &Term) -> Result<FVal, Trap> {
        // Logical connectives short-circuit so well-definedness guards (ADR 0010) work concretely.
        match op {
            Bin::And => {
                if !self.eval_bool(a)? {
                    return Ok(FVal::Bool(false));
                }
                return Ok(FVal::Bool(self.eval_bool(b)?));
            }
            Bin::Or => {
                if self.eval_bool(a)? {
                    return Ok(FVal::Bool(true));
                }
                return Ok(FVal::Bool(self.eval_bool(b)?));
            }
            Bin::Implies => {
                if !self.eval_bool(a)? {
                    return Ok(FVal::Bool(true));
                }
                return Ok(FVal::Bool(self.eval_bool(b)?));
            }
            _ => {}
        }

        let va = self.eval(a)?;
        let vb = self.eval(b)?;
        match op {
            Bin::Eq | Bin::Ne => {
                let eq = match (va, vb) {
                    (FVal::Bool(x), FVal::Bool(y)) => x == y,
                    (FVal::Int(x, _), FVal::Int(y, _)) => x == y,
                    (FVal::Float(t1, x), FVal::Float(t2, y)) if t1 == t2 => {
                        float_ops::feq(&t1, x, y)
                    }
                    (FVal::Val(t1, x), FVal::Val(t2, y)) if t1 == t2 => {
                        val_eq(&t1, &x, &y).map_err(Trap::formula)?
                    }
                    (x, y) => {
                        return Err(Trap::formula(format!(
                            "incompatible types in formula equality: {x:?} vs {y:?}"
                        )));
                    }
                };
                Ok(FVal::Bool(if op == Bin::Eq { eq } else { !eq }))
            }
            Bin::Lt | Bin::Le | Bin::Gt | Bin::Ge => {
                let res = match (va, vb) {
                    (FVal::Int(x, _), FVal::Int(y, _)) => match op {
                        Bin::Lt => x < y,
                        Bin::Le => x <= y,
                        Bin::Gt => x > y,
                        _ => x >= y,
                    },
                    (FVal::Float(t1, x), FVal::Float(t2, y)) if t1 == t2 => match op {
                        Bin::Lt => float_ops::flt(&t1, x, y),
                        Bin::Le => float_ops::fle(&t1, x, y),
                        Bin::Gt => float_ops::fgt(&t1, x, y),
                        _ => float_ops::fge(&t1, x, y),
                    },
                    (x, y) => {
                        return Err(Trap::formula(format!(
                            "incompatible types in formula comparison: {x:?} vs {y:?}"
                        )));
                    }
                };
                Ok(FVal::Bool(res))
            }
            Bin::Add | Bin::Sub | Bin::Mul | Bin::Div | Bin::Mod => match (va, vb) {
                (FVal::Int(x, _), FVal::Int(y, _)) => {
                    let res = match op {
                        Bin::Add => &x + &y,
                        Bin::Sub => &x - &y,
                        Bin::Mul => &x * &y,
                        Bin::Div => {
                            x.div_rem(&y)
                                .ok_or_else(|| {
                                    Trap::formula(format!("formula division by zero: {x} / 0"))
                                })?
                                .0
                        }
                        Bin::Mod => {
                            x.div_rem(&y)
                                .ok_or_else(|| {
                                    Trap::formula(format!("formula modulo by zero: {x} % 0"))
                                })?
                                .1
                        }
                        _ => unreachable!(),
                    };
                    Ok(FVal::Int(res, None))
                }
                (FVal::Float(t1, x), FVal::Float(t2, y)) if t1 == t2 => {
                    let bits = match op {
                        Bin::Add => float_ops::fadd(&t1, x, y),
                        Bin::Sub => float_ops::fsub(&t1, x, y),
                        Bin::Mul => float_ops::fmul(&t1, x, y),
                        Bin::Div => float_ops::fdiv(&t1, x, y),
                        Bin::Mod => {
                            return Err(Trap::formula("`%` is not defined on float terms"));
                        }
                        _ => unreachable!(),
                    };
                    Ok(FVal::Float(t1, bits))
                }
                (x, y) => Err(Trap::formula(format!(
                    "incompatible types in formula arithmetic: {x:?} vs {y:?}"
                ))),
            },
            Bin::And | Bin::Or | Bin::Implies => unreachable!("handled above"),
        }
    }

    fn eval_index(&mut self, base: &Term, idxs: &[Term]) -> Result<FVal, Trap> {
        let FVal::Val(ty, Value::Seq(elems)) = self.eval(base)? else {
            return Err(Trap::formula("indexing requires a collection value"));
        };
        match &ty {
            Type::Array(elem, _) | Type::Slice(elem) => {
                if idxs.len() != 1 {
                    return Err(Trap::formula(format!(
                        "1D collection {ty} indexed with {} indices",
                        idxs.len()
                    )));
                }
                let k = self.eval_int(&idxs[0])?;
                let idx = k.to_usize().filter(|&u| u < elems.len()).ok_or_else(|| {
                    Trap::formula(format!(
                        "formula index {k} out of bounds for length {}",
                        elems.len()
                    ))
                })?;
                FVal::from_value(elem, &elems[idx])
            }
            Type::Tensor(elem, dims) => {
                if idxs.len() != dims.len() {
                    return Err(Trap::formula(format!(
                        "tensor {ty} indexed with {} indices",
                        idxs.len()
                    )));
                }
                let mut flat = 0usize;
                for (m, (idx_t, &dim)) in idxs.iter().zip(dims).enumerate() {
                    let k = self.eval_int(idx_t)?;
                    let ku = k.to_usize().filter(|&u| u < dim as usize).ok_or_else(|| {
                        Trap::formula(format!(
                            "tensor index #{m} = {k} out of bounds for dim {dim}"
                        ))
                    })?;
                    flat = flat * (dim as usize) + ku;
                }
                let val = elems
                    .get(flat)
                    .ok_or_else(|| Trap::formula(format!("tensor flat index {flat} missing")))?;
                FVal::from_value(elem, val)
            }
            _ => Err(Trap::formula(format!("cannot index type {ty}"))),
        }
    }

    fn eval_forall(&mut self, ranges: &[(Term, Term)], body: &Term) -> Result<bool, Trap> {
        let Some(((lo_t, hi_t), rest)) = ranges.split_first() else {
            return self.eval_bool(body);
        };
        let lo = self.eval_int(lo_t)?;
        let hi = self.eval_int(hi_t)?;
        let mut cur = lo;
        while cur < hi {
            self.env.push(FVal::Int(cur.clone(), None));
            let res = self.eval_forall(rest, body);
            self.env.pop();
            if !res? {
                return Ok(false);
            }
            cur = cur + BigInt::one();
        }
        Ok(true)
    }

    fn eval_builtin(&mut self, b: Builtin, args: &[Term]) -> Result<FVal, Trap> {
        if args.len() != b.arity() {
            return Err(Trap::formula(format!(
                "builtin `{}` expects {} args, got {}",
                b.name(),
                b.arity(),
                args.len()
            )));
        }
        match b {
            Builtin::Len => {
                let FVal::Val(ty, Value::Seq(elems)) = self.eval(&args[0])? else {
                    return Err(Trap::formula("`len` requires Array, Slice, or Tensor"));
                };
                if ty.elem().is_none() {
                    return Err(Trap::formula(format!("`len` unsupported on {ty}")));
                }
                Ok(FVal::Int(BigInt::from(elems.len()), None))
            }
            Builtin::IsFinite => match self.eval(&args[0])? {
                FVal::Float(ty, bits) => Ok(FVal::Bool(
                    !float::is_nan(&ty, bits) && !float::is_inf(&ty, bits),
                )),
                other => Err(Trap::formula(format!("`is_finite` on non-float {other:?}"))),
            },
            Builtin::IsNan => match self.eval(&args[0])? {
                FVal::Float(ty, bits) => Ok(FVal::Bool(float::is_nan(&ty, bits))),
                other => Err(Trap::formula(format!("`is_nan` on non-float {other:?}"))),
            },
            Builtin::Abs => match self.eval(&args[0])? {
                FVal::Int(n, _) => Ok(FVal::Int(n.abs(), None)),
                FVal::Float(ty, bits) => Ok(FVal::Float(ty.clone(), float_ops::fabs(&ty, bits))),
                other => Err(Trap::formula(format!("`abs` on {other:?}"))),
            },
            Builtin::Min | Builtin::Max => {
                let va = self.eval(&args[0])?;
                let vb = self.eval(&args[1])?;
                match (va, vb) {
                    (FVal::Int(x, _), FVal::Int(y, _)) => Ok(FVal::Int(
                        if b == Builtin::Min {
                            x.min(y)
                        } else {
                            x.max(y)
                        },
                        None,
                    )),
                    (FVal::Float(t1, x), FVal::Float(t2, y)) if t1 == t2 => {
                        let bits = if b == Builtin::Min {
                            float_ops::fmin(&t1, x, y)
                        } else {
                            float_ops::fmax(&t1, x, y)
                        };
                        Ok(FVal::Float(t1, bits))
                    }
                    (x, y) => Err(Trap::formula(format!(
                        "`{}` on incompatible operands {x:?}, {y:?}",
                        b.name()
                    ))),
                }
            }
            Builtin::AsBytes => {
                let v = self.eval(&args[0])?;
                let bytes = match v {
                    FVal::Bool(flag) => vec![u8::from(flag)],
                    FVal::Int(n, Some(ty)) => {
                        let iv = n.to_i128().ok_or_else(|| {
                            Trap::formula(format!("`as_bytes` integer {n} out of i128 range"))
                        })?;
                        value_to_bytes(&ty, &Value::Int(iv)).map_err(Trap::formula)?
                    }
                    FVal::Int(_, None) => {
                        return Err(Trap::formula(
                            "`as_bytes` cannot encode an untyped mathematical integer",
                        ));
                    }
                    FVal::Float(ty, bits) => {
                        value_to_bytes(&ty, &Value::Float(bits)).map_err(Trap::formula)?
                    }
                    FVal::Val(ty, val) => value_to_bytes(&ty, &val).map_err(Trap::formula)?,
                };
                Ok(FVal::Val(
                    Type::Slice(Box::new(Type::UInt(8))),
                    Value::Seq(
                        bytes
                            .into_iter()
                            .map(|b| Value::Int(i128::from(b)))
                            .collect(),
                    ),
                ))
            }
            Builtin::Same => {
                let va = self.eval(&args[0])?;
                let vb = self.eval(&args[1])?;
                let res = match (va, vb) {
                    (FVal::Bool(x), FVal::Bool(y)) => x == y,
                    (FVal::Int(x, _), FVal::Int(y, _)) => x == y,
                    (FVal::Float(t1, x), FVal::Float(t2, y)) if t1 == t2 => {
                        float_ops::canonicalize(&t1, x) == float_ops::canonicalize(&t1, y)
                    }
                    (FVal::Val(t1, x), FVal::Val(t2, y)) if t1 == t2 => {
                        val_same(&t1, &x, &y).map_err(Trap::formula)?
                    }
                    (x, y) => {
                        return Err(Trap::formula(format!(
                            "`same` on incompatible operands {x:?}, {y:?}"
                        )));
                    }
                };
                Ok(FVal::Bool(res))
            }
            Builtin::IsOk => match self.eval(&args[0])? {
                FVal::Val(Type::Result(..), Value::Ok(_)) => Ok(FVal::Bool(true)),
                FVal::Val(Type::Result(..), Value::Err(_)) => Ok(FVal::Bool(false)),
                other => Err(Trap::formula(format!("`is_ok` on non-Result {other:?}"))),
            },
            Builtin::Ok => match self.eval(&args[0])? {
                FVal::Val(Type::Result(ok_t, _), Value::Ok(inner)) => {
                    FVal::from_value(&ok_t, &inner)
                }
                FVal::Val(Type::Result(..), Value::Err(e)) => {
                    Err(Trap::formula(format!("`ok(r)` evaluated on Err({e:?})")))
                }
                other => Err(Trap::formula(format!("`ok` on non-Result {other:?}"))),
            },
            Builtin::Err => match self.eval(&args[0])? {
                FVal::Val(Type::Result(_, err_t), Value::Err(inner)) => {
                    FVal::from_value(&err_t, &inner)
                }
                FVal::Val(Type::Result(..), Value::Ok(v)) => {
                    Err(Trap::formula(format!("`err(r)` evaluated on Ok({v:?})")))
                }
                other => Err(Trap::formula(format!("`err` on non-Result {other:?}"))),
            },
        }
    }

    fn eval_spec(&mut self, call_idx: u32, arg_terms: &[Term]) -> Result<FVal, Trap> {
        let h = self
            .node
            .calls
            .get(call_idx as usize)
            .copied()
            .ok_or_else(|| Trap::formula(format!("spec call index {call_idx} out of range")))?;
        let spec_node = (self.callee)(&h).ok_or_else(|| {
            Trap::formula(format!("missing SPEC callee {}", vg_core::ir::hex(&h)))
        })?;
        if !spec_node.spec {
            return Err(Trap::formula(format!(
                "spec call {} names a NODE, not a SPEC",
                vg_core::ir::hex(&h)
            )));
        }
        if spec_node.effects.0 != 0 {
            return Err(Trap::formula("SPEC node cannot have effects"));
        }
        if arg_terms.len() != spec_node.params.len() {
            return Err(Trap::formula(format!(
                "spec call expects {} args, got {}",
                spec_node.params.len(),
                arg_terms.len()
            )));
        }
        let mut arg_vals = Vec::with_capacity(arg_terms.len());
        for (t, p_ty) in arg_terms.iter().zip(&spec_node.params) {
            let fv = self.eval(t)?;
            arg_vals.push(fv.to_value(p_ty)?);
        }
        if !holds_with_depth(
            &spec_node,
            Clause::Pre,
            &arg_vals,
            &[],
            self.callee,
            self.depth + 1,
        )? {
            return Err(Trap::formula(format!(
                "spec:{} called outside its PRE",
                vg_core::ir::hex(&h)
            )));
        }
        let mut pure_host = NoEffectsHost;
        let res = eval::run_inner_with_depth(
            &spec_node,
            &arg_vals,
            self.callee,
            &mut pure_host,
            &mut None,
            self.depth + 1,
        )?;
        pack_call_results(&spec_node.results, res)
    }

    fn eval_apply(&mut self, call_idx: u32, arg_terms: &[Term]) -> Result<FVal, Trap> {
        let h = self
            .node
            .calls
            .get(call_idx as usize)
            .copied()
            .ok_or_else(|| Trap::formula(format!("apply call index {call_idx} out of range")))?;
        let k_node = (self.callee)(&h).ok_or_else(|| {
            Trap::formula(format!("missing kernel callee {}", vg_core::ir::hex(&h)))
        })?;
        if k_node.spec || k_node.effects.0 != 0 {
            return Err(Trap::formula(
                "formula application requires a pure NODE kernel",
            ));
        }
        if arg_terms.len() != k_node.params.len() {
            return Err(Trap::formula(format!(
                "kernel apply expects {} args, got {}",
                k_node.params.len(),
                arg_terms.len()
            )));
        }
        let mut arg_vals = Vec::with_capacity(arg_terms.len());
        for (t, p_ty) in arg_terms.iter().zip(&k_node.params) {
            let fv = self.eval(t)?;
            arg_vals.push(fv.to_value(p_ty)?);
        }
        if !holds_with_depth(
            &k_node,
            Clause::Pre,
            &arg_vals,
            &[],
            self.callee,
            self.depth + 1,
        )? {
            return Err(Trap::formula(format!(
                "formula kernel {} called outside its PRE",
                vg_core::ir::hex(&h)
            )));
        }
        let mut pure_host = NoEffectsHost;
        let res = eval::run_inner_with_depth(
            &k_node,
            &arg_vals,
            self.callee,
            &mut pure_host,
            &mut None,
            self.depth + 1,
        )?;
        pack_call_results(&k_node.results, res)
    }

    fn eval_fold(&mut self, call_idx: u32, arg_terms: &[Term]) -> Result<FVal, Trap> {
        let h = self
            .node
            .calls
            .get(call_idx as usize)
            .copied()
            .ok_or_else(|| Trap::formula(format!("fold call index {call_idx} out of range")))?;
        let k_node = (self.callee)(&h).ok_or_else(|| {
            Trap::formula(format!("missing fold kernel {}", vg_core::ir::hex(&h)))
        })?;
        if k_node.spec
            || k_node.effects.0 != 0
            || k_node.params.len() < 2
            || k_node.results.len() != 1
            || k_node.params[0] != k_node.results[0]
            || arg_terms.len() != k_node.params.len() + 1
        {
            return Err(Trap::formula(
                "fold: requires a pure kernel (A, T, B...) -> (A) and (init, xs, k, b...)",
            ));
        }

        let init_val = self.eval(&arg_terms[0])?.to_value(&k_node.params[0])?;
        let xs_fval = self.eval(&arg_terms[1])?;
        let xs = match xs_fval {
            FVal::Val(Type::Array(ref e, _) | Type::Slice(ref e), Value::Seq(elems))
                if **e == k_node.params[1] =>
            {
                elems
            }
            other => {
                return Err(Trap::formula(format!(
                    "fold: collection {other:?} does not match element type {}",
                    k_node.params[1]
                )));
            }
        };
        let k_big = self.eval_int(&arg_terms[2])?;
        let k = k_big.to_usize().filter(|&u| u <= xs.len()).ok_or_else(|| {
            Trap::formula(format!(
                "fold: count k = {k_big} out of bounds [0, {}]",
                xs.len()
            ))
        })?;
        let mut bcasts = Vec::with_capacity(arg_terms.len() - 3);
        for (t, p_ty) in arg_terms[3..].iter().zip(&k_node.params[2..]) {
            bcasts.push(self.eval(t)?.to_value(p_ty)?);
        }

        let mut acc = init_val;
        let mut pure_host = NoEffectsHost;
        for (step, elem) in xs.into_iter().take(k).enumerate() {
            let mut args = Vec::with_capacity(2 + bcasts.len());
            args.push(acc);
            args.push(elem);
            args.extend(bcasts.iter().cloned());
            if !holds_with_depth(
                &k_node,
                Clause::Pre,
                &args,
                &[],
                self.callee,
                self.depth + 1,
            )? {
                return Err(Trap::formula(format!(
                    "fold: kernel PRE failed at step {step}"
                )));
            }
            let res = eval::run_inner_with_depth(
                &k_node,
                &args,
                self.callee,
                &mut pure_host,
                &mut None,
                self.depth + 1,
            )?;
            if res.len() != 1 {
                return Err(Trap::formula("fold: kernel did not return 1 result"));
            }
            acc = res.into_iter().next().expect("checked len == 1");
        }
        FVal::from_value(&k_node.results[0], &acc)
    }
}

fn pack_call_results(types: &[Type], values: Vec<Value>) -> Result<FVal, Trap> {
    if types.len() != values.len() {
        return Err(Trap::formula(format!(
            "call result arity mismatch: expected {}, got {}",
            types.len(),
            values.len()
        )));
    }
    if types.len() == 1 {
        FVal::from_value(&types[0], &values[0])
    } else {
        Ok(FVal::Val(Type::Tuple(types.to_vec()), Value::Tuple(values)))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use vg_core::asm_local;

    use super::*;

    fn assemble_map(src: &str) -> (Vec<vg_core::Assembled>, HashMap<Hash, Node>) {
        let asm = asm_local(src).expect("valid VG-Sym");
        let map = asm
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        (asm, map)
    }

    #[test]
    fn short_circuit_well_definedness_guards() {
        let src = r#"
module m;
NODE @guarded {
  SIGNATURE: (x: i32, y: i32, xs: Slice<u32>, r: Result<u8, u32>) -> (Bool);
  EFFECTS: [];
  CONTRACT {
    PRE: (y == 0 || (x / y) > 0)
      && (y != 0 ==> (x % y) >= -1000000)
      && (len(xs) > 0 && xs[0] < 10)
      && (is_ok(r) ==> ok(r) <= 255)
      && (!is_ok(r) ==> err(r) >= 0);
  }
  BODY {
    RETURN(true);
  }
}
"#;
        let (asm, map) = assemble_map(src);
        let node = &asm[0].node;
        let lookup = |h: &Hash| map.get(h).cloned();

        // y = 0, xs = [], r = Err(1) -> short-circuits safely without trapping on /0, xs[0], ok(r)!
        assert_eq!(
            holds(
                node,
                Clause::Pre,
                &[
                    Value::Int(10),
                    Value::Int(0),
                    Value::Seq(vec![]),
                    Value::Err(Box::new(Value::Int(1))),
                ],
                &[],
                &lookup
            ),
            Ok(false) // len(xs) > 0 is false, short-circuits before xs[0]
        );

        // y = 0, xs = [5], r = Err(1) -> true!
        assert_eq!(
            holds(
                node,
                Clause::Pre,
                &[
                    Value::Int(10),
                    Value::Int(0),
                    Value::Seq(vec![Value::Int(5)]),
                    Value::Err(Box::new(Value::Int(1))),
                ],
                &[],
                &lookup
            ),
            Ok(true)
        );

        // Unguarded division by zero in formula still traps.
        let unguarded = r#"
module m;
NODE @bad {
  SIGNATURE: (x: i32, y: i32) -> (Bool);
  EFFECTS: [];
  CONTRACT {
    PRE: x / y > 0;
  }
  BODY { RETURN(true); }
}
"#;
        let (uasm, umap) = assemble_map(unguarded);
        assert!(
            holds(
                &uasm[0].node,
                Clause::Pre,
                &[Value::Int(1), Value::Int(0)],
                &[],
                &|h| umap.get(h).cloned()
            )
            .is_err()
        );
    }

    #[test]
    fn mathematical_integers_never_wrap_in_formulas() {
        let src = r#"
module m;
NODE @math {
  SIGNATURE: (a: u64, b: u64) -> (Bool);
  EFFECTS: [];
  CONTRACT {
    PRE: a + 1 > a && a * b >= a;
  }
  BODY { RETURN(true); }
}
"#;
        let (asm, map) = assemble_map(src);
        let max = i128::from(u64::MAX);
        assert_eq!(
            holds(
                &asm[0].node,
                Clause::Pre,
                &[Value::Int(max), Value::Int(max)],
                &[],
                &|h| map.get(h).cloned()
            ),
            Ok(true)
        );
    }

    #[test]
    fn same_nan_signed_zero_and_as_bytes() {
        let src = r#"
module m;
NODE @f {
  SIGNATURE: (x: f32, y: f32) -> (f32);
  EFFECTS: [];
  CONTRACT {
    PRE: !is_nan(x) && !is_nan(y) && x == y;
    POST: same(result, x) && as_bytes(result) == as_bytes(x);
  }
  BODY { RETURN(x); }
}
"#;
        let (asm, map) = assemble_map(src);
        let node = &asm[0].node;
        let lookup = |h: &Hash| map.get(h).cloned();
        let pz = Value::Float(0);
        let nz = Value::Float(0x8000_0000);
        // -0.0 == +0.0 is true in PRE, but same(-0.0, +0.0) is false!
        assert_eq!(
            holds(node, Clause::Pre, &[pz.clone(), nz.clone()], &[], &lookup),
            Ok(true)
        );
        assert_eq!(
            holds(
                node,
                Clause::Post,
                &[nz.clone(), pz.clone()],
                &[pz],
                &lookup
            ),
            Ok(false)
        );
        assert_eq!(
            holds(
                node,
                Clause::Post,
                &[nz.clone(), nz.clone()],
                &[nz],
                &lookup
            ),
            Ok(true)
        );
    }

    #[test]
    fn tensor_forall_spec_and_fold_in_formulas() {
        // Test spec: from a48 and fold: from a34.
        let src = r#"
module m;
SPEC @spec_double {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT { PRE: x < 1000; }
  BODY {
    %0 = MUL.checked(x, 2:u32);
    RETURN(%0);
  }
}
NODE @step {
  SIGNATURE: (acc: u32, x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT { PRE: x < 100; }
  BODY {
    %0 = ADD.wrap(acc, x);
    RETURN(%0);
  }
}
NODE @sum {
  SIGNATURE: (t: Tensor<u32, [2, 2]>, xs: Array<u32, 3>, init: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE: forall(i in 0..2, j in 0..2) { t[i, j] < 500 }
      && forall(i in 0..3) { xs[i] < 100 };
    INVARIANT @inv(k, acc): acc == fold:step(init, xs, k) && acc <= %0;
    POST: result == fold:step(init, xs, 3) && spec:spec_double(t[1, 0]) == t[1, 0] * 2;
  }
  BODY {
    %0 = ADD.wrap(init, 1000:u32);
    %1 = FOLD_BOUNDED(@step, xs, init=init, invariant=@inv);
    RETURN(%1);
  }
}
"#;
        let (asm, map) = assemble_map(src);
        let node = &asm[2].node;
        let lookup = |h: &Hash| map.get(h).cloned();
        let t = Value::Seq(vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30),
            Value::Int(40),
        ]);
        let xs = Value::Seq(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let init = Value::Int(5);
        let inputs = vec![t, xs, init];
        assert_eq!(holds(node, Clause::Pre, &inputs, &[], &lookup), Ok(true));
        let mut host = NoEffectsHost;
        let res = eval::run(node, &inputs, &lookup, &mut host).unwrap();
        assert_eq!(res, vec![Value::Int(11)]);
        assert_eq!(holds(node, Clause::Post, &inputs, &res, &lookup), Ok(true));

        // Check INVARIANT with Term::Port (%0 = 1005) and fold: at steps 0..=3.
        let ports = vec![vec![Value::Int(1005)]];
        assert_eq!(
            holds_invariant(node, 0, 0, &Value::Int(5), &inputs, &ports, &lookup),
            Ok(true)
        );
        assert_eq!(
            holds_invariant(node, 0, 2, &Value::Int(8), &inputs, &ports, &lookup),
            Ok(true)
        );
        assert_eq!(
            holds_invariant(node, 0, 3, &Value::Int(11), &inputs, &ports, &lookup),
            Ok(true)
        );
        assert_eq!(
            holds_invariant(node, 0, 3, &Value::Int(12), &inputs, &ports, &lookup),
            Ok(false)
        );
    }

    #[test]
    fn callee_pre_violation_traps_in_eval() {
        let src = r#"
module m;
NODE @nonzero_div {
  SIGNATURE: (a: i32, b: i32) -> (i32);
  EFFECTS: [];
  CONTRACT { PRE: b != 0; }
  BODY {
    %0 = DIV.checked(a, b);
    RETURN(%0);
  }
}
NODE @caller {
  SIGNATURE: (a: i32, b: i32) -> (i32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = CALL_HASH(@nonzero_div, a, b);
    RETURN(%0);
  }
}
"#;
        let (asm, map) = assemble_map(src);
        let caller = &asm[1].node;
        let lookup = |h: &Hash| map.get(h).cloned();
        let mut host = NoEffectsHost;
        let trap =
            eval::run(caller, &[Value::Int(10), Value::Int(0)], &lookup, &mut host).unwrap_err();
        assert_eq!(trap.op, Some(0));
        assert_eq!(trap.opcode, Some("CALL_HASH"));
        assert!(trap.message.contains("CALL.pre"));
    }
}
