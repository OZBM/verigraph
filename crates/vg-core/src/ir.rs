//! The VG-IR data model (P1.1): one node is one function, as docs/02 lays it out.

use crate::types::{EffectMask, Type};

pub type Hash = [u8; 32];

/// What an op reads (docs/02, OPERANDS.src_kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Operand {
    /// An input by position: parameters, then LEARNABLE parameters θ.
    Input(u32),
    /// Result `slot` of op `index`.
    Op(u32, u8),
    /// A CONSTS index.
    Const(u32),
    /// A CALLS index: a kernel, a callee or a branch.
    Callee(u32),
    /// An index into the contract's INVARIANT clauses.
    Invariant(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Op {
    pub opcode: u16,
    pub mode: u8,
    pub operands: Vec<Operand>,
    pub results: Vec<Type>,
}

/// A typed constant: its exact bytes (docs/13), NaN written canonically; an Int constant holds its
/// shortest two's-complement bytes, little-endian, at least one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Const {
    pub ty: Type,
    pub bytes: Vec<u8>,
}

/// A contract formula, with names replaced by positions (ADR 0007).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Term {
    Input(u32),
    /// Result j of the node.
    Result(u32),
    /// Result `slot` of op `index`; INVARIANT clauses only.
    Port(u32, u8),
    /// A bound variable as a de Bruijn index: 0 is the innermost binder.
    Bound(u32),
    Const(u32),
    /// A COST or BUDGET resource.
    Resource(Resource),
    Not(Box<Term>),
    Neg(Box<Term>),
    Bin(Bin, Box<Term>, Box<Term>),
    Index(Box<Term>, Vec<Term>),
    Proj(Box<Term>, u32),
    /// Ranges `(lo, hi)`, each bound in the later ranges and in the body.
    Forall(Vec<(Term, Term)>, Box<Term>),
    Builtin(Builtin, Vec<Term>),
    /// `spec:s(…)`, by CALLS index.
    Spec(u32, Vec<Term>),
    /// `fold:f(…)`, by CALLS index.
    Fold(u32, Vec<Term>),
    /// A kernel applied inside a formula (ADR 0008), by CALLS index.
    Apply(u32, Vec<Term>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bin {
    And,
    Or,
    Implies,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Builtin {
    Len = 1,
    IsFinite,
    IsNan,
    Abs,
    Min,
    Max,
    AsBytes,
    Same,
    IsOk,
    Ok,
    Err,
}

impl Builtin {
    pub const ALL: [Builtin; 11] = [
        Builtin::Len,
        Builtin::IsFinite,
        Builtin::IsNan,
        Builtin::Abs,
        Builtin::Min,
        Builtin::Max,
        Builtin::AsBytes,
        Builtin::Same,
        Builtin::IsOk,
        Builtin::Ok,
        Builtin::Err,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Builtin::Len => "len",
            Builtin::IsFinite => "is_finite",
            Builtin::IsNan => "is_nan",
            Builtin::Abs => "abs",
            Builtin::Min => "min",
            Builtin::Max => "max",
            Builtin::AsBytes => "as_bytes",
            Builtin::Same => "same",
            Builtin::IsOk => "is_ok",
            Builtin::Ok => "ok",
            Builtin::Err => "err",
        }
    }

    pub fn arity(self) -> usize {
        match self {
            Builtin::Min | Builtin::Max | Builtin::Same => 2,
            _ => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    HeapBytes = 0,
    ArenaBytes,
    StackBytes,
    Ops,
    LatencyP99Us,
}

impl Resource {
    pub const ALL: [Resource; 5] = [
        Resource::HeapBytes,
        Resource::ArenaBytes,
        Resource::StackBytes,
        Resource::Ops,
        Resource::LatencyP99Us,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Resource::HeapBytes => "heap_bytes",
            Resource::ArenaBytes => "arena_bytes",
            Resource::StackBytes => "stack_bytes",
            Resource::Ops => "ops",
            Resource::LatencyP99Us => "latency_p99_us",
        }
    }
}

/// Each clause kind is one conjunction, its conjuncts in source order (ADR 0007). An INVARIANT binds
/// two variables, the step count and the accumulator: Bound(1) and Bound(0) at its top level.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Contract {
    pub pre: Vec<Term>,
    pub post: Vec<Term>,
    pub cost: Vec<Term>,
    pub budget: Vec<Term>,
    pub decreases: Vec<Term>,
    pub invariants: Vec<Vec<Term>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Learnable {
    pub ty: Type,
    /// CONSTS indices of the bounds, in `ty`.
    pub lo: u32,
    pub hi: u32,
    /// The loss node, by CALLS index.
    pub loss: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    /// A SPEC node: its identity is a spec_hash (docs/02).
    pub spec: bool,
    pub effects: EffectMask,
    pub params: Vec<Type>,
    pub results: Vec<Type>,
    /// The LEARNABLE parameters θ, the implicit trailing inputs.
    pub learnable: Vec<Learnable>,
    pub consts: Vec<Const>,
    pub calls: Vec<Hash>,
    pub ops: Vec<Op>,
    /// What RETURN returns, one per result.
    pub outputs: Vec<Operand>,
    pub contract: Contract,
    /// The node's VG-Sym source as written (`vg asm --keep-debug`); never hashed.
    pub debug: Option<String>,
}

impl Node {
    /// The type of input i: parameters, then θ.
    pub fn input_type(&self, i: u32) -> Option<&Type> {
        let i = i as usize;
        match self.params.get(i) {
            Some(t) => Some(t),
            None => self.learnable.get(i - self.params.len()).map(|l| &l.ty),
        }
    }

    pub fn input_count(&self) -> usize {
        self.params.len() + self.learnable.len()
    }

    /// The type of a value operand.
    pub fn operand_type(&self, o: Operand) -> Option<&Type> {
        match o {
            Operand::Input(i) => self.input_type(i),
            Operand::Op(i, s) => self.ops.get(i as usize)?.results.get(s as usize),
            Operand::Const(c) => self.consts.get(c as usize).map(|c| &c.ty),
            Operand::Callee(_) | Operand::Invariant(_) => None,
        }
    }
}

/// Hex digits of a hash.
pub fn hex(h: &Hash) -> String {
    h.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parses 64 hex digits.
pub fn parse_hex(s: &str) -> Option<Hash> {
    if s.len() != 64 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let mut h = [0u8; 32];
    for (i, b) in h.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(h)
}
