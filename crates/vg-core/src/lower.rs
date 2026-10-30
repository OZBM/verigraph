//! Lowering: a VG-Sym node to a VG-IR node, the front half of `vg asm`. It applies the structural
//! rules of docs/15 (VG-S codes: names, labels, arities, modes), computes the result types every op
//! carries (docs/14: typing is local, so they follow from the operands), types the numbers of
//! formulas (docs/03, "Formula types") and checks that Eff is linear, which keeps two effects from
//! ever hashing alike. Everything else about types and effects is vg-types' job.

use std::collections::HashMap;

use crate::ast::{self, ArgVal, ClauseKind, FKind, Literal, Stmt, TypeExpr, Value};
use crate::diag::{Diagnostic, Span};
use crate::float;
use crate::ir::{
    Bin, Builtin, Const, Contract, Hash, Learnable, Node, Op, Operand, Resource, Term,
};
use crate::opcode::{self, Modes, OpInfo, Shape};
use crate::types::{EffectMask, Type};

type R<T> = Result<T, Diagnostic>;

/// Deepest formula a node may hold; deeper ones are rejected so no pass can exhaust the stack.
pub const MAX_TERM_DEPTH: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Node,
    Spec,
    Query,
}

/// What a label names, as callers see it.
#[derive(Clone, Debug)]
pub struct Callable {
    pub kind: Kind,
    /// Index of the defining item in the module, for define-before-use.
    pub item: usize,
    pub params: Vec<Type>,
    pub results: Vec<Type>,
    pub effects: EffectMask,
    /// The node_hash of a NODE, the spec_hash of a SPEC, or a @MESH_QUERY's vg.lock resolution.
    pub hash: Option<Hash>,
    /// The spec_hash a `spec:` call reaches: a SPEC's own, or a @MESH_QUERY's from vg.lock.
    pub spec: Option<Hash>,
}

pub type Labels = HashMap<String, Callable>;

fn err(code: &'static str, span: Span, msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error(code, span, msg)
}

/// A type written in a signature, with its refinement when it has one at the top.
pub fn lower_type(t: &TypeExpr, span: Span) -> R<Type> {
    lower_type_depth(t, span, 0)
}

fn lower_type_depth(t: &TypeExpr, span: Span, depth: usize) -> R<Type> {
    if depth > 64 {
        return Err(err("VG-T0118", span, "type nested deeper than 64 levels"));
    }
    let sub = |t: &TypeExpr| lower_type_depth(t, span, depth + 1).map(Box::new);
    let dim = |d: &ast::Dim| -> R<u32> {
        match d {
            ast::Dim::Int(s) => s
                .parse::<u32>()
                .map_err(|_| err("VG-T0118", span, format!("dimension {s} is above 2^32 - 1"))),
            ast::Dim::Ident(s) => Err(err(
                "VG-S0104",
                span,
                format!("symbolic dimension `{s}` is [ALPHA]; the MVP needs a number"),
            )),
        }
    };
    Ok(match t {
        TypeExpr::Scalar(s) => Type::scalar(s).expect("the parser accepts only scalar keywords"),
        TypeExpr::Bool => Type::Bool,
        TypeExpr::Eff => Type::Eff,
        TypeExpr::Handle(h) => Type::Handle(*h),
        TypeExpr::Array(e, s) => {
            if s.dims.len() != 1 {
                return Err(err("VG-T0101", span, "an Array has exactly one length"));
            }
            Type::Array(sub(e)?, dim(&s.dims[0])?)
        }
        TypeExpr::Tensor(e, s) => Type::Tensor(sub(e)?, s.dims.iter().map(dim).collect::<R<_>>()?),
        TypeExpr::Slice(e) => Type::Slice(sub(e)?),
        TypeExpr::Tuple(ms) => Type::Tuple(
            ms.iter()
                .map(|m| lower_type_depth(m, span, depth + 1))
                .collect::<R<_>>()?,
        ),
        TypeExpr::Result(a, b) => Type::Result(sub(a)?, sub(b)?),
        TypeExpr::Arena(n) => Type::Arena(
            n.parse()
                .map_err(|_| err("VG-T0118", span, "arena size above 2^32 - 1"))?,
        ),
        TypeExpr::Refine(..) => {
            return Err(err(
                "VG-T0116",
                span,
                "a refinement type is allowed only at the top of a SIGNATURE",
            ));
        }
    })
}

/// The bytes of an integer constant of type t, or a message when the literal does not fit.
fn int_bytes(n: &ast::SNum, t: &Type) -> Result<Vec<u8>, String> {
    if n.text.contains(['.', 'e', 'E']) {
        return Err(format!(
            "{} is a decimal; type {t} needs an integer",
            n.text
        ));
    }
    let mag: i128 = n
        .text
        .parse::<u64>()
        .map(i128::from)
        .map_err(|_| format!("{} does not fit {t}", n.text))?;
    let v = if n.neg { -mag } else { mag };
    let (bits, signed) = match t {
        Type::UInt(b) => (*b, false),
        Type::SInt(b) => (*b, true),
        _ => unreachable!("int_bytes is called for integer types"),
    };
    let (lo, hi) = if signed {
        (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
    } else {
        (0, (1i128 << bits) - 1)
    };
    if v < lo || v > hi {
        let sign = if n.neg { "-" } else { "" };
        return Err(format!("{sign}{} does not fit {t}", n.text));
    }
    Ok(v.to_le_bytes()[..usize::from(bits / 8)].to_vec())
}

fn float_bytes(n: &ast::SNum, t: &Type) -> Option<Vec<u8>> {
    let bits = float::parse(&n.text, n.neg, t)?;
    let size = t.byte_size().expect("floats have a size") as usize;
    Some(bits.to_le_bytes()[..size].to_vec())
}

/// A typed scalar literal as a constant.
fn scalar_const(n: &ast::SNum, t: &Type, span: Span) -> R<Const> {
    let bytes = if t.is_float() {
        float_bytes(n, t).ok_or_else(|| {
            err(
                "VG-P0006",
                span,
                format!("{}{} overflows {t}", if n.neg { "-" } else { "" }, n.text),
            )
        })?
    } else {
        int_bytes(n, t).map_err(|m| err("VG-P0003", span, m))?
    };
    Ok(Const {
        ty: t.clone(),
        bytes,
    })
}

/// The shortest two's-complement bytes of an Int constant, little-endian, at least one.
pub fn int_const(v: i128) -> Const {
    let all = v.to_le_bytes();
    let mut n = 16;
    while n > 1 {
        let top = all[n - 1];
        let below_sign = all[n - 2] & 0x80;
        if (top == 0 && below_sign == 0) || (top == 0xFF && below_sign != 0) {
            n -= 1;
        } else {
            break;
        }
    }
    Const {
        ty: Type::Int,
        bytes: all[..n].to_vec(),
    }
}

/// The value of an Int constant's bytes.
pub fn int_value(bytes: &[u8]) -> Option<i128> {
    if bytes.is_empty() || bytes.len() > 16 {
        return None;
    }
    let fill = if bytes[bytes.len() - 1] & 0x80 != 0 {
        0xFF
    } else {
        0
    };
    let mut all = [fill; 16];
    all[..bytes.len()].copy_from_slice(bytes);
    Some(i128::from_le_bytes(all))
}

fn array_const(elems: &[ast::Elem], t: &Type, span: Span) -> R<Const> {
    fn walk(es: &[ast::Elem], t: &Type, out: &mut Vec<u8>, span: Span) -> R<()> {
        let (elem, dims): (&Type, &[u32]) = match t {
            Type::Array(e, n) => (e, std::slice::from_ref(n)),
            Type::Tensor(e, d) => (e, d),
            _ => unreachable!("checked by the caller"),
        };
        rows(es, elem, dims, out, span)
    }
    fn rows(es: &[ast::Elem], elem: &Type, dims: &[u32], out: &mut Vec<u8>, span: Span) -> R<()> {
        if es.len() != dims[0] as usize {
            return Err(err(
                "VG-T0101",
                span,
                format!(
                    "array literal has {} elements where its type has {}",
                    es.len(),
                    dims[0]
                ),
            ));
        }
        for e in es {
            match (e, &dims[1..]) {
                (ast::Elem::Array(inner), [])
                    if matches!(elem, Type::Array(..) | Type::Tensor(..)) =>
                {
                    walk(inner, elem, out, span)?
                }
                (ast::Elem::Array(inner), rest) if !rest.is_empty() => {
                    rows(inner, elem, rest, out, span)?
                }
                (ast::Elem::Num(n), []) if elem.is_int() || elem.is_float() => {
                    out.extend(scalar_const(n, elem, span)?.bytes)
                }
                (ast::Elem::Bool(b), []) if *elem == Type::Bool => out.push(u8::from(*b)),
                _ => {
                    return Err(err(
                        "VG-T0101",
                        span,
                        format!("array literal element does not fit its type {elem}"),
                    ));
                }
            }
        }
        Ok(())
    }
    if !matches!(t, Type::Array(..) | Type::Tensor(..)) || t.byte_size().is_none() {
        return Err(err(
            "VG-T0101",
            span,
            format!("an array literal's type must be an Array or Tensor of scalars, not {t}"),
        ));
    }
    let mut bytes = Vec::new();
    walk(elems, t, &mut bytes, span)?;
    Ok(Const {
        ty: t.clone(),
        bytes,
    })
}

pub fn literal_const(l: &Literal, span: Span) -> R<Const> {
    match l {
        Literal::Num(n, t) => scalar_const(n, &Type::scalar(t).expect("scalar keyword"), span),
        Literal::Bool(b) => Ok(Const {
            ty: Type::Bool,
            bytes: vec![u8::from(*b)],
        }),
        Literal::Array(es, t) => array_const(es, &lower_type(t, span)?, span),
    }
}

/// A formula's type while it is being typed: numbers wait for the float term they meet.
#[derive(Clone, Debug, PartialEq)]
enum FTy {
    Bool,
    Int,
    Float(Type),
    Val(Type),
    /// Untyped numbers only; true when one of them is a decimal.
    Num(bool),
}

fn fty(t: &Type) -> FTy {
    if t.is_int() || *t == Type::Int {
        FTy::Int
    } else if t.is_float() {
        FTy::Float(t.clone())
    } else if *t == Type::Bool {
        FTy::Bool
    } else {
        FTy::Val(t.clone())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Clause {
    Pre,
    Post,
    Cost,
    Budget,
    Decreases,
    Invariant,
}

/// Formula scope: the clause, the named values in reach, and the binders (innermost last).
struct Cx {
    clause: Clause,
    /// Parameters and θ visible here, by name.
    named: Vec<(String, Term, FTy)>,
    bound: Vec<(String, FTy)>,
}

impl Cx {
    fn lookup(&self, name: &str) -> Option<(Term, FTy)> {
        if let Some(pos) = self.bound.iter().rposition(|(n, _)| n == name) {
            let db = (self.bound.len() - 1 - pos) as u32;
            return Some((Term::Bound(db), self.bound[pos].1.clone()));
        }
        if matches!(self.clause, Clause::Cost | Clause::Budget)
            && let Some(r) = Resource::ALL.iter().find(|r| r.name() == name)
        {
            return Some((Term::Resource(*r), FTy::Int));
        }
        self.named
            .iter()
            .rev()
            .find(|(n, ..)| n == name)
            .map(|(_, t, f)| (t.clone(), f.clone()))
    }
}

/// A lowered node, before canonicalization, with the first port of each op as written.
pub struct Lowered {
    pub node: Node,
    pub warnings: Vec<Diagnostic>,
    pub op_ports: Vec<String>,
}

pub struct NodeLowering<'a> {
    labels: &'a Labels,
    item: usize,
    label: &'a str,
    def: &'a ast::NodeDef,
    params: Vec<Type>,
    results: Vec<Type>,
    learnable: Vec<Learnable>,
    /// Parameters, then θ: (name, type).
    inputs: Vec<(String, Type)>,
    consts: Vec<Const>,
    const_ix: HashMap<Const, u32>,
    calls: Vec<Hash>,
    call_ix: HashMap<Hash, u32>,
    ops: Vec<Op>,
    ports: HashMap<String, Operand>,
    invariant_ix: HashMap<String, u32>,
    warnings: Vec<Diagnostic>,
    op_ports: Vec<String>,
}

impl<'a> NodeLowering<'a> {
    pub fn new(labels: &'a Labels, item: usize, def: &'a ast::NodeDef) -> Self {
        NodeLowering {
            labels,
            item,
            label: &def.label,
            def,
            params: Vec::new(),
            results: Vec::new(),
            learnable: Vec::new(),
            inputs: Vec::new(),
            consts: Vec::new(),
            const_ix: HashMap::new(),
            calls: Vec::new(),
            call_ix: HashMap::new(),
            ops: Vec::new(),
            ports: HashMap::new(),
            invariant_ix: HashMap::new(),
            warnings: Vec::new(),
            op_ports: Vec::new(),
        }
    }

    fn here(&self, d: Diagnostic) -> Diagnostic {
        d.at_node(&format!("@{}", self.label))
    }

    fn konst(&mut self, c: Const) -> u32 {
        if let Some(&i) = self.const_ix.get(&c) {
            return i;
        }
        let i = self.consts.len() as u32;
        self.const_ix.insert(c.clone(), i);
        self.consts.push(c);
        i
    }

    fn call(&mut self, h: Hash) -> u32 {
        if let Some(&i) = self.call_ix.get(&h) {
            return i;
        }
        let i = self.calls.len() as u32;
        self.call_ix.insert(h, i);
        self.calls.push(h);
        i
    }

    /// A label used in this node: it must name an item defined before this one (ADR 0005).
    fn callable(&self, name: &ast::Name) -> R<&'a Callable> {
        match self.labels.get(&name.text) {
            Some(c) if c.item < self.item => Ok(c),
            Some(c) if c.item == self.item => Err(err(
                "VG-S0007",
                name.span,
                format!(
                    "@{} calls itself; recursion (REC_CALL) is [ALPHA]",
                    name.text
                ),
            )),
            Some(_) => Err(err(
                "VG-S0004",
                name.span,
                format!(
                    "@{} is used before its definition; define it earlier in the module",
                    name.text
                ),
            )),
            None => Err(err(
                "VG-S0005",
                name.span,
                format!("unknown label @{}", name.text),
            )),
        }
    }

    fn resolved(&self, c: &Callable, name: &ast::Name) -> R<Hash> {
        c.hash.ok_or_else(|| {
            err(
                "VG-S0009",
                name.span,
                format!(
                    "@{} is a @MESH_QUERY with no resolution in vg.lock; resolve it before assembling",
                    name.text
                ),
            )
        })
    }

    pub fn lower(mut self) -> R<Lowered> {
        self.lower_inner().map_err(|e| self.here(e))
    }

    fn lower_inner(&mut self) -> R<Lowered> {
        let def = self.def;
        let mut refinements: Vec<(usize, bool, &ast::Name, &ast::Formula)> = Vec::new();
        for (i, p) in def.params.iter().enumerate() {
            self.declare(&p.name)?;
            let t = match &p.ty {
                TypeExpr::Refine(v, base, f) => {
                    refinements.push((i, false, v, f));
                    lower_type(base, p.name.span)?
                }
                t => lower_type(t, p.name.span)?,
            };
            self.params.push(t.clone());
            self.inputs.push((p.name.text.clone(), t));
        }
        for (j, t) in def.results.iter().enumerate() {
            let t = match t {
                TypeExpr::Refine(v, base, f) => {
                    refinements.push((j, true, v, f));
                    lower_type(base, def.span)?
                }
                t => lower_type(t, def.span)?,
            };
            self.results.push(t);
        }
        for l in def.learnable.iter().flatten() {
            self.declare(&l.name)?;
            let ty = Type::scalar(l.ty).expect("float keyword");
            let bound = |n: &ast::SNum, me: &mut Self| -> R<u32> {
                let c = scalar_const(n, &ty, l.name.span)?;
                Ok(me.konst(c))
            };
            let lo = bound(&l.lo, self)?;
            let hi = bound(&l.hi, self)?;
            let (lov, hiv) = (self.float_of(lo), self.float_of(hi));
            if lov > hiv {
                return Err(err(
                    "VG-T0117",
                    l.name.span,
                    format!("LEARNABLE {}: lower bound above upper bound", l.name.text),
                ));
            }
            let loss = match &l.loss {
                Some(name) => {
                    let c = self.callable(name)?;
                    if c.kind != Kind::Node {
                        return Err(err("VG-S0109", name.span, "a loss must be a NODE"));
                    }
                    let h = self.resolved(c, name)?;
                    Some(self.call(h))
                }
                None => None,
            };
            self.learnable.push(Learnable {
                ty: ty.clone(),
                lo,
                hi,
                loss,
            });
            self.inputs.push((l.name.text.clone(), ty));
        }
        let mut effects = 0u32;
        for e in &def.effects {
            effects |= EffectMask::bit(&e.text).expect("the parser accepts only effect keywords");
        }
        // INVARIANT labels are local to the node.
        let mut n_inv = 0u32;
        for c in &def.clauses {
            if let ClauseKind::Invariant { label, k, acc } = &c.kind {
                if self
                    .invariant_ix
                    .insert(label.text.clone(), n_inv)
                    .is_some()
                {
                    return Err(err(
                        "VG-S0006",
                        label.span,
                        format!("INVARIANT @{} is defined twice", label.text),
                    ));
                }
                if k.text == acc.text {
                    return Err(err(
                        "VG-S0001",
                        acc.span,
                        format!("{} is bound twice", acc.text),
                    ));
                }
                n_inv += 1;
            }
        }
        let outputs = self.body()?;
        self.check_eff_linear(&outputs)?;
        let contract = self.contract(&refinements)?;
        let node = Node {
            spec: def.spec,
            effects: EffectMask(effects),
            params: std::mem::take(&mut self.params),
            results: std::mem::take(&mut self.results),
            learnable: std::mem::take(&mut self.learnable),
            consts: std::mem::take(&mut self.consts),
            calls: std::mem::take(&mut self.calls),
            ops: std::mem::take(&mut self.ops),
            outputs,
            contract,
            debug: None,
        };
        Ok(Lowered {
            node,
            warnings: std::mem::take(&mut self.warnings),
            op_ports: std::mem::take(&mut self.op_ports),
        })
    }

    fn float_of(&self, c: u32) -> f64 {
        let c = &self.consts[c as usize];
        let mut b = [0u8; 8];
        b[..c.bytes.len()].copy_from_slice(&c.bytes);
        float::to_f64(&c.ty, u64::from_le_bytes(b))
    }

    fn declare(&self, name: &ast::Name) -> R<()> {
        if self.inputs.iter().any(|(n, _)| *n == name.text) {
            return Err(err(
                "VG-S0001",
                name.span,
                format!("{} is declared twice", name.text),
            ));
        }
        Ok(())
    }

    fn value(&mut self, v: &Value, later: &HashMap<&str, ()>) -> R<Operand> {
        match v {
            Value::Port(p) => match self.ports.get(&p.text) {
                Some(o) => Ok(*o),
                None if later.contains_key(p.text.as_str()) => Err(err(
                    "VG-S0002",
                    p.span,
                    format!("%{} is used before the statement that defines it", p.text),
                )),
                None => Err(err("VG-S0003", p.span, format!("unknown port %{}", p.text))),
            },
            Value::Ident(n) => match self.inputs.iter().position(|(m, _)| *m == n.text) {
                Some(i) => Ok(Operand::Input(i as u32)),
                None => Err(err(
                    "VG-S0003",
                    n.span,
                    format!("unknown parameter {}", n.text),
                )),
            },
            Value::Lit(l, span) => {
                let c = literal_const(l, *span)?;
                Ok(Operand::Const(self.konst(c)))
            }
        }
    }

    fn vty(&self, o: Operand) -> Type {
        match o {
            Operand::Input(i) => self.inputs[i as usize].1.clone(),
            Operand::Op(i, s) => self.ops[i as usize].results[s as usize].clone(),
            Operand::Const(c) => self.consts[c as usize].ty.clone(),
            Operand::Callee(_) | Operand::Invariant(_) => unreachable!("not a value"),
        }
    }

    fn body(&mut self) -> R<Vec<Operand>> {
        let def = self.def;
        let mut later: HashMap<&str, ()> = HashMap::new();
        for s in &def.body {
            if let Stmt::Assign { binds, .. } = s {
                for b in binds {
                    later.insert(&b.port.text, ());
                }
            }
        }
        let last = def.body.len() - 1;
        for (si, s) in def.body.iter().enumerate() {
            match s {
                Stmt::Return { values, span } => {
                    if si != last {
                        return Err(err("VG-S0008", *span, "RETURN must be the last statement"));
                    }
                    if values.len() != self.results.len() {
                        return Err(err(
                            "VG-T0108",
                            *span,
                            format!(
                                "RETURN has {} values; the SIGNATURE has {} results",
                                values.len(),
                                self.results.len()
                            ),
                        ));
                    }
                    return values.iter().map(|v| self.value(v, &later)).collect();
                }
                Stmt::Assign {
                    binds,
                    op,
                    mode,
                    args,
                    span,
                } => {
                    let first = format!("%{}", binds[0].port.text);
                    self.stmt(binds, op, mode.as_ref(), args, *span, &later)
                        .map_err(|mut e| {
                            e.op.get_or_insert(first);
                            e
                        })?;
                }
            }
        }
        let span = match &def.body[last] {
            Stmt::Assign { span, .. } | Stmt::Return { span, .. } => *span,
        };
        Err(err("VG-S0008", span, "BODY must end with RETURN"))
    }

    fn stmt(
        &mut self,
        binds: &[ast::Bind],
        opname: &ast::Name,
        mode: Option<&ast::Name>,
        args: &[ast::Arg],
        span: Span,
        later: &HashMap<&str, ()>,
    ) -> R<()> {
        let info: &OpInfo = opcode::by_name(&opname.text).ok_or_else(|| {
            err(
                "VG-S0101",
                opname.span,
                format!("unknown opcode {}", opname.text),
            )
        })?;
        if !info.mvp {
            return Err(err(
                "VG-S0104",
                opname.span,
                format!("{} is not available in the MVP", info.name),
            ));
        }
        let mode_text = mode.map(|m| m.text.as_str());
        let mode_byte = info.mode_byte(mode_text).ok_or_else(|| {
            let what = match mode_text {
                Some(m) => format!("{} takes no mode .{m}", info.name),
                None => format!("{} needs a mode", info.name),
            };
            err("VG-S0102", mode.map_or(opname.span, |m| m.span), what)
        })?;
        let cast_target = if info.modes == Modes::Cast {
            Some(Type::scalar(mode_text.unwrap_or("")).ok_or_else(|| {
                err(
                    "VG-S0102",
                    opname.span,
                    "CAST names its target scalar type: CAST.u32(x)",
                )
            })?)
        } else {
            None
        };
        // Positional arguments first, then named ones (VG-S0105).
        let mut positional = Vec::new();
        let mut named: Vec<(&str, &ast::Arg)> = Vec::new();
        for a in args {
            match &a.name {
                None if !named.is_empty() => {
                    return Err(err(
                        "VG-S0105",
                        a.span,
                        "a positional argument follows a named one",
                    ));
                }
                None => positional.push(a),
                Some(n) => {
                    let allowed: &[&str] = match info.shape {
                        Shape::Fold => &["init", "invariant"],
                        Shape::Guard => &["else"],
                        _ => &[],
                    };
                    if !allowed.contains(&n.text.as_str())
                        || named.iter().any(|(m, _)| *m == n.text)
                    {
                        return Err(err(
                            "VG-S0105",
                            n.span,
                            format!("{} takes no argument {}= here", info.name, n.text),
                        ));
                    }
                    named.push((&n.text, a));
                }
            }
        }
        let arity_err = |what: &str| err("VG-S0103", span, format!("{} takes {what}", info.name));
        let (min, exact) = match info.shape {
            Shape::Fixed(n) => (n as usize, true),
            Shape::AtLeast(n) => (n as usize, false),
            Shape::Kernel | Shape::Fold | Shape::Guard => (2, false),
            Shape::Call => (1, false),
        };
        if positional.len() < min || (exact && positional.len() != min) {
            return Err(arity_err(&format!(
                "{}{} arguments, not {}",
                if exact { "" } else { "at least " },
                min,
                positional.len()
            )));
        }
        let get = |k: &str| named.iter().find(|(m, _)| *m == k).map(|(_, a)| *a);
        if info.shape == Shape::Fold && get("invariant").is_none() {
            return Err(err(
                "VG-S0107",
                span,
                "FOLD_BOUNDED needs invariant=@I naming an INVARIANT",
            ));
        }
        if info.shape == Shape::Fold && get("init").is_none() {
            return Err(arity_err("init= for the initial accumulator"));
        }
        if info.shape == Shape::Guard && get("else").is_none() {
            return Err(arity_err("else=@g for the other branch"));
        }
        // Operands in the order docs/02 fixes, with each label slot noted.
        let label_slot = |i: usize| match info.shape {
            Shape::Kernel | Shape::Fold | Shape::Call => i == 0,
            Shape::Guard => i == 1,
            _ => false,
        };
        let mut ordered: Vec<(&ast::Arg, bool)> = positional
            .iter()
            .enumerate()
            .map(|(i, a)| (*a, label_slot(i)))
            .collect();
        match info.shape {
            Shape::Fold => {
                ordered.push((get("init").expect("checked"), false));
                ordered.push((get("invariant").expect("checked"), true));
            }
            Shape::Guard => ordered.push((get("else").expect("checked"), true)),
            _ => {}
        }
        let mut operands = Vec::new();
        let mut callees: Vec<&'a Callable> = Vec::new();
        for (a, want_label) in &ordered {
            match (&a.val, want_label) {
                (ArgVal::Label(l), true)
                    if a.name.as_ref().is_some_and(|n| n.text == "invariant") =>
                {
                    let ix = *self.invariant_ix.get(&l.text).ok_or_else(|| {
                        err(
                            "VG-S0107",
                            l.span,
                            format!("@{} names no INVARIANT of this node", l.text),
                        )
                    })?;
                    operands.push(Operand::Invariant(ix));
                }
                (ArgVal::Label(l), true) => {
                    let c = self.callable(l)?;
                    if c.kind == Kind::Spec {
                        return Err(err(
                            "VG-S0110",
                            l.span,
                            format!(
                                "@{} is a SPEC; call it from a formula as spec:{}(…)",
                                l.text, l.text
                            ),
                        ));
                    }
                    let h = self.resolved(c, l)?;
                    callees.push(c);
                    operands.push(Operand::Callee(self.call(h)));
                }
                (ArgVal::Value(_), true) => {
                    return Err(err(
                        "VG-S0109",
                        a.span,
                        "this argument must be a label such as @f",
                    ));
                }
                (ArgVal::Label(l), false) => {
                    return Err(err(
                        "VG-T0101",
                        l.span,
                        format!("@{} is a label where a value is expected", l.text),
                    ));
                }
                (ArgVal::Value(v), false) => operands.push(self.value(v, later)?),
            }
        }
        if info.code == opcode::IDENTITY {
            if binds.len() != 1 {
                return Err(err("VG-S0106", span, "IDENTITY has one result"));
            }
            let t = self.vty(operands[0]);
            self.bind(&binds[0], operands[0], &t)?;
            return Ok(());
        }
        let annotation = |i: usize| -> R<Option<Type>> {
            binds
                .get(i)
                .and_then(|b| b.ty.as_ref())
                .map(|t| lower_type(t, span))
                .transpose()
        };
        let results =
            self.result_types(info, cast_target, &operands, &callees, annotation(0)?, span)?;
        if results.len() != binds.len() {
            return Err(err(
                "VG-S0106",
                span,
                format!(
                    "{} has {} results; {} ports are bound",
                    info.name,
                    results.len(),
                    binds.len()
                ),
            ));
        }
        let index = self.ops.len() as u32;
        self.op_ports.push(binds[0].port.text.clone());
        self.ops.push(Op {
            opcode: info.code,
            mode: mode_byte,
            operands,
            results: results.clone(),
        });
        for (slot, (b, t)) in binds.iter().zip(&results).enumerate() {
            self.bind(b, Operand::Op(index, slot as u8), t)?;
        }
        Ok(())
    }

    fn bind(&mut self, b: &ast::Bind, o: Operand, t: &Type) -> R<()> {
        if let Some(ann) = &b.ty {
            let ann = lower_type(ann, b.port.span)?;
            if ann != *t {
                return Err(err(
                    "VG-T0115",
                    b.port.span,
                    format!("%{} is annotated {ann} but the op gives {t}", b.port.text),
                ));
            }
        }
        if self.ports.insert(b.port.text.clone(), o).is_some() {
            return Err(err(
                "VG-S0001",
                b.port.span,
                format!("%{} is assigned twice", b.port.text),
            ));
        }
        Ok(())
    }

    /// Result types per docs/14. Mismatches that do not change the result type are vg-types' to
    /// report; here only what the type of a result needs is checked.
    fn result_types(
        &self,
        info: &OpInfo,
        cast: Option<Type>,
        ops: &[Operand],
        callees: &[&Callable],
        annotation: Option<Type>,
        span: Span,
    ) -> R<Vec<Type>> {
        let bad = |m: String| Err(err("VG-T0101", span, m));
        let t0 = || self.vty(ops[0]);
        let one = |t: Type| Ok(vec![t]);
        let kernel_result = |c: &Callable| -> R<Type> {
            match c.results.as_slice() {
                [t] => Ok(t.clone()),
                _ => Err(err("VG-T0107", span, "a kernel has exactly one result")),
            }
        };
        match info.name {
            "TUPLE" => one(Type::Tuple(ops.iter().map(|o| self.vty(*o)).collect())),
            "PROJ" => {
                let Type::Tuple(ms) = t0() else {
                    return bad(format!("PROJ needs a Tuple, found {}", t0()));
                };
                let Operand::Const(c) = ops[1] else {
                    return Err(err("VG-S0108", span, "PROJ's index must be a u32 literal"));
                };
                let c = &self.consts[c as usize];
                if c.ty != Type::UInt(32) {
                    return bad(format!("PROJ's index must be a u32, found {}", c.ty));
                }
                let j = u32::from_le_bytes(c.bytes[..4].try_into().expect("u32 bytes")) as usize;
                match ms.get(j) {
                    Some(t) => one(t.clone()),
                    None => bad(format!("PROJ index {j} is out of range for {}", t0())),
                }
            }
            "SELECT" => one(self.vty(ops[1])),
            "CAST" => one(cast.expect("CAST has a target")),
            "AS_BYTES" => one(Type::Slice(Box::new(Type::UInt(8)))),
            "OK" | "ERR" => match annotation {
                Some(t @ Type::Result(..)) => one(t),
                Some(t) => Err(err(
                    "VG-T0115",
                    span,
                    format!("{} builds a Result, not {t}", info.name),
                )),
                None => Err(err(
                    "VG-T0115",
                    span,
                    format!(
                        "{} needs its port annotated: %r: Result<T, E> = {}(v);",
                        info.name, info.name
                    ),
                )),
            },
            "IS_OK" => one(Type::Bool),
            "GET_OK" | "GET_ERR" => match t0() {
                Type::Result(a, b) => one(if info.name == "GET_OK" { *a } else { *b }),
                t => bad(format!("{} needs a Result, found {t}", info.name)),
            },
            "EQ" | "NE" | "LT" | "LE" | "GT" | "GE" | "BAND" | "BOR" | "BNOT" => one(Type::Bool),
            "INDEX" => match t0() {
                Type::Array(e, _) | Type::Slice(e) => one(*e),
                t => bad(format!("INDEX needs an Array or Slice, found {t}")),
            },
            "UPDATE" => one(t0()),
            "LEN" => one(Type::UInt(64)),
            "SLICE" => match t0() {
                Type::Array(e, _) | Type::Slice(e) => one(Type::Slice(e)),
                t => bad(format!("SLICE needs an Array or Slice, found {t}")),
            },
            "CONCAT" => match (t0(), self.vty(ops[1])) {
                (Type::Array(e, n), Type::Array(_, m)) => match n.checked_add(m) {
                    Some(len) => one(Type::Array(e, len)),
                    None => Err(err("VG-T0118", span, "CONCAT result longer than 2^32 - 1")),
                },
                (Type::Array(e, _) | Type::Slice(e), _) => one(Type::Slice(e)),
                (t, _) => bad(format!("CONCAT needs Arrays or Slices, found {t}")),
            },
            "SIMD_MAP" => {
                let u = Box::new(kernel_result(callees[0])?);
                match self.vty(ops[1]) {
                    Type::Array(_, n) => one(Type::Array(u, n)),
                    Type::Tensor(_, d) => one(Type::Tensor(u, d)),
                    Type::Slice(_) => one(Type::Slice(u)),
                    t => bad(format!("SIMD_MAP maps an Array, Tensor or Slice, not {t}")),
                }
            }
            "FOLD_BOUNDED" => one(kernel_result(callees[0])?),
            "GENERATE" => {
                let u = Box::new(kernel_result(callees[0])?);
                match ops[1] {
                    Operand::Const(c) if self.consts[c as usize].ty == Type::UInt(64) => {
                        let b = &self.consts[c as usize].bytes;
                        let n = u64::from_le_bytes(b[..8].try_into().expect("u64 bytes"));
                        match u32::try_from(n) {
                            Ok(n) => one(Type::Array(u, n)),
                            Err(_) => Err(err("VG-T0118", span, "GENERATE count above 2^32 - 1")),
                        }
                    }
                    _ => one(Type::Slice(u)),
                }
            }
            "ASSERT_GUARD" | "CALL_HASH" => Ok(callees[0].results.clone()),
            "FILE_READ" => Ok(vec![
                Type::Eff,
                Type::Result(
                    Box::new(Type::Slice(Box::new(Type::UInt(8)))),
                    Box::new(Type::UInt(32)),
                ),
            ]),
            "FILE_WRITE" => Ok(vec![
                Type::Eff,
                Type::Result(Box::new(Type::UInt(64)), Box::new(Type::UInt(32))),
            ]),
            "CLOCK_READ" => Ok(vec![Type::Eff, Type::UInt(64)]),
            // Arithmetic, bits and floats: the operands' type.
            _ => one(t0()),
        }
    }

    /// Each Eff value is used exactly once (VG-E0102). Checked before canonicalization, so merging
    /// can never fold two effects into one.
    fn check_eff_linear(&self, outputs: &[Operand]) -> R<()> {
        let mut uses: HashMap<Operand, u32> = HashMap::new();
        for o in self.ops.iter().flat_map(|op| &op.operands).chain(outputs) {
            *uses.entry(*o).or_default() += 1;
        }
        let mut values: Vec<(Operand, String)> = self
            .inputs
            .iter()
            .enumerate()
            .filter(|(_, (_, t))| *t == Type::Eff)
            .map(|(i, (n, _))| (Operand::Input(i as u32), n.clone()))
            .collect();
        for (i, op) in self.ops.iter().enumerate() {
            for (s, t) in op.results.iter().enumerate() {
                if *t == Type::Eff {
                    values.push((
                        Operand::Op(i as u32, s as u8),
                        format!("result {s} of op {i}"),
                    ));
                }
            }
        }
        for (v, name) in values {
            let n = uses.get(&v).copied().unwrap_or(0);
            if n != 1 {
                return Err(err(
                    "VG-E0102",
                    self.def.span,
                    format!("Eff value {name} is used {n} times; each Eff is used exactly once"),
                ));
            }
        }
        Ok(())
    }

    fn contract(
        &mut self,
        refinements: &[(usize, bool, &ast::Name, &ast::Formula)],
    ) -> R<Contract> {
        let def = self.def;
        let named: Vec<(String, Term, FTy)> = self
            .inputs
            .iter()
            .enumerate()
            .map(|(i, (n, t))| (n.clone(), Term::Input(i as u32), fty(t)))
            .collect();
        let mut c = Contract::default();
        let mut invariants: Vec<Vec<Term>> = Vec::new();
        for cl in &def.clauses {
            let (clause, bound) = match &cl.kind {
                ClauseKind::Pre => (Clause::Pre, vec![]),
                ClauseKind::Post => (Clause::Post, vec![]),
                ClauseKind::Cost => (Clause::Cost, vec![]),
                ClauseKind::Budget => (Clause::Budget, vec![]),
                ClauseKind::Decreases => (Clause::Decreases, vec![]),
                ClauseKind::Invariant { label, k, acc } => {
                    let ix = self.invariant_ix[&label.text];
                    match self.fold_acc(ix)? {
                        Some(acc_t) => (
                            Clause::Invariant,
                            vec![(k.text.clone(), FTy::Int), (acc.text.clone(), fty(&acc_t))],
                        ),
                        None => {
                            self.warnings.push(
                                Diagnostic::warning(
                                    "VG-W0002",
                                    label.span,
                                    format!(
                                        "INVARIANT @{} is declared but no FOLD_BOUNDED uses it",
                                        label.text
                                    ),
                                )
                                .at_node(&format!("@{}", self.label)),
                            );
                            invariants.push(Vec::new());
                            continue;
                        }
                    }
                }
            };
            let mut cx = Cx {
                clause,
                named: named.clone(),
                bound,
            };
            let conj = self.clause_terms(&cl.formula, &mut cx)?;
            match clause {
                Clause::Pre => c.pre.extend(conj),
                Clause::Post => c.post.extend(conj),
                Clause::Cost => c.cost.extend(conj),
                Clause::Budget => c.budget.extend(conj),
                Clause::Decreases => c.decreases.extend(conj),
                Clause::Invariant => invariants.push(conj),
            }
        }
        // Refined parameters join PRE after it, refined results join POST, in signature order.
        for &(i, is_result, v, f) in refinements {
            let (clause, term, t) = if is_result {
                (
                    Clause::Post,
                    Term::Result(i as u32),
                    self.results[i].clone(),
                )
            } else {
                (Clause::Pre, Term::Input(i as u32), self.params[i].clone())
            };
            // A refined parameter sees the parameters before it; a refined result sees them all.
            let visible = if is_result { named.len() } else { i };
            let mut named_here: Vec<_> = named[..visible].to_vec();
            named_here.push((v.text.clone(), term, fty(&t)));
            let mut cx = Cx {
                clause,
                named: named_here,
                bound: vec![],
            };
            let conj = self.clause_terms(f, &mut cx)?;
            if is_result {
                c.post.extend(conj)
            } else {
                c.pre.extend(conj)
            }
        }
        c.invariants = invariants;
        Ok(c)
    }

    /// The accumulator type of the folds that use INVARIANT ix, or None when none does.
    fn fold_acc(&self, ix: u32) -> R<Option<Type>> {
        let mut acc: Option<Type> = None;
        for op in &self.ops {
            if op.opcode == opcode::FOLD_BOUNDED
                && op.operands.last() == Some(&Operand::Invariant(ix))
            {
                match &acc {
                    Some(t) if *t != op.results[0] => {
                        return Err(err(
                            "VG-C0101",
                            self.def.span,
                            "one INVARIANT is used by folds with different accumulator types",
                        ));
                    }
                    _ => acc = Some(op.results[0].clone()),
                }
            }
        }
        Ok(acc)
    }

    /// A clause's conjuncts: its top-level `&&` flattened, each one a Bool formula (an Int measure
    /// for DECREASES).
    fn clause_terms(&mut self, f: &ast::Formula, cx: &mut Cx) -> R<Vec<Term>> {
        let mut conj = Vec::new();
        let mut stack = vec![f];
        while let Some(g) = stack.pop() {
            if let FKind::Binary(ast::BinOp::And, a, b) = &g.kind {
                stack.push(b);
                stack.push(a);
                continue;
            }
            let want = if cx.clause == Clause::Decreases {
                FTy::Int
            } else {
                FTy::Bool
            };
            let t = settle(self.infer(g, cx)?, g.span)?;
            if t != want {
                return Err(err(
                    "VG-C0101",
                    g.span,
                    format!(
                        "a {} conjunct must be {}",
                        clause_name(cx.clause),
                        if want == FTy::Int {
                            "an integer"
                        } else {
                            "Bool"
                        }
                    ),
                ));
            }
            let term = self.build(g, cx, None)?;
            if depth(&term) > MAX_TERM_DEPTH {
                return Err(err(
                    "VG-P0001",
                    g.span,
                    "formula nested deeper than 256 levels",
                ));
            }
            conj.push(term);
        }
        Ok(conj)
    }

    fn port_term(&self, p: &ast::Name) -> R<(Term, FTy)> {
        let o = *self
            .ports
            .get(&p.text)
            .ok_or_else(|| err("VG-S0003", p.span, format!("unknown port %{}", p.text)))?;
        let t = fty(&self.vty(o));
        Ok((
            match o {
                Operand::Input(i) => Term::Input(i),
                Operand::Const(c) => Term::Const(c),
                Operand::Op(i, s) => Term::Port(i, s),
                Operand::Callee(_) | Operand::Invariant(_) => unreachable!("ports hold values"),
            },
            t,
        ))
    }

    /// A name or a value inside a formula.
    fn leaf(&mut self, v: &Value, cx: &Cx) -> R<(Term, FTy)> {
        match v {
            Value::Port(p) if cx.clause != Clause::Invariant => Err(err(
                "VG-C0105",
                p.span,
                format!("%{} is a port; only an INVARIANT may name ports", p.text),
            )),
            Value::Port(p) => self.port_term(p),
            Value::Ident(n) => cx.lookup(&n.text).ok_or_else(|| {
                if matches!(cx.clause, Clause::Cost | Clause::Budget) {
                    err(
                        "VG-C0106",
                        n.span,
                        format!("unknown name or resource {}", n.text),
                    )
                } else {
                    err("VG-S0003", n.span, format!("unknown name {}", n.text))
                }
            }),
            Value::Lit(l, span) => {
                let c = literal_const(l, *span)?;
                let t = fty(&c.ty);
                Ok((Term::Const(self.konst(c)), t))
            }
        }
    }

    fn result_term(&self, f: &ast::Formula, cx: &Cx) -> R<Option<(Term, FTy)>> {
        if cx.clause != Clause::Post {
            if matches!(f.kind, FKind::Result) {
                return Err(err("VG-C0104", f.span, "`result` is allowed only in POST"));
            }
            return Ok(None);
        }
        match &f.kind {
            FKind::Result if self.results.len() == 1 => {
                Ok(Some((Term::Result(0), fty(&self.results[0]))))
            }
            FKind::Result => Err(err(
                "VG-C0104",
                f.span,
                format!(
                    "the node has {} results: write result.0 … result.{}",
                    self.results.len(),
                    self.results.len() - 1
                ),
            )),
            FKind::Proj(b, j) if matches!(b.kind, FKind::Result) && self.results.len() > 1 => {
                let j: usize = j.parse().unwrap_or(usize::MAX);
                match self.results.get(j) {
                    Some(t) => Ok(Some((Term::Result(j as u32), fty(t)))),
                    None => Err(err(
                        "VG-C0104",
                        f.span,
                        format!("result.{j} is out of range"),
                    )),
                }
            }
            _ => Ok(None),
        }
    }

    /// A call's callee, its kind checked against the prefix.
    fn formula_callee(
        &mut self,
        prefix: Option<ast::CallPrefix>,
        name: &ast::Name,
    ) -> R<(u32, &'a Callable)> {
        let label = ast::Name {
            text: name.text.clone(),
            span: name.span,
        };
        let c = self.callable(&label).map_err(|e| {
            if e.code == "VG-S0005" {
                err(
                    "VG-C0103",
                    name.span,
                    format!("unknown function {}", name.text),
                )
            } else {
                e
            }
        })?;
        let h = match (prefix, c.kind) {
            (Some(ast::CallPrefix::Spec), Kind::Spec) => c.hash,
            (Some(ast::CallPrefix::Spec), Kind::Query) => c.spec,
            (Some(ast::CallPrefix::Spec), Kind::Node) => {
                return Err(err(
                    "VG-C0107",
                    name.span,
                    format!("spec:{} names a NODE, not a SPEC", name.text),
                ));
            }
            (_, Kind::Node) => c.hash,
            _ => {
                return Err(err(
                    "VG-C0108",
                    name.span,
                    format!("{} is not a NODE kernel", name.text),
                ));
            }
        };
        let h = h.ok_or_else(|| {
            err(
                "VG-S0009",
                name.span,
                format!("@{} has no resolution in vg.lock", name.text),
            )
        })?;
        Ok((self.call(h), c))
    }

    fn infer(&mut self, f: &ast::Formula, cx: &mut Cx) -> R<FTy> {
        if let Some((_, t)) = self.result_term(f, cx)? {
            return Ok(t);
        }
        let c0101 = |m: String| Err(err("VG-C0101", f.span, m));
        Ok(match &f.kind {
            FKind::Value(v) => {
                // Inference must not add constants: type literals without keeping them.
                match v {
                    Value::Lit(l, span) => fty(&literal_const(l, *span)?.ty),
                    _ => self.leaf(v, cx)?.1,
                }
            }
            FKind::Num(s) => FTy::Num(s.contains(['.', 'e', 'E'])),
            FKind::Result => unreachable!("handled by result_term"),
            FKind::Unary(ast::UnOp::Not, e) => {
                if self.infer(e, cx)? != FTy::Bool {
                    return c0101("! needs a Bool".into());
                }
                FTy::Bool
            }
            FKind::Unary(ast::UnOp::Neg, e) => match self.infer(e, cx)? {
                t @ (FTy::Int | FTy::Float(_) | FTy::Num(_)) => t,
                t => return c0101(format!("- needs a number, found {t:?}")),
            },
            FKind::Binary(op, a, b) => {
                let (ta, tb) = (self.infer(a, cx)?, self.infer(b, cx)?);
                use ast::BinOp::*;
                match op {
                    And | Or | Implies => {
                        if ta != FTy::Bool || tb != FTy::Bool {
                            return c0101(format!("{} needs Bool operands", op.symbol()));
                        }
                        FTy::Bool
                    }
                    Eq | Ne => {
                        match (&ta, &tb) {
                            (FTy::Bool, FTy::Bool) => {}
                            (FTy::Val(x), FTy::Val(y)) if x == y => {}
                            _ => {
                                settle(unify(ta, tb, f.span)?, f.span)?;
                            }
                        }
                        FTy::Bool
                    }
                    Lt | Le | Gt | Ge => {
                        settle(unify(ta, tb, f.span)?, f.span)?;
                        FTy::Bool
                    }
                    Add | Sub | Mul | Div | Mod => unify(ta, tb, f.span)?,
                }
            }
            FKind::Index(base, idx) => {
                let elem = match self.infer(base, cx)? {
                    FTy::Val(Type::Array(e, _) | Type::Slice(e)) if idx.len() == 1 => *e,
                    FTy::Val(Type::Tensor(e, d)) if idx.len() == d.len() => *e,
                    t => return c0101(format!("cannot index {t:?} with {} indices", idx.len())),
                };
                for i in idx {
                    let t = self.infer(i, cx)?;
                    if settle(unify(t, FTy::Int, i.span)?, i.span)? != FTy::Int {
                        return c0101("an index is an integer".into());
                    }
                }
                fty(&elem)
            }
            FKind::Proj(base, j) => match self.infer(base, cx)? {
                FTy::Val(Type::Tuple(ms)) => {
                    match j.parse::<usize>().ok().and_then(|j| ms.get(j)) {
                        Some(t) => fty(t),
                        None => return c0101(format!(".{j} is out of range")),
                    }
                }
                t => return c0101(format!(".{j} needs a Tuple, found {t:?}")),
            },
            FKind::Forall { ranges, body } => {
                let depth0 = cx.bound.len();
                for r in ranges {
                    for e in [&r.lo, &r.hi] {
                        let t = self.infer(e, cx)?;
                        settle(unify(t, FTy::Int, e.span)?, e.span)?;
                    }
                    cx.bound.push((r.var.text.clone(), FTy::Int));
                }
                let t = self.infer(body, cx);
                cx.bound.truncate(depth0);
                if t? != FTy::Bool {
                    return c0101("a forall body is Bool".into());
                }
                FTy::Bool
            }
            FKind::Call { prefix, name, args } => {
                self.call_type(*prefix, name, args, cx, f.span)?
            }
        })
    }

    fn call_type(
        &mut self,
        prefix: Option<ast::CallPrefix>,
        name: &ast::Name,
        args: &[ast::Formula],
        cx: &mut Cx,
        span: Span,
    ) -> R<FTy> {
        let c0101 = |m: String| Err(err("VG-C0101", span, m));
        if prefix.is_none()
            && let Some(b) = Builtin::ALL.iter().find(|b| b.name() == name.text)
        {
            {
                if args.len() != b.arity() {
                    return Err(err(
                        "VG-C0103",
                        span,
                        format!("{} takes {} argument(s)", b.name(), b.arity()),
                    ));
                }
                let ts: Vec<FTy> = args.iter().map(|a| self.infer(a, cx)).collect::<R<_>>()?;
                return Ok(match (b, ts.as_slice()) {
                    (Builtin::Len, [FTy::Val(t)]) if t.elem().is_some() => FTy::Int,
                    (Builtin::IsFinite | Builtin::IsNan, [FTy::Float(_)]) => FTy::Bool,
                    (Builtin::Abs, [t @ (FTy::Int | FTy::Float(_) | FTy::Num(_))]) => t.clone(),
                    (Builtin::Min | Builtin::Max, [a, b]) => unify(a.clone(), b.clone(), span)?,
                    (Builtin::AsBytes, [t]) => {
                        let ok = match t {
                            FTy::Bool | FTy::Int | FTy::Float(_) => true,
                            FTy::Val(v) => v.elem().is_some_and(|e| e.is_scalar_or_bool()),
                            FTy::Num(_) => false,
                        };
                        if !ok {
                            return c0101(
                                "as_bytes takes a scalar or a collection of scalars".into(),
                            );
                        }
                        FTy::Val(Type::Slice(Box::new(Type::UInt(8))))
                    }
                    (Builtin::Same, [a, b]) => {
                        match (a, b) {
                            (FTy::Val(x), FTy::Val(y)) if x == y => {}
                            (FTy::Bool, FTy::Bool) => {}
                            _ => {
                                settle(unify(a.clone(), b.clone(), span)?, span)?;
                            }
                        }
                        FTy::Bool
                    }
                    (Builtin::IsOk, [FTy::Val(Type::Result(..))]) => FTy::Bool,
                    (Builtin::Ok, [FTy::Val(Type::Result(t, _))]) => fty(t),
                    (Builtin::Err, [FTy::Val(Type::Result(_, e))]) => fty(e),
                    _ => return c0101(format!("{} does not take {ts:?}", b.name())),
                });
            }
        }
        let (_, callee) = self.formula_callee(prefix, name)?;
        let (params, result) = self.call_signature(prefix, callee, args.len(), span)?;
        for (a, p) in args.iter().zip(&params) {
            let t = self.infer(a, cx)?;
            check_arg(t, p, a.span)?;
        }
        Ok(result)
    }

    /// The parameter types a formula call's arguments must have, and its result type.
    fn call_signature(
        &self,
        prefix: Option<ast::CallPrefix>,
        c: &Callable,
        nargs: usize,
        span: Span,
    ) -> R<(Vec<FTy>, FTy)> {
        let result = match c.results.as_slice() {
            [t] => fty(t),
            ts => FTy::Val(Type::Tuple(ts.to_vec())),
        };
        let params: Vec<FTy> = match prefix {
            Some(ast::CallPrefix::Fold) => {
                // fold:f(init, xs, k, b…) for a kernel f: (A, T, B…) -> (A).
                let ok = c.params.len() >= 2 && c.results.len() == 1 && c.params[0] == c.results[0];
                if !ok || nargs != c.params.len() + 1 {
                    return Err(err(
                        "VG-C0108",
                        span,
                        "fold: needs a kernel (A, T, B…) -> (A) and the arguments (init, xs, k, b…)",
                    ));
                }
                let mut v = vec![
                    fty(&c.params[0]),
                    FTy::Val(Type::Slice(Box::new(c.params[1].clone()))),
                    FTy::Int,
                ];
                v.extend(c.params[2..].iter().map(fty));
                v
            }
            _ => {
                if nargs != c.params.len() {
                    return Err(err(
                        "VG-C0103",
                        span,
                        format!("the call takes {} arguments, not {nargs}", c.params.len()),
                    ));
                }
                c.params.iter().map(fty).collect()
            }
        };
        Ok((params, result))
    }

    /// Builds the term of a formula already typed by `infer`; `want` types its untyped numbers.
    fn build(&mut self, f: &ast::Formula, cx: &mut Cx, want: Option<&FTy>) -> R<Term> {
        if let Some((t, _)) = self.result_term(f, cx)? {
            return Ok(t);
        }
        let bx = Box::new;
        Ok(match &f.kind {
            FKind::Value(v) => self.leaf(v, cx)?.0,
            FKind::Num(s) => {
                let decimal = s.contains(['.', 'e', 'E']);
                let c = match want {
                    Some(FTy::Float(t)) => Const {
                        ty: t.clone(),
                        bytes: float_bytes(
                            &ast::SNum {
                                neg: false,
                                text: s.clone(),
                            },
                            t,
                        )
                        .ok_or_else(|| err("VG-P0006", f.span, format!("{s} overflows {t}")))?,
                    },
                    Some(FTy::Int) | None if !decimal => match s.parse::<i128>() {
                        Ok(v) => int_const(v),
                        Err(_) => return Err(err("VG-P0003", f.span, format!("{s} is too large"))),
                    },
                    Some(FTy::Int) | None => {
                        return Err(err(
                            "VG-C0102",
                            f.span,
                            format!("{s} meets no float term to take its type from"),
                        ));
                    }
                    Some(t) => {
                        return Err(err(
                            "VG-C0101",
                            f.span,
                            format!("a number where {t:?} is expected"),
                        ));
                    }
                };
                Term::Const(self.konst(c))
            }
            FKind::Result => unreachable!("handled by result_term"),
            FKind::Unary(ast::UnOp::Not, e) => Term::Not(bx(self.build(e, cx, None)?)),
            FKind::Unary(ast::UnOp::Neg, e) => Term::Neg(bx(self.build(e, cx, want)?)),
            FKind::Binary(op, a, b) => {
                use ast::BinOp::*;
                let w = match op {
                    And | Or | Implies => None,
                    _ => {
                        let (ta, tb) = (self.infer(a, cx)?, self.infer(b, cx)?);
                        match (op, &ta, &tb) {
                            (Eq | Ne, FTy::Bool, FTy::Bool)
                            | (Eq | Ne, FTy::Val(_), FTy::Val(_)) => None,
                            (Add | Sub | Mul | Div | Mod, ..) => match unify(ta, tb, f.span)? {
                                FTy::Num(_) => want.cloned(),
                                t => Some(t),
                            },
                            _ => Some(settle(unify(ta, tb, f.span)?, f.span)?),
                        }
                    }
                };
                let (ta, tb) = (
                    self.build(a, cx, w.as_ref())?,
                    self.build(b, cx, w.as_ref())?,
                );
                Term::Bin(bin(*op), bx(ta), bx(tb))
            }
            FKind::Index(base, idx) => {
                let b = self.build(base, cx, None)?;
                let i = idx
                    .iter()
                    .map(|i| self.build(i, cx, Some(&FTy::Int)))
                    .collect::<R<_>>()?;
                Term::Index(bx(b), i)
            }
            FKind::Proj(base, j) => Term::Proj(
                bx(self.build(base, cx, None)?),
                j.parse().expect("checked by infer"),
            ),
            FKind::Forall { ranges, body } => {
                let depth0 = cx.bound.len();
                let mut rs = Vec::new();
                for r in ranges {
                    let lo = self.build(&r.lo, cx, Some(&FTy::Int))?;
                    let hi = self.build(&r.hi, cx, Some(&FTy::Int))?;
                    rs.push((lo, hi));
                    cx.bound.push((r.var.text.clone(), FTy::Int));
                }
                let b = self.build(body, cx, None);
                cx.bound.truncate(depth0);
                Term::Forall(rs, bx(b?))
            }
            FKind::Call { prefix, name, args } => {
                if prefix.is_none()
                    && let Some(b) = Builtin::ALL.iter().find(|b| b.name() == name.text)
                {
                    {
                        let w = match b {
                            Builtin::Abs => want.cloned(),
                            Builtin::Min | Builtin::Max | Builtin::Same => {
                                let ts: Vec<FTy> =
                                    args.iter().map(|a| self.infer(a, cx)).collect::<R<_>>()?;
                                match unify(ts[0].clone(), ts[1].clone(), f.span) {
                                    Ok(FTy::Num(d)) if *b == Builtin::Same => {
                                        Some(settle(FTy::Num(d), f.span)?)
                                    }
                                    Ok(FTy::Num(_)) => want.cloned(),
                                    Ok(t) => Some(t),
                                    Err(_) => None,
                                }
                            }
                            _ => None,
                        };
                        let a = args
                            .iter()
                            .map(|a| self.build(a, cx, w.as_ref()))
                            .collect::<R<_>>()?;
                        return Ok(Term::Builtin(*b, a));
                    }
                }
                let (ix, callee) = self.formula_callee(*prefix, name)?;
                let (params, _) = self.call_signature(*prefix, callee, args.len(), f.span)?;
                let a = args
                    .iter()
                    .zip(&params)
                    .map(|(a, p)| self.build(a, cx, Some(p)))
                    .collect::<R<_>>()?;
                match prefix {
                    Some(ast::CallPrefix::Spec) => Term::Spec(ix, a),
                    Some(ast::CallPrefix::Fold) => Term::Fold(ix, a),
                    None => Term::Apply(ix, a),
                }
            }
        })
    }
}

fn clause_name(c: Clause) -> &'static str {
    match c {
        Clause::Pre => "PRE",
        Clause::Post => "POST",
        Clause::Cost => "COST",
        Clause::Budget => "BUDGET",
        Clause::Decreases => "DECREASES",
        Clause::Invariant => "INVARIANT",
    }
}

fn bin(op: ast::BinOp) -> Bin {
    use ast::BinOp as A;
    match op {
        A::Implies => Bin::Implies,
        A::Or => Bin::Or,
        A::And => Bin::And,
        A::Eq => Bin::Eq,
        A::Ne => Bin::Ne,
        A::Lt => Bin::Lt,
        A::Le => Bin::Le,
        A::Gt => Bin::Gt,
        A::Ge => Bin::Ge,
        A::Add => Bin::Add,
        A::Sub => Bin::Sub,
        A::Mul => Bin::Mul,
        A::Div => Bin::Div,
        A::Mod => Bin::Mod,
    }
}

/// The numeric type two operands share (docs/03): an untyped number takes the float type it meets.
fn unify(a: FTy, b: FTy, span: Span) -> R<FTy> {
    Ok(match (a, b) {
        (FTy::Num(x), FTy::Num(y)) => FTy::Num(x || y),
        (FTy::Num(true), FTy::Int) | (FTy::Int, FTy::Num(true)) => {
            return Err(err(
                "VG-C0102",
                span,
                "a decimal number meets an integer term",
            ));
        }
        (FTy::Num(false), FTy::Int) | (FTy::Int, FTy::Num(false)) | (FTy::Int, FTy::Int) => {
            FTy::Int
        }
        (FTy::Num(_), FTy::Float(t)) | (FTy::Float(t), FTy::Num(_)) => FTy::Float(t),
        (FTy::Float(x), FTy::Float(y)) if x == y => FTy::Float(x),
        (a, b) => {
            return Err(err("VG-C0101", span, format!("{a:?} meets {b:?}")));
        }
    })
}

/// Untyped numbers that met nothing typed: integers become Int; a decimal is an error.
fn settle(t: FTy, span: Span) -> R<FTy> {
    match t {
        FTy::Num(true) => Err(err(
            "VG-C0102",
            span,
            "a decimal number meets no float term",
        )),
        FTy::Num(false) => Ok(FTy::Int),
        t => Ok(t),
    }
}

/// An argument against a formula call's parameter.
fn check_arg(t: FTy, p: &FTy, span: Span) -> R<()> {
    let ok = match (&t, p) {
        (FTy::Num(_), FTy::Float(_)) | (FTy::Num(false), FTy::Int) => true,
        (FTy::Val(Type::Array(e, _) | Type::Slice(e)), FTy::Val(Type::Slice(pe))) => e == pe,
        (a, b) => a == b,
    };
    if ok {
        Ok(())
    } else {
        Err(err(
            "VG-C0101",
            span,
            format!("argument {t:?} does not fit {p:?}"),
        ))
    }
}

pub fn depth(t: &Term) -> usize {
    let kids: Vec<&Term> = match t {
        Term::Not(a) | Term::Neg(a) | Term::Proj(a, _) => vec![a],
        Term::Bin(_, a, b) => vec![a, b],
        Term::Index(a, v) => std::iter::once(&**a).chain(v).collect(),
        Term::Forall(rs, b) => rs
            .iter()
            .flat_map(|(l, h)| [l, h])
            .chain(std::iter::once(&**b))
            .collect(),
        Term::Builtin(_, v) | Term::Spec(_, v) | Term::Fold(_, v) | Term::Apply(_, v) => {
            v.iter().collect()
        }
        _ => vec![],
    };
    1 + kids.into_iter().map(depth).max().unwrap_or(0)
}
