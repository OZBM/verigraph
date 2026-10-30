//! The VG-Sym syntax tree (docs/03). It keeps everything the printer needs to reproduce a tree
//! exactly: parse after print is the identity (docs/13, lens laws). Comments and layout are not kept.

use crate::diag::Span;
use crate::types::Handle;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Module {
    pub name: String,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Query(MeshQuery),
    Node(NodeDef),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MeshQuery {
    pub label: String,
    pub params: Vec<TypeExpr>,
    pub results: Vec<TypeExpr>,
    pub effects: Vec<Name>,
    pub pre: Option<Formula>,
    pub post: Formula,
    /// As written, quotes included.
    pub intent: Option<String>,
    pub span: Span,
}

/// A NODE, or a SPEC when `spec` is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeDef {
    pub spec: bool,
    pub label: String,
    pub attrs: Vec<Attr>,
    pub params: Vec<Param>,
    pub results: Vec<TypeExpr>,
    pub effects: Vec<Name>,
    pub clauses: Vec<Clause>,
    pub learnable: Option<Vec<Learn>>,
    pub body: Vec<Stmt>,
    /// From the NODE or SPEC keyword to the closing brace.
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub text: String,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attr {
    /// As written, quotes included.
    Intent(String),
    Export,
    Inline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Param {
    pub name: Name,
    pub ty: TypeExpr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TypeExpr {
    /// A scalar keyword: `u8` … `i64`, `f16`, `bf16`, `f32`, `f64`.
    Scalar(&'static str),
    Bool,
    Eff,
    Handle(Handle),
    Array(Box<TypeExpr>, Shape),
    Tensor(Box<TypeExpr>, Shape),
    Slice(Box<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    Result(Box<TypeExpr>, Box<TypeExpr>),
    /// The byte count as written.
    Arena(String),
    /// `{x: T | φ}`.
    Refine(Name, Box<TypeExpr>, Box<Formula>),
}

/// A shape as written: `2` or `[512, 2]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    pub bracketed: bool,
    pub dims: Vec<Dim>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dim {
    Int(String),
    Ident(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClauseKind {
    Pre,
    Post,
    Invariant { label: Name, k: Name, acc: Name },
    Decreases,
    Cost,
    Budget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clause {
    pub kind: ClauseKind,
    pub formula: Formula,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Learn {
    pub name: Name,
    pub ty: &'static str,
    pub lo: SNum,
    pub hi: SNum,
    pub loss: Option<Name>,
}

/// A number with an optional minus sign, as written: `-0.5`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SNum {
    pub neg: bool,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stmt {
    Assign {
        binds: Vec<Bind>,
        op: Name,
        mode: Option<Name>,
        args: Vec<Arg>,
        span: Span,
    },
    Return {
        values: Vec<Value>,
        span: Span,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bind {
    /// The port's digits, without `%`.
    pub port: Name,
    pub ty: Option<TypeExpr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arg {
    pub name: Option<Name>,
    pub val: ArgVal,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgVal {
    Value(Value),
    /// A label, without `@`.
    Label(Name),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// A port's digits, without `%`.
    Port(Name),
    Ident(Name),
    Lit(Literal, Span),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Literal {
    /// `1.0:f32`, `-128:i8`.
    Num(SNum, &'static str),
    Bool(bool),
    /// `[1, 2]:Array<u8, 2>`, row-major.
    Array(Vec<Elem>, TypeExpr),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Elem {
    Num(SNum),
    Bool(bool),
    Array(Vec<Elem>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Formula {
    pub kind: FKind,
    pub span: Span,
    /// The tree's depth, 1 for a leaf; follows from `kind`.
    pub depth: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPrefix {
    Spec,
    Fold,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FKind {
    Value(Value),
    /// An untyped number, as written.
    Num(String),
    Result,
    Call {
        prefix: Option<CallPrefix>,
        name: Name,
        args: Vec<Formula>,
    },
    Forall {
        ranges: Vec<Range>,
        body: Box<Formula>,
    },
    Unary(UnOp, Box<Formula>),
    Binary(BinOp, Box<Formula>, Box<Formula>),
    Index(Box<Formula>, Vec<Formula>),
    /// `.j`, with j as written.
    Proj(Box<Formula>, String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Range {
    pub var: Name,
    pub lo: Formula,
    pub hi: Formula,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Implies,
    Or,
    And,
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

impl BinOp {
    pub fn symbol(self) -> &'static str {
        match self {
            BinOp::Implies => "==>",
            BinOp::Or => "||",
            BinOp::And => "&&",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
        }
    }

    /// Binding strength: 1 `==>`, 2 `||`, 3 `&&`, 4 comparisons, 5 `+ -`, 6 `* / %`.
    pub fn level(self) -> u8 {
        match self {
            BinOp::Implies => 1,
            BinOp::Or => 2,
            BinOp::And => 3,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 4,
            BinOp::Add | BinOp::Sub => 5,
            BinOp::Mul | BinOp::Div | BinOp::Mod => 6,
        }
    }
}

impl Formula {
    pub fn new(kind: FKind) -> Formula {
        Formula::at(kind, Span::default())
    }

    pub fn at(kind: FKind, span: Span) -> Formula {
        let below = |fs: &mut dyn Iterator<Item = &Formula>| fs.map(|f| f.depth).max().unwrap_or(0);
        let depth = 1u16.saturating_add(match &kind {
            FKind::Value(_) | FKind::Num(_) | FKind::Result => 0,
            FKind::Call { args, .. } => below(&mut args.iter()),
            FKind::Forall { ranges, body } => below(
                &mut ranges
                    .iter()
                    .flat_map(|r| [&r.lo, &r.hi])
                    .chain(std::iter::once(&**body)),
            ),
            FKind::Unary(_, a) | FKind::Proj(a, _) => a.depth,
            FKind::Binary(_, a, b) => a.depth.max(b.depth),
            FKind::Index(a, idx) => below(&mut std::iter::once(&**a).chain(idx)),
        });
        Formula { kind, span, depth }
    }
}

impl Name {
    pub fn new(text: impl Into<String>) -> Name {
        Name {
            text: text.into(),
            span: Span::default(),
        }
    }
}
