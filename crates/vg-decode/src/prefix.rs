//! Tier 2 incremental checker (P5.4, docs/15): prefix-checkable parser, type/effect/linearity
//! checker, and exact `Allowed` continuation query for constrained decoding.

use std::collections::{BTreeMap, BTreeSet};

use vg_core::ast::{self, BinOp, CallPrefix, SNum, UnOp};
use vg_core::diag::{Diagnostic, Span};
use vg_core::float;
use vg_core::lexer::{LOWER_KEYWORDS, Tok, lex};
use vg_core::lower::literal_const;
use vg_core::opcode::{self, Modes, OpInfo, Shape};
use vg_core::types::{EffectMask, Handle, Type};
use vg_types::cost::{
    MAX_STATIC_BYTES, MAX_STATIC_ELEMS, MAX_TYPE_DEPTH, abi_size_align, is_fixed_size,
    static_element_count,
};

/// Function signature of a NODE, SPEC, or `@MESH_QUERY` label.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FnType {
    pub params: Vec<Type>,
    pub results: Vec<Type>,
}

/// Token classes permitted at the current prefix position.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TokenClass {
    Keyword(&'static str),
    Punct(&'static str),
    /// Any fresh lowercase/underscore identifier (`[a-z_][A-Za-z0-9_]*`).
    Ident,
    /// Specific allowed identifier (op mode, named arg, formula variable/builtin/resource).
    Name(String),
    /// Fresh LHS port definition (`%[0-9]+`).
    NewPort,
    /// Fresh label definition (`@[a-z_][A-Za-z0-9_]*`).
    NewLabel,
    IntLiteral,
    DecimalLiteral,
    StringLiteral,
}

/// Exact continuations at the current prefix without a parse, structure, type, or effect error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Allowed {
    pub token_classes: Vec<TokenClass>,
    pub ports: Vec<(String, Type)>,
    pub labels: Vec<(String, FnType)>,
    pub opcodes: Vec<(String, Vec<String>)>,
}

impl Allowed {
    /// Returns `true` iff `token` is a single valid lexical token permitted by this `Allowed` set.
    pub fn accepts(&self, token: &str) -> bool {
        let Ok(toks) = lex(token) else {
            return false;
        };
        if toks.len() != 2 {
            return false;
        }
        match &toks[0].tok {
            Tok::Kw(k) => {
                self.token_classes.contains(&TokenClass::Keyword(k))
                    || self.opcodes.iter().any(|(op, _)| op == k)
            }
            Tok::Punct(p) => self.token_classes.contains(&TokenClass::Punct(p)),
            Tok::Ident(s) => {
                self.token_classes.contains(&TokenClass::Ident)
                    || self.token_classes.contains(&TokenClass::Name(s.clone()))
                    || self.ports.iter().any(|(n, _)| n == token)
            }
            Tok::Port(_) => {
                self.token_classes.contains(&TokenClass::NewPort)
                    || self.ports.iter().any(|(n, _)| n == token)
            }
            Tok::Label(_) => {
                self.token_classes.contains(&TokenClass::NewLabel)
                    || self.labels.iter().any(|(n, _)| n == token)
            }
            Tok::OpName(s) => self.opcodes.iter().any(|(op, _)| op == s),
            Tok::Int(_) => self.token_classes.contains(&TokenClass::IntLiteral),
            Tok::Decimal(_) => self.token_classes.contains(&TokenClass::DecimalLiteral),
            Tok::Str(_) => self.token_classes.contains(&TokenClass::StringLiteral),
            Tok::Eof => false,
        }
    }
}

const SCALARS: &[&str] = &[
    "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f16", "bf16", "f32", "f64",
];
const FLOATS: &[&str] = &["f16", "bf16", "f32", "f64"];
const EFFECTS: &[&str] = &EffectMask::NAMES;
const HANDLES: &[&str] = &["FileIn", "FileOut", "Socket", "Clock", "GpuQueue"];
const RESOURCES: &[&str] = &[
    "heap_bytes",
    "arena_bytes",
    "stack_bytes",
    "ops",
    "latency_p99_us",
];
const BUILTINS: &[&str] = &[
    "len",
    "is_finite",
    "is_nan",
    "abs",
    "min",
    "max",
    "as_bytes",
    "same",
    "is_ok",
    "ok",
    "err",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemKind {
    Node,
    Spec,
    Query,
}

#[derive(Clone, Debug)]
struct LabelInfo {
    kind: ItemKind,
    fn_type: FnType,
    effects: EffectMask,
    result_views_param: Vec<Vec<bool>>,
    result_handle_valid: Vec<Option<Handle>>,
}

#[derive(Clone, Debug)]
struct ValMeta {
    ty: Type,
    origin_local_frame: bool,
    has_local_slice: bool,
    owns_fresh_arena_slice: bool,
    from_params: Vec<bool>,
    valid_handle: Option<Handle>,
    const_u64: Option<u64>,
    is_literal: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClauseKind {
    Pre,
    Post,
    Invariant,
    Decreases,
    Cost,
    Budget,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FTy {
    Bool,
    Int,
    Float(Type),
    Val(Type),
    Num(bool),
    /// Placeholder for an INVARIANT accumulator parameter `acc` before FOLD_BOUNDED binds it.
    AnyInvAcc,
}

#[derive(Clone, Debug)]
struct InvariantInfo {
    k_name: String,
    acc_name: String,
    bound_acc_ty: Option<Type>,
    pending_ports: Vec<(String, Vec<FTy>)>,
}

#[derive(Clone, Debug)]
struct NodeState {
    kind: ItemKind,
    label: String,
    params: Vec<(String, Type)>,
    results: Vec<Type>,
    effects: EffectMask,
    invariants: BTreeMap<String, InvariantInfo>,
    /// Named values visible in CONTRACT/LEARNABLE/BODY (parameters + learnables).
    inputs: Vec<(String, Type)>,
    values: BTreeMap<String, ValMeta>,
    eff_live: BTreeSet<String>,
    /// Fresh arena slices provisionally consumed in-place by an `UPDATE` when `ARENA` is absent.
    consumed_in_place: BTreeSet<String>,
    /// Value names used at least once in BODY.
    used_counts: BTreeMap<String, u32>,
    has_returned: bool,
    /// Per-result provenance computed at RETURN for caller slice-escape/handle checks.
    result_views_param: Vec<Vec<bool>>,
    result_handle_valid: Vec<Option<Handle>>,
}

impl NodeState {
    fn new(kind: ItemKind, label: String) -> Self {
        Self {
            kind,
            label,
            params: Vec::new(),
            results: Vec::new(),
            effects: EffectMask(0),
            invariants: BTreeMap::new(),
            inputs: Vec::new(),
            values: BTreeMap::new(),
            eff_live: BTreeSet::new(),
            consumed_in_place: BTreeSet::new(),
            used_counts: BTreeMap::new(),
            has_returned: false,
            result_views_param: Vec::new(),
            result_handle_valid: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
enum TypeCont {
    QueryParam,
    QueryResult,
    NodeParam { name: String },
    NodeResult,
    Binding { port: String },
    ArrayLit { elems: Vec<ast::Elem> },
    FormulaArrayLit { elems: Vec<ast::Elem> },
    RefineBase { var: String },
    ArrayElem,
    TensorElem,
    SliceElem,
    TupleElem { elems: Vec<Type> },
    ResultOk,
    ResultErr { ok: Type },
}

#[derive(Clone, Debug)]
enum TypeParse {
    Start {
        allow_refine: bool,
        cont: TypeCont,
    },
    RefineVar {
        cont: TypeCont,
    },
    RefineColon {
        var: String,
        cont: TypeCont,
    },
    RefineBar {
        var: String,
        base: Type,
        cont: TypeCont,
    },
    RefineClose {
        base: Type,
        cont: TypeCont,
    },
    OpenAngle {
        kw: &'static str,
        cont: TypeCont,
    },
    ArrayComma {
        elem: Type,
        cont: TypeCont,
    },
    TensorComma {
        elem: Type,
        cont: TypeCont,
    },
    ShapeDim {
        kw: &'static str,
        elem: Type,
        bracketed: bool,
        dims: Vec<u32>,
        cont: TypeCont,
    },
    ShapeCommaOrClose {
        kw: &'static str,
        elem: Type,
        dims: Vec<u32>,
        cont: TypeCont,
    },
    CloseAngle {
        ty: Type,
        cont: TypeCont,
    },
    TupleCommaOrClose {
        elems: Vec<Type>,
        cont: TypeCont,
    },
    ResultComma {
        ok: Type,
        cont: TypeCont,
    },
}

#[derive(Clone, Debug)]
enum FormulaCont {
    QueryPre,
    QueryPost,
    Clause {
        kind: ClauseKind,
        inv_label: Option<String>,
    },
    Refine {
        base: Type,
        cont: TypeCont,
    },
    Paren,
    Index {
        base: FTy,
        dims_left: Option<usize>,
    },
    ForallLo {
        ranges_done: Vec<String>,
        var: String,
    },
    ForallHi {
        ranges_done: Vec<String>,
        var: String,
    },
    ForallBody {
        saved_bound_len: usize,
    },
    CallArg {
        callee: FormulaCallee,
        arg_tys: Vec<FTy>,
    },
}

#[derive(Clone, Debug)]
enum FormulaCallee {
    Builtin(&'static str),
    Node(FnType),
    Spec(FnType),
    Fold(FnType),
}

#[derive(Clone, Debug)]
struct FormulaCtx {
    clause: ClauseKind,
    bound: Vec<(String, FTy)>,
    inv_label: Option<String>,
    allow_ports: bool,
    allow_result: bool,
    allow_resources: bool,
}

#[derive(Clone, Debug)]
struct FormulaFrame {
    unaries: Vec<UnOp>,
    ops: Vec<(BinOp, FTy)>,
    expect_atom: bool,
    cur: Option<(FTy, bool /* is_result_root */)>,
    min_allowed_prec: u8,
    cont: FormulaCont,
}

#[derive(Clone, Debug)]
enum FormulaState {
    Frame(FormulaFrame),
    AfterResultPendingDot,
    AfterIdent {
        name: String,
    },
    AfterNum {
        text: String,
        is_decimal: bool,
        neg_popped: bool,
    },
    AfterNumColon {
        snum: SNum,
    },
    ArrayElem {
        stack: Vec<Vec<ast::Elem>>,
        expect_elem: bool,
    },
    ArraySign {
        stack: Vec<Vec<ast::Elem>>,
    },
    ArrayColon {
        elems: Vec<ast::Elem>,
    },
    AfterCallPrefix {
        prefix: CallPrefix,
    },
    AfterCallColon {
        prefix: CallPrefix,
    },
    AfterCallName {
        callee: FormulaCallee,
    },
    CallCommaOrClose {
        callee: FormulaCallee,
        arg_tys: Vec<FTy>,
    },
    AfterDot {
        base: FTy,
        is_result_root: bool,
    },
    IndexCommaOrClose {
        base: FTy,
        dims_left: Option<usize>,
    },
    ForallOpen,
    ForallVar {
        ranges_done: Vec<String>,
    },
    ForallIn {
        ranges_done: Vec<String>,
        var: String,
    },
    ForallDots {
        ranges_done: Vec<String>,
        var: String,
    },
    ForallCommaOrClose {
        ranges_done: Vec<String>,
    },
    ForallLBrace {
        saved_bound_len: usize,
    },
    ForallRBrace,
}

#[derive(Clone, Debug)]
struct StmtBind {
    port: String,
    ann: Option<Type>,
}

#[derive(Clone, Debug)]
struct StmtArg {
    name: Option<String>,
    val: Option<ValMeta>,
    callee: Option<(String, LabelInfo)>,
}

#[derive(Clone, Debug)]
struct StmtCall {
    op_name: String,
    info: &'static OpInfo,
    mode: Option<String>,
    cast_ty: Option<Type>,
    binds: Vec<StmtBind>,
    args: Vec<StmtArg>,
    named_seen: BTreeSet<String>,
    cur_arg_name: Option<String>,
}

#[derive(Clone, Debug)]
enum State {
    ModuleKw,
    ModuleName,
    ModuleSemi,
    TopLevel,

    // @MESH_QUERY
    QueryLabel,
    QueryLBrace,
    QuerySigKw,
    QuerySigColon,
    QuerySigLParen,
    QuerySigParamOrClose {
        first: bool,
    },
    QuerySigCommaOrClose,
    QueryArrow,
    QueryResLParen,
    QueryResCommaOrClose,
    QueryResSemi,
    QueryEffKw,
    QueryEffColon,
    QueryEffLBracket,
    QueryEffElemOrClose {
        first: bool,
    },
    QueryEffCommaOrClose,
    QueryEffSemi,
    QueryPreOrPostKw,
    QueryPreColon,
    QueryPreSemi,
    QueryPostKw,
    QueryPostColon,
    QueryPostSemi,
    QueryIntentOrClose,
    QueryIntentColon,
    QueryIntentStr,
    QueryIntentSemi,
    QueryRBrace,

    // NODE / SPEC
    ItemLabel {
        kind: ItemKind,
    },
    NodeAttrOrLBrace,
    NodeAttrKey,
    NodeAttrIntentColon,
    NodeAttrIntentStr,
    NodeAttrCommaOrClose,
    NodeLBrace,
    SigKw,
    SigColon,
    SigLParen,
    SigParamOrClose {
        first: bool,
    },
    SigParamColon {
        name: String,
    },
    SigParamCommaOrClose,
    SigArrow,
    SigResLParen,
    SigResCommaOrClose,
    SigSemi,
    EffKw,
    EffColon,
    EffLBracket,
    EffElemOrClose {
        first: bool,
    },
    EffCommaOrClose,
    EffSemi,
    ContractKw,
    ContractLBrace,
    ContractClauseOrClose,
    ClauseColon {
        kind: ClauseKind,
        inv_label: Option<String>,
    },
    ClauseSemi,
    InvLabel,
    InvLParen {
        label: String,
    },
    InvK {
        label: String,
    },
    InvComma {
        label: String,
        k: String,
    },
    InvAcc {
        label: String,
        k: String,
    },
    InvRParen {
        label: String,
        k: String,
        acc: String,
    },
    BodyOrLearnable,
    LearnableLBrace,
    LearnableEntryOrClose {
        first: bool,
    },
    LearnableColon {
        name: String,
    },
    LearnableFloat {
        name: String,
    },
    LearnableIn {
        name: String,
        ty: &'static str,
    },
    LearnableLBracket {
        name: String,
        ty: &'static str,
    },
    LearnableLoSign {
        name: String,
        ty: &'static str,
    },
    LearnableLoNum {
        name: String,
        ty: &'static str,
        neg: bool,
    },
    LearnableComma {
        name: String,
        ty: &'static str,
        lo: f64,
    },
    LearnableHiSign {
        name: String,
        ty: &'static str,
        lo: f64,
    },
    LearnableHiNum {
        name: String,
        ty: &'static str,
        lo: f64,
        neg: bool,
    },
    LearnableRBracket {
        name: String,
        ty: &'static str,
    },
    LearnableLossOrSemi,
    LearnableLossColon,
    LearnableLossLabel,
    LearnableSemi,
    BodyLBrace,

    // BODY statements
    StmtStart {
        first: bool,
    },
    BindAfterPort {
        binds: Vec<StmtBind>,
        port: String,
    },
    BindAfterColon {
        binds: Vec<StmtBind>,
    },
    BindCommaOrEq {
        binds: Vec<StmtBind>,
    },
    BindNextPort {
        binds: Vec<StmtBind>,
    },
    OpName {
        binds: Vec<StmtBind>,
    },
    OpDotOrLParen {
        call: StmtCall,
    },
    OpMode {
        call: StmtCall,
    },
    OpLParen {
        call: StmtCall,
    },
    OpArgOrClose {
        call: StmtCall,
        first: bool,
    },
    OpArgAfterIdent {
        call: StmtCall,
        ident: String,
    },
    OpArgAfterEq {
        call: StmtCall,
    },
    OpLitSign {
        call: StmtCall,
        neg: bool,
    },
    OpLitColon {
        call: StmtCall,
        snum: SNum,
    },
    OpLitScalar {
        call: StmtCall,
        snum: SNum,
    },
    OpArrayElem {
        call: StmtCall,
        stack: Vec<Vec<ast::Elem>>,
        expect_elem: bool,
    },
    OpArraySign {
        call: StmtCall,
        stack: Vec<Vec<ast::Elem>>,
    },
    OpArrayColon {
        call: StmtCall,
        elems: Vec<ast::Elem>,
    },
    OpArgCommaOrClose {
        call: StmtCall,
    },
    OpSemi,

    ReturnLParen,
    ReturnArg {
        idx: usize,
    },
    ReturnLitSign {
        idx: usize,
        neg: bool,
    },
    ReturnLitColon {
        idx: usize,
        snum: SNum,
    },
    ReturnLitScalar {
        idx: usize,
        snum: SNum,
    },
    ReturnArrayElem {
        idx: usize,
        stack: Vec<Vec<ast::Elem>>,
        expect_elem: bool,
    },
    ReturnArraySign {
        idx: usize,
        stack: Vec<Vec<ast::Elem>>,
    },
    ReturnArrayColon {
        idx: usize,
        elems: Vec<ast::Elem>,
    },
    ReturnCommaOrClose {
        idx: usize,
    },
    ReturnSemi,
    BodyRBrace,
    NodeRBrace,
}

/// Prefix-checkable VG-Sym parser and type/effect checker (Tier 2, P5.4).
#[derive(Clone, Debug)]
pub struct Prefix {
    state: State,
    type_stack: Vec<TypeParse>,
    formula_stack: Vec<FormulaState>,
    formula_ctx: Option<FormulaCtx>,
    labels: BTreeMap<String, LabelInfo>,
    node: Option<NodeState>,
    query_params: Vec<Type>,
    query_results: Vec<Type>,
    query_effects: EffectMask,
    query_label: String,
    tok_count: u32,
}

impl Default for Prefix {
    fn default() -> Self {
        Self::new()
    }
}

impl Prefix {
    pub fn new() -> Prefix {
        Prefix {
            state: State::ModuleKw,
            type_stack: Vec::new(),
            formula_stack: Vec::new(),
            formula_ctx: None,
            labels: BTreeMap::new(),
            node: None,
            query_params: Vec::new(),
            query_results: Vec::new(),
            query_effects: EffectMask(0),
            query_label: String::new(),
            tok_count: 0,
        }
    }

    fn span(&self, token: &str) -> Span {
        Span {
            line: 1,
            col: self.tok_count + 1,
            len: token.chars().count() as u32,
            offset: self.tok_count,
        }
    }

    fn err(&self, code: &'static str, token: &str, msg: impl Into<String>) -> Diagnostic {
        let mut d = Diagnostic::error(code, self.span(token), msg);
        if let Some(n) = &self.node {
            d.node = Some(format!("@{}", n.label));
        }
        d
    }

    /// Feed one lexical token. Returns `Err` when the prefix can no longer become a valid module.
    pub fn push(&mut self, token: &str) -> Result<(), Diagnostic> {
        let toks = lex(token)?;
        if toks.len() != 2 {
            return Err(self.err("VG-P0001", token, "push expects exactly one lexical token"));
        }
        let tok = toks[0].tok.clone();
        let res = self.step(&tok, token);
        self.tok_count += 1;
        res
    }

    /// What may come next without a parse, structure, type, or effect error.
    pub fn allowed(&self) -> Allowed {
        if let Some(ts) = self.type_stack.last() {
            return self.allowed_type(ts);
        }
        if let Some(fs) = self.formula_stack.last() {
            return self.allowed_formula(fs);
        }
        self.allowed_main()
    }
}

// ---------------------------------------------------------------------------
// Type validation helpers
// ---------------------------------------------------------------------------

fn contains_eff_or_handle(ty: &Type) -> bool {
    match ty {
        Type::Eff | Type::Handle(_) => true,
        Type::Array(e, _) | Type::Slice(e) | Type::Tensor(e, _) => contains_eff_or_handle(e),
        Type::Tuple(ms) => ms.iter().any(contains_eff_or_handle),
        Type::Result(a, b) => contains_eff_or_handle(a) || contains_eff_or_handle(b),
        _ => false,
    }
}

fn type_contains_slice(ty: &Type) -> bool {
    match ty {
        Type::Slice(_) => true,
        Type::Array(e, _) | Type::Tensor(e, _) => type_contains_slice(e),
        Type::Tuple(ms) => ms.iter().any(type_contains_slice),
        Type::Result(a, b) => type_contains_slice(a) || type_contains_slice(b),
        _ => false,
    }
}

fn is_frame_value(ty: &Type) -> bool {
    !matches!(ty, Type::Slice(_) | Type::Eff | Type::Handle(_))
}

fn is_valid_scalar(ty: &Type) -> bool {
    matches!(
        ty,
        Type::UInt(8 | 16 | 32 | 64)
            | Type::SInt(8 | 16 | 32 | 64)
            | Type::Float(16 | 32 | 64)
            | Type::BFloat16
    )
}

fn is_mvp_arith_float(ty: &Type) -> bool {
    matches!(ty, Type::Float(32 | 64))
}

fn is_f16_or_bf16(ty: &Type) -> bool {
    matches!(ty, Type::Float(16) | Type::BFloat16)
}

fn has_byte_form(ty: &Type) -> bool {
    match ty {
        Type::Slice(e) => e.byte_size().is_some(),
        _ => ty.byte_size().is_some(),
    }
}

fn is_valid_cast(src: &Type, dst: &Type) -> bool {
    if src == dst || !is_valid_scalar(dst) {
        return false;
    }
    match src {
        Type::Bool => dst.is_int(),
        t if is_valid_scalar(t) => true,
        _ => false,
    }
}

fn validate_type(
    ty: &Type,
    allow_top_eff_handle: bool,
    depth: usize,
    span: Span,
) -> Result<(), Diagnostic> {
    if depth > MAX_TYPE_DEPTH {
        return Err(Diagnostic::error(
            "VG-T0118",
            span,
            format!("type nested deeper than {MAX_TYPE_DEPTH} levels"),
        ));
    }
    match ty {
        Type::Bool | Type::BFloat16 => Ok(()),
        Type::UInt(8 | 16 | 32 | 64) | Type::SInt(8 | 16 | 32 | 64) => Ok(()),
        Type::Float(16 | 32 | 64) => Ok(()),
        Type::UInt(_) | Type::SInt(_) | Type::Float(_) | Type::Int => Err(Diagnostic::error(
            "VG-T0101",
            span,
            format!("invalid type {ty}"),
        )),
        Type::Arena(_) => Err(Diagnostic::error(
            "VG-S0104",
            span,
            "Arena<N> is [ALPHA] and not available in the MVP",
        )),
        Type::Eff | Type::Handle(_) => {
            if allow_top_eff_handle {
                Ok(())
            } else {
                Err(Diagnostic::error(
                    "VG-T0119",
                    span,
                    format!("{ty} is not allowed inside a composite type"),
                ))
            }
        }
        Type::Array(e, _) => {
            validate_type(e, false, depth + 1, span)?;
            check_static_size(ty, span)
        }
        Type::Slice(e) => validate_type(e, false, depth + 1, span),
        Type::Tensor(e, dims) => {
            if dims.is_empty() {
                return Err(Diagnostic::error(
                    "VG-T0101",
                    span,
                    "Tensor must have at least one dimension",
                ));
            }
            validate_type(e, false, depth + 1, span)?;
            check_static_size(ty, span)
        }
        Type::Tuple(ms) => {
            if ms.is_empty() {
                return Err(Diagnostic::error(
                    "VG-T0101",
                    span,
                    "Tuple must have at least one member",
                ));
            }
            for m in ms {
                validate_type(m, false, depth + 1, span)?;
            }
            check_static_size(ty, span)
        }
        Type::Result(ok, err) => {
            validate_type(ok, false, depth + 1, span)?;
            validate_type(err, false, depth + 1, span)?;
            check_static_size(ty, span)
        }
    }
}

fn check_static_size(ty: &Type, span: Span) -> Result<(), Diagnostic> {
    if let Some(elems) = static_element_count(ty)
        && elems > MAX_STATIC_ELEMS
    {
        return Err(Diagnostic::error(
            "VG-T0118",
            span,
            format!("static type {ty} has {elems} elements, above 2^32 - 1"),
        ));
    }
    if matches!(ty, Type::Tensor(_, dims) if dims.iter().try_fold(1u64, |a, &d| a.checked_mul(u64::from(d))).is_none())
    {
        return Err(Diagnostic::error(
            "VG-T0118",
            span,
            format!("tensor element count overflows u64 in {ty}"),
        ));
    }
    if is_fixed_size(ty) || matches!(ty, Type::Array(..) | Type::Tensor(..)) {
        match abi_size_align(ty) {
            Some((sz, _)) if sz <= MAX_STATIC_BYTES => Ok(()),
            _ => Err(Diagnostic::error(
                "VG-T0118",
                span,
                format!("static type {ty} exceeds the 16 MiB static limit"),
            )),
        }
    } else {
        Ok(())
    }
}

fn required_handle_effect_ok(h: Handle, mask: EffectMask) -> Result<(), &'static str> {
    match h {
        Handle::FileIn => (mask.0 & EffectMask::bit("FILE_READ").unwrap() != 0)
            .then_some(())
            .ok_or("FILE_READ"),
        Handle::FileOut => (mask.0 & EffectMask::bit("FILE_WRITE").unwrap() != 0)
            .then_some(())
            .ok_or("FILE_WRITE"),
        Handle::Clock => (mask.0 & EffectMask::bit("CLOCK").unwrap() != 0)
            .then_some(())
            .ok_or("CLOCK"),
        Handle::Socket => {
            let bits = EffectMask::bit("NET_READ").unwrap() | EffectMask::bit("NET_WRITE").unwrap();
            (mask.0 & bits != 0)
                .then_some(())
                .ok_or("NET_READ or NET_WRITE")
        }
        Handle::GpuQueue => (mask.0 & EffectMask::bit("GPU").unwrap() != 0)
            .then_some(())
            .ok_or("GPU"),
    }
}

fn is_valid_kernel_label(info: &LabelInfo) -> bool {
    info.kind == ItemKind::Node
        && info.effects.0 == 0
        && info.fn_type.results.len() == 1
        && info.fn_type.params.iter().all(is_fixed_size)
        && is_fixed_size(&info.fn_type.results[0])
}

fn is_valid_fold_spec_kernel(info: &LabelInfo) -> bool {
    is_valid_kernel_label(info)
        && info.fn_type.params.len() >= 2
        && info.fn_type.params[0] == info.fn_type.results[0]
}

fn type_to_ast(ty: &Type) -> ast::TypeExpr {
    match ty {
        Type::Bool => ast::TypeExpr::Bool,
        Type::Eff => ast::TypeExpr::Eff,
        Type::Handle(h) => ast::TypeExpr::Handle(*h),
        Type::Array(e, n) => ast::TypeExpr::Array(
            Box::new(type_to_ast(e)),
            ast::Shape {
                bracketed: false,
                dims: vec![ast::Dim::Int(n.to_string())],
            },
        ),
        Type::Tensor(e, dims) => ast::TypeExpr::Tensor(
            Box::new(type_to_ast(e)),
            ast::Shape {
                bracketed: dims.len() != 1,
                dims: dims.iter().map(|d| ast::Dim::Int(d.to_string())).collect(),
            },
        ),
        Type::Slice(e) => ast::TypeExpr::Slice(Box::new(type_to_ast(e))),
        Type::Tuple(ms) => ast::TypeExpr::Tuple(ms.iter().map(type_to_ast).collect()),
        Type::Result(a, b) => {
            ast::TypeExpr::Result(Box::new(type_to_ast(a)), Box::new(type_to_ast(b)))
        }
        Type::Arena(n) => ast::TypeExpr::Arena(n.to_string()),
        t => ast::TypeExpr::Scalar(t.scalar_name().expect("scalar name")),
    }
}

fn empty_allowed() -> Allowed {
    Allowed {
        token_classes: Vec::new(),
        ports: Vec::new(),
        labels: Vec::new(),
        opcodes: Vec::new(),
    }
}

fn kw_allowed(kws: &[&'static str], puncts: &[&'static str]) -> Allowed {
    let mut token_classes = Vec::with_capacity(kws.len() + puncts.len());
    for &k in kws {
        token_classes.push(TokenClass::Keyword(k));
    }
    for &p in puncts {
        token_classes.push(TokenClass::Punct(p));
    }
    token_classes.sort();
    token_classes.dedup();
    Allowed {
        token_classes,
        ports: Vec::new(),
        labels: Vec::new(),
        opcodes: Vec::new(),
    }
}

fn handle_from_kw(k: &str) -> Option<Handle> {
    match k {
        "FileIn" => Some(Handle::FileIn),
        "FileOut" => Some(Handle::FileOut),
        "Socket" => Some(Handle::Socket),
        "Clock" => Some(Handle::Clock),
        "GpuQueue" => Some(Handle::GpuQueue),
        _ => None,
    }
}

fn is_lower_keyword(k: &str) -> bool {
    LOWER_KEYWORDS.contains(&k)
}

fn fty_of(ty: &Type) -> Result<FTy, &'static str> {
    match ty {
        Type::Bool => Ok(FTy::Bool),
        Type::UInt(_) | Type::SInt(_) | Type::Int => Ok(FTy::Int),
        Type::Float(_) | Type::BFloat16 => Ok(FTy::Float(ty.clone())),
        Type::Eff | Type::Handle(_) => Err("VG-T0119"),
        Type::Arena(_) => Err("VG-S0104"),
        _ if contains_eff_or_handle(ty) => Err("VG-T0119"),
        _ => Ok(FTy::Val(ty.clone())),
    }
}

fn unify_fty(a: &FTy, b: &FTy) -> Result<FTy, (&'static str, String)> {
    match (a, b) {
        (FTy::AnyInvAcc, x) | (x, FTy::AnyInvAcc) => Ok(x.clone()),
        (FTy::Num(x), FTy::Num(y)) => Ok(FTy::Num(*x || *y)),
        (FTy::Num(true), FTy::Int) | (FTy::Int, FTy::Num(true)) => {
            Err(("VG-C0102", "decimal number meets an integer term".into()))
        }
        (FTy::Num(false), FTy::Int) | (FTy::Int, FTy::Num(false)) | (FTy::Int, FTy::Int) => {
            Ok(FTy::Int)
        }
        (FTy::Num(_), FTy::Float(t)) | (FTy::Float(t), FTy::Num(_)) => Ok(FTy::Float(t.clone())),
        (FTy::Float(x), FTy::Float(y)) if x == y => Ok(FTy::Float(x.clone())),
        _ => Err(("VG-C0101", format!("{a:?} meets {b:?}"))),
    }
}

fn eq_fty_ok(a: &FTy, b: &FTy) -> Result<(), (&'static str, String)> {
    match (a, b) {
        (FTy::AnyInvAcc, _) | (_, FTy::AnyInvAcc) => Ok(()),
        (FTy::Bool, FTy::Bool) => Ok(()),
        (FTy::Val(x), FTy::Val(y)) if x == y => Ok(()),
        _ => {
            let u = unify_fty(a, b)?;
            settle_fty(u).map(|_| ())
        }
    }
}

fn settle_fty(t: FTy) -> Result<FTy, (&'static str, String)> {
    match t {
        FTy::Num(true) => Err(("VG-C0102", "decimal number meets no float term".into())),
        FTy::Num(false) => Ok(FTy::Int),
        other => Ok(other),
    }
}

fn formula_arg_fits(arg: &FTy, param: &FTy) -> bool {
    match (arg, param) {
        (FTy::AnyInvAcc, _) | (_, FTy::AnyInvAcc) => true,
        (FTy::Num(_), FTy::Float(_)) | (FTy::Num(false), FTy::Int) => true,
        (FTy::Val(Type::Array(e, _) | Type::Slice(e)), FTy::Val(Type::Slice(pe))) => e == pe,
        (a, b) => a == b,
    }
}

fn binop_of(tok: &Tok) -> Option<BinOp> {
    Some(match tok {
        Tok::Punct("==>") => BinOp::Implies,
        Tok::Punct("||") => BinOp::Or,
        Tok::Punct("&&") => BinOp::And,
        Tok::Punct("==") => BinOp::Eq,
        Tok::Punct("!=") => BinOp::Ne,
        Tok::Punct("<") => BinOp::Lt,
        Tok::Punct("<=") => BinOp::Le,
        Tok::Punct(">") => BinOp::Gt,
        Tok::Punct(">=") => BinOp::Ge,
        Tok::Punct("+") => BinOp::Add,
        Tok::Punct("-") => BinOp::Sub,
        Tok::Punct("*") => BinOp::Mul,
        Tok::Punct("/") => BinOp::Div,
        Tok::Punct("%") => BinOp::Mod,
        _ => return None,
    })
}

fn combine_binop(op: BinOp, a: &FTy, b: &FTy) -> Result<FTy, (&'static str, String)> {
    match op {
        BinOp::And | BinOp::Or | BinOp::Implies => {
            if (*a == FTy::Bool || *a == FTy::AnyInvAcc)
                && (*b == FTy::Bool || *b == FTy::AnyInvAcc)
            {
                Ok(FTy::Bool)
            } else {
                Err((
                    "VG-C0101",
                    format!("{} requires Bool operands", op.symbol()),
                ))
            }
        }
        BinOp::Eq | BinOp::Ne => {
            eq_fty_ok(a, b)?;
            Ok(FTy::Bool)
        }
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            let u = unify_fty(a, b)?;
            settle_fty(u)?;
            Ok(FTy::Bool)
        }
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => unify_fty(a, b),
        BinOp::Mod => {
            let u = unify_fty(a, b)?;
            match u {
                FTy::Int | FTy::Num(false) | FTy::AnyInvAcc => Ok(u),
                _ => Err(("VG-C0101", "`%` requires integer operands".into())),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Step dispatcher + Type parser
// ---------------------------------------------------------------------------

impl Prefix {
    fn step(&mut self, tok: &Tok, raw: &str) -> Result<(), Diagnostic> {
        if !self.type_stack.is_empty() {
            return self.step_type(tok, raw);
        }
        if !self.formula_stack.is_empty() {
            return self.step_formula(tok, raw);
        }
        self.step_main(tok, raw)
    }

    fn kw_or_ident_err(&self, tok: &Tok, raw: &str, expected: &str) -> Diagnostic {
        if let Tok::Kw(k) = tok
            && expected.contains("name")
            && is_lower_keyword(k)
        {
            return self.err(
                "VG-P0004",
                raw,
                format!("reserved word `{k}` cannot be used as a name"),
            );
        }
        self.err(
            "VG-P0001",
            raw,
            format!("expected {expected}, found `{raw}`"),
        )
    }

    fn push_type(&mut self, allow_refine: bool, cont: TypeCont) {
        self.type_stack
            .push(TypeParse::Start { allow_refine, cont });
    }

    fn finish_type(&mut self, ty: Type, cont: TypeCont, raw: &str) -> Result<(), Diagnostic> {
        let span = self.span(raw);
        let allow_top = matches!(
            cont,
            TypeCont::QueryParam
                | TypeCont::QueryResult
                | TypeCont::NodeParam { .. }
                | TypeCont::NodeResult
                | TypeCont::Binding { .. }
        );
        validate_type(&ty, allow_top, 1, span)?;
        match cont {
            TypeCont::QueryParam => {
                self.query_params.push(ty);
                self.state = State::QuerySigCommaOrClose;
            }
            TypeCont::QueryResult => {
                self.query_results.push(ty);
                self.state = State::QueryResCommaOrClose;
            }
            TypeCont::NodeParam { name } => {
                let n = self.node.as_mut().unwrap();
                if n.inputs.iter().any(|(m, _)| m == &name) {
                    return Err(self.err(
                        "VG-S0001",
                        raw,
                        format!("parameter `{name}` declared twice"),
                    ));
                }
                let idx = n.params.len();
                if ty == Type::Eff && idx != 0 {
                    return Err(self.err(
                        "VG-E0103",
                        raw,
                        format!("Eff parameter may only appear at parameter 0, not {idx}"),
                    ));
                }
                n.params.push((name.clone(), ty.clone()));
                n.inputs.push((name.clone(), ty.clone()));
                let mut from_params = vec![false; idx + 1];
                from_params[idx] = true;
                for m in n.values.values_mut() {
                    m.from_params.resize(idx + 1, false);
                }
                let valid_handle = match ty {
                    Type::Handle(h) => Some(h),
                    _ => None,
                };
                if ty == Type::Eff {
                    n.eff_live.insert(name.clone());
                }
                n.values.insert(
                    name,
                    ValMeta {
                        ty,
                        origin_local_frame: false,
                        has_local_slice: false,
                        owns_fresh_arena_slice: false,
                        from_params,
                        valid_handle,
                        const_u64: None,
                        is_literal: false,
                    },
                );
                self.state = State::SigParamCommaOrClose;
            }
            TypeCont::NodeResult => {
                let n = self.node.as_mut().unwrap();
                let idx = n.results.len();
                if ty == Type::Eff && idx != 0 {
                    return Err(self.err(
                        "VG-E0103",
                        raw,
                        format!("Eff result may only appear at result 0, not {idx}"),
                    ));
                }
                n.results.push(ty);
                self.state = State::SigResCommaOrClose;
            }
            TypeCont::Binding { port } => {
                let State::BindAfterColon { mut binds, .. } = self.state.clone() else {
                    unreachable!()
                };
                binds.push(StmtBind {
                    port,
                    ann: Some(ty),
                });
                self.state = State::BindCommaOrEq { binds };
            }
            TypeCont::ArrayLit { elems } => {
                let lit = ast::Literal::Array(elems, type_to_ast(&ty));
                let c = literal_const(&lit, span)?;
                let n = self.node.as_ref().unwrap();
                let meta = ValMeta {
                    ty: c.ty,
                    origin_local_frame: false,
                    has_local_slice: false,
                    owns_fresh_arena_slice: false,
                    from_params: vec![false; n.params.len()],
                    valid_handle: None,
                    const_u64: None,
                    is_literal: true,
                };
                match self.state.clone() {
                    State::OpArrayColon { call, .. } => {
                        let call = self.accept_stmt_val(call, meta, raw)?;
                        self.state = State::OpArgCommaOrClose { call };
                    }
                    State::ReturnArrayColon { idx, .. } => {
                        self.accept_return_val(idx, meta, None, raw)?;
                        self.state = State::ReturnCommaOrClose { idx };
                    }
                    _ => unreachable!(),
                }
            }
            TypeCont::FormulaArrayLit { elems } => {
                let lit = ast::Literal::Array(elems, type_to_ast(&ty));
                let c = literal_const(&lit, span)?;
                let fty =
                    fty_of(&c.ty).map_err(|code| self.err(code, raw, "invalid array type"))?;
                let Some(FormulaState::Frame(mut f)) = self.formula_stack.pop() else {
                    unreachable!()
                };
                f.expect_atom = false;
                f.cur = Some((fty, false));
                self.formula_stack.push(FormulaState::Frame(f));
            }
            TypeCont::RefineBase { .. }
            | TypeCont::ArrayElem
            | TypeCont::TensorElem
            | TypeCont::SliceElem
            | TypeCont::TupleElem { .. }
            | TypeCont::ResultOk
            | TypeCont::ResultErr { .. } => unreachable!(),
        }
        Ok(())
    }

    fn complete_type(&mut self, ty: Type, cont: TypeCont, raw: &str) -> Result<(), Diagnostic> {
        let span = self.span(raw);
        match cont {
            TypeCont::RefineBase { var } => {
                validate_type(&ty, false, 1, span)?;
                let Some(TypeParse::RefineClose {
                    cont: outer_cont, ..
                }) = self.type_stack.pop()
                else {
                    unreachable!()
                };
                self.type_stack.push(TypeParse::RefineBar {
                    var,
                    base: ty,
                    cont: outer_cont,
                });
                Ok(())
            }
            TypeCont::ArrayElem => {
                let Some(TypeParse::ArrayComma {
                    cont: outer_cont, ..
                }) = self.type_stack.pop()
                else {
                    unreachable!()
                };
                self.type_stack.push(TypeParse::ArrayComma {
                    elem: ty,
                    cont: outer_cont,
                });
                Ok(())
            }
            TypeCont::TensorElem => {
                let Some(TypeParse::TensorComma {
                    cont: outer_cont, ..
                }) = self.type_stack.pop()
                else {
                    unreachable!()
                };
                self.type_stack.push(TypeParse::TensorComma {
                    elem: ty,
                    cont: outer_cont,
                });
                Ok(())
            }
            TypeCont::SliceElem => {
                let Some(TypeParse::CloseAngle {
                    cont: outer_cont, ..
                }) = self.type_stack.pop()
                else {
                    unreachable!()
                };
                self.type_stack.push(TypeParse::CloseAngle {
                    ty: Type::Slice(Box::new(ty)),
                    cont: outer_cont,
                });
                Ok(())
            }
            TypeCont::TupleElem { mut elems } => {
                elems.push(ty);
                let Some(TypeParse::CloseAngle {
                    cont: outer_cont, ..
                }) = self.type_stack.pop()
                else {
                    unreachable!()
                };
                self.type_stack.push(TypeParse::TupleCommaOrClose {
                    elems,
                    cont: outer_cont,
                });
                Ok(())
            }
            TypeCont::ResultOk => {
                let Some(TypeParse::ResultComma {
                    cont: outer_cont, ..
                }) = self.type_stack.pop()
                else {
                    unreachable!()
                };
                self.type_stack.push(TypeParse::ResultComma {
                    ok: ty,
                    cont: outer_cont,
                });
                Ok(())
            }
            TypeCont::ResultErr { ok } => {
                let Some(TypeParse::CloseAngle {
                    cont: outer_cont, ..
                }) = self.type_stack.pop()
                else {
                    unreachable!()
                };
                self.type_stack.push(TypeParse::CloseAngle {
                    ty: Type::Result(Box::new(ok), Box::new(ty)),
                    cont: outer_cont,
                });
                Ok(())
            }
            other => self.finish_type(ty, other, raw),
        }
    }

    fn step_type(&mut self, tok: &Tok, raw: &str) -> Result<(), Diagnostic> {
        let top = self.type_stack.pop().unwrap();
        let depth = self.type_stack.len() + 1;
        if depth > MAX_TYPE_DEPTH {
            return Err(self.err(
                "VG-T0118",
                raw,
                format!("type nested deeper than {MAX_TYPE_DEPTH} levels"),
            ));
        }
        match top {
            TypeParse::Start { allow_refine, cont } => match tok {
                Tok::Kw(k) if SCALARS.contains(k) => {
                    let ty = Type::scalar(k).unwrap();
                    self.complete_type(ty, cont, raw)
                }
                Tok::Kw("Bool") => self.complete_type(Type::Bool, cont, raw),
                Tok::Kw("Eff") => {
                    if !matches!(
                        cont,
                        TypeCont::QueryParam
                            | TypeCont::QueryResult
                            | TypeCont::NodeParam { .. }
                            | TypeCont::NodeResult
                            | TypeCont::Binding { .. }
                    ) {
                        return Err(self.err(
                            "VG-T0119",
                            raw,
                            "Eff is not allowed inside a composite type",
                        ));
                    }
                    self.complete_type(Type::Eff, cont, raw)
                }
                Tok::Kw(k) if HANDLES.contains(k) => {
                    if !matches!(
                        cont,
                        TypeCont::QueryParam
                            | TypeCont::QueryResult
                            | TypeCont::NodeParam { .. }
                            | TypeCont::NodeResult
                            | TypeCont::Binding { .. }
                    ) {
                        return Err(self.err(
                            "VG-T0119",
                            raw,
                            format!("{k} is not allowed inside a composite type"),
                        ));
                    }
                    self.complete_type(Type::Handle(handle_from_kw(k).unwrap()), cont, raw)
                }
                Tok::Kw(k @ ("Array" | "Tensor" | "Slice" | "Tuple" | "Result")) => {
                    self.type_stack.push(TypeParse::OpenAngle { kw: k, cont });
                    Ok(())
                }
                Tok::Kw("Arena") => Err(self.err(
                    "VG-S0104",
                    raw,
                    "Arena<N> is [ALPHA] and not available in the MVP",
                )),
                Tok::Punct("{") => {
                    if !allow_refine {
                        return Err(self.err(
                            "VG-T0116",
                            raw,
                            "a refinement type is allowed only at the top of a SIGNATURE",
                        ));
                    }
                    self.type_stack.push(TypeParse::RefineVar { cont });
                    Ok(())
                }
                _ => Err(self.err("VG-P0001", raw, format!("expected a type, found `{raw}`"))),
            },
            TypeParse::RefineVar { cont } => match tok {
                Tok::Ident(v) => {
                    self.type_stack.push(TypeParse::RefineColon {
                        var: v.clone(),
                        cont,
                    });
                    Ok(())
                }
                _ => Err(self.kw_or_ident_err(tok, raw, "a refinement variable name")),
            },
            TypeParse::RefineColon { var, cont } => {
                if *tok == Tok::Punct(":") {
                    self.type_stack.push(TypeParse::RefineClose {
                        base: Type::Bool,
                        cont,
                    });
                    self.type_stack.push(TypeParse::Start {
                        allow_refine: false,
                        cont: TypeCont::RefineBase { var },
                    });
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `:`, found `{raw}`")))
                }
            }
            TypeParse::RefineBar { var, base, cont } => {
                if *tok == Tok::Punct("|") {
                    let kind = match cont {
                        TypeCont::NodeParam { .. } => ClauseKind::Pre,
                        TypeCont::NodeResult => ClauseKind::Post,
                        _ => unreachable!(),
                    };
                    let mut bound = Vec::new();
                    if let Some(n) = &self.node {
                        for (pn, pt) in &n.params {
                            if let Ok(ft) = fty_of(pt) {
                                bound.push((pn.clone(), ft));
                            }
                        }
                    }
                    if let Ok(ft) = fty_of(&base) {
                        bound.push((var.clone(), ft));
                    }
                    self.formula_ctx = Some(FormulaCtx {
                        clause: kind,
                        bound,
                        inv_label: None,
                        allow_ports: false,
                        allow_result: kind == ClauseKind::Post,
                        allow_resources: false,
                    });
                    self.formula_stack.push(FormulaState::Frame(FormulaFrame {
                        unaries: Vec::new(),
                        ops: Vec::new(),
                        expect_atom: true,
                        cur: None,
                        min_allowed_prec: 1,
                        cont: FormulaCont::Refine { base, cont },
                    }));
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `|`, found `{raw}`")))
                }
            }
            TypeParse::RefineClose { base, cont } => {
                if *tok == Tok::Punct("}") {
                    self.complete_type(base, cont, raw)
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `}}`, found `{raw}`")))
                }
            }
            TypeParse::OpenAngle { kw, cont } => {
                if *tok != Tok::Punct("<") {
                    return Err(self.err("VG-P0001", raw, format!("expected `<`, found `{raw}`")));
                }
                match kw {
                    "Array" => {
                        self.type_stack.push(TypeParse::ArrayComma {
                            elem: Type::Bool,
                            cont,
                        });
                        self.push_type(false, TypeCont::ArrayElem);
                    }
                    "Tensor" => {
                        self.type_stack.push(TypeParse::TensorComma {
                            elem: Type::Bool,
                            cont,
                        });
                        self.push_type(false, TypeCont::TensorElem);
                    }
                    "Slice" => {
                        self.type_stack.push(TypeParse::CloseAngle {
                            ty: Type::Bool,
                            cont,
                        });
                        self.push_type(false, TypeCont::SliceElem);
                    }
                    "Tuple" => {
                        self.type_stack.push(TypeParse::CloseAngle {
                            ty: Type::Bool,
                            cont,
                        });
                        self.push_type(false, TypeCont::TupleElem { elems: Vec::new() });
                    }
                    "Result" => {
                        self.type_stack.push(TypeParse::ResultComma {
                            ok: Type::Bool,
                            cont,
                        });
                        self.push_type(false, TypeCont::ResultOk);
                    }
                    _ => unreachable!(),
                }
                Ok(())
            }
            TypeParse::ArrayComma { elem, cont } => {
                if *tok == Tok::Punct(",") {
                    self.type_stack.push(TypeParse::ShapeDim {
                        kw: "Array",
                        elem,
                        bracketed: false,
                        dims: Vec::new(),
                        cont,
                    });
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `,`, found `{raw}`")))
                }
            }
            TypeParse::TensorComma { elem, cont } => {
                if *tok == Tok::Punct(",") {
                    self.type_stack.push(TypeParse::ShapeDim {
                        kw: "Tensor",
                        elem,
                        bracketed: false,
                        dims: Vec::new(),
                        cont,
                    });
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `,`, found `{raw}`")))
                }
            }
            TypeParse::ShapeDim {
                kw,
                elem,
                bracketed,
                mut dims,
                cont,
            } => match tok {
                Tok::Punct("[") if !bracketed && dims.is_empty() => {
                    self.type_stack.push(TypeParse::ShapeDim {
                        kw,
                        elem,
                        bracketed: true,
                        dims,
                        cont,
                    });
                    Ok(())
                }
                Tok::Int(s) => {
                    let d = s.parse::<u32>().map_err(|_| {
                        self.err("VG-T0118", raw, format!("dimension `{s}` exceeds 2^32 - 1"))
                    })?;
                    dims.push(d);
                    if bracketed {
                        self.type_stack.push(TypeParse::ShapeCommaOrClose {
                            kw,
                            elem,
                            dims,
                            cont,
                        });
                    } else {
                        if kw == "Array" && dims.len() != 1 {
                            return Err(self.err("VG-T0101", raw, "Array takes one length"));
                        }
                        let ty = if kw == "Array" {
                            Type::Array(Box::new(elem), dims[0])
                        } else {
                            Type::Tensor(Box::new(elem), dims)
                        };
                        check_static_size(&ty, self.span(raw))?;
                        self.type_stack.push(TypeParse::CloseAngle { ty, cont });
                    }
                    Ok(())
                }
                Tok::Ident(s) => Err(self.err(
                    "VG-S0104",
                    raw,
                    format!("symbolic dimension `{s}` is [ALPHA]; MVP requires an integer"),
                )),
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected dimension, found `{raw}`"),
                )),
            },
            TypeParse::ShapeCommaOrClose {
                kw,
                elem,
                dims,
                cont,
            } => match tok {
                Tok::Punct(",") => {
                    if kw == "Array" {
                        return Err(self.err("VG-T0101", raw, "Array takes a single dimension"));
                    }
                    self.type_stack.push(TypeParse::ShapeDim {
                        kw,
                        elem,
                        bracketed: true,
                        dims,
                        cont,
                    });
                    Ok(())
                }
                Tok::Punct("]") => {
                    if kw == "Array" && dims.len() != 1 {
                        return Err(self.err("VG-T0101", raw, "Array takes a single dimension"));
                    }
                    let ty = if kw == "Array" {
                        Type::Array(Box::new(elem), dims[0])
                    } else {
                        Type::Tensor(Box::new(elem), dims)
                    };
                    check_static_size(&ty, self.span(raw))?;
                    self.type_stack.push(TypeParse::CloseAngle { ty, cont });
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `]`, found `{raw}`"),
                )),
            },
            TypeParse::CloseAngle { ty, cont } => {
                if *tok == Tok::Punct(">") {
                    self.complete_type(ty, cont, raw)
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `>`, found `{raw}`")))
                }
            }
            TypeParse::TupleCommaOrClose { elems, cont } => match tok {
                Tok::Punct(",") => {
                    self.type_stack.push(TypeParse::CloseAngle {
                        ty: Type::Bool,
                        cont,
                    });
                    self.push_type(false, TypeCont::TupleElem { elems });
                    Ok(())
                }
                Tok::Punct(">") => {
                    let ty = Type::Tuple(elems);
                    check_static_size(&ty, self.span(raw))?;
                    self.complete_type(ty, cont, raw)
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `>`, found `{raw}`"),
                )),
            },
            TypeParse::ResultComma { ok, cont } => {
                if *tok == Tok::Punct(",") {
                    self.type_stack.push(TypeParse::CloseAngle {
                        ty: Type::Bool,
                        cont,
                    });
                    self.push_type(false, TypeCont::ResultErr { ok });
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `,`, found `{raw}`")))
                }
            }
        }
    }

    fn allowed_type(&self, top: &TypeParse) -> Allowed {
        match top {
            TypeParse::Start { allow_refine, cont } => {
                let mut kws = Vec::new();
                kws.extend_from_slice(SCALARS);
                kws.push("Bool");
                kws.extend_from_slice(&["Array", "Tensor", "Slice", "Tuple", "Result"]);
                let allow_handles = matches!(
                    cont,
                    TypeCont::QueryParam
                        | TypeCont::QueryResult
                        | TypeCont::NodeParam { .. }
                        | TypeCont::NodeResult
                        | TypeCont::Binding { .. }
                );
                let allow_eff = match cont {
                    TypeCont::QueryParam | TypeCont::QueryResult | TypeCont::Binding { .. } => true,
                    TypeCont::NodeParam { .. } => {
                        self.node.as_ref().is_some_and(|n| n.params.is_empty())
                    }
                    TypeCont::NodeResult => {
                        self.node.as_ref().is_some_and(|n| n.results.is_empty())
                    }
                    _ => false,
                };
                if allow_eff {
                    kws.push("Eff");
                }
                if allow_handles {
                    kws.extend_from_slice(HANDLES);
                }
                let puncts: &[&str] = if *allow_refine { &["{"] } else { &[] };
                kw_allowed(&kws, puncts)
            }
            TypeParse::RefineVar { .. } => Allowed {
                token_classes: vec![TokenClass::Ident],
                ports: Vec::new(),
                labels: Vec::new(),
                opcodes: Vec::new(),
            },
            TypeParse::RefineColon { .. } => kw_allowed(&[], &[":"]),
            TypeParse::RefineBar { .. } => kw_allowed(&[], &["|"]),
            TypeParse::RefineClose { .. } => kw_allowed(&[], &["}"]),
            TypeParse::OpenAngle { .. } => kw_allowed(&[], &["<"]),
            TypeParse::ArrayComma { .. }
            | TypeParse::TensorComma { .. }
            | TypeParse::ResultComma { .. } => kw_allowed(&[], &[","]),
            TypeParse::ShapeDim {
                bracketed, dims, ..
            } => {
                let mut tc = vec![TokenClass::IntLiteral];
                if !bracketed && dims.is_empty() {
                    tc.push(TokenClass::Punct("["));
                }
                Allowed {
                    token_classes: tc,
                    ports: Vec::new(),
                    labels: Vec::new(),
                    opcodes: Vec::new(),
                }
            }
            TypeParse::ShapeCommaOrClose { kw, elem, dims, .. } => {
                let candidate = if *kw == "Array" {
                    Type::Array(Box::new(elem.clone()), dims.first().copied().unwrap_or(0))
                } else {
                    Type::Tensor(Box::new(elem.clone()), dims.clone())
                };
                let can_close = (*kw != "Array" || dims.len() == 1)
                    && check_static_size(&candidate, Span::default()).is_ok();
                match (*kw == "Array", can_close) {
                    (true, true) => kw_allowed(&[], &["]"]),
                    (true, false) => empty_allowed(),
                    (false, true) => kw_allowed(&[], &[",", "]"]),
                    (false, false) => kw_allowed(&[], &[","]),
                }
            }
            TypeParse::CloseAngle { .. } => kw_allowed(&[], &[">"]),
            TypeParse::TupleCommaOrClose { .. } => kw_allowed(&[], &[",", ">"]),
        }
    }

    // -----------------------------------------------------------------------
    // Incremental formula parser & type checker
    // -----------------------------------------------------------------------

    fn start_formula(&mut self, ctx: FormulaCtx, cont: FormulaCont) {
        self.formula_ctx = Some(ctx);
        self.formula_stack.push(FormulaState::Frame(FormulaFrame {
            unaries: Vec::new(),
            ops: Vec::new(),
            expect_atom: true,
            cur: None,
            min_allowed_prec: 1,
            cont,
        }));
    }

    fn formula_lookup_ident(&self, name: &str) -> Result<FTy, (&'static str, String)> {
        let ctx = self.formula_ctx.as_ref().unwrap();
        if let Some((_, ty)) = ctx.bound.iter().rev().find(|(n, _)| n == name) {
            return Ok(ty.clone());
        }
        if ctx.allow_resources && RESOURCES.contains(&name) {
            if ctx.clause == ClauseKind::Cost && name == "latency_p99_us" {
                return Err((
                    "VG-C0106",
                    "latency_p99_us is a measured resource for BUDGET, not COST".into(),
                ));
            }
            return Ok(FTy::Int);
        }
        if let Some(n) = &self.node {
            if let Some((_, ty)) = n.inputs.iter().find(|(m, _)| m == name) {
                return fty_of(ty).map_err(|c| (c, format!("{ty} cannot appear in formula")));
            }
        } else if (ctx.clause == ClauseKind::Pre || ctx.clause == ClauseKind::Post)
            && let Some(rest) = name.strip_prefix("arg")
            && let Ok(idx) = rest.parse::<usize>()
            && let Some(ty) = self.query_params.get(idx)
        {
            return fty_of(ty).map_err(|c| (c, format!("{ty} cannot appear in formula")));
        }
        if ctx.allow_resources {
            Err(("VG-C0106", format!("unknown name or resource `{name}`")))
        } else {
            Err(("VG-S0003", format!("unknown name `{name}`")))
        }
    }

    fn formula_result_type(&self, proj: Option<usize>) -> Result<FTy, (&'static str, String)> {
        let ctx = self.formula_ctx.as_ref().unwrap();
        if !ctx.allow_result {
            return Err(("VG-C0104", "`result` is allowed only in POST".into()));
        }
        let results = if let Some(n) = &self.node {
            &n.results
        } else {
            &self.query_results
        };
        match proj {
            None => {
                if results.len() == 1 {
                    fty_of(&results[0]).map_err(|c| (c, format!("{} in formula", results[0])))
                } else {
                    Err((
                        "VG-C0104",
                        format!(
                            "node has {} results; write result.0 .. result.{}",
                            results.len(),
                            results.len() - 1
                        ),
                    ))
                }
            }
            // docs/13: `result.j` is the j-th result only when there are several; the one result
            // of a node that has one is `result`, and `.j` projects a member of its tuple.
            Some(j) => match (results.as_slice(), results.get(j)) {
                ([Type::Tuple(ms)], _) => match ms.get(j) {
                    Some(t) => fty_of(t).map_err(|c| (c, format!("{t} in formula"))),
                    None => Err(("VG-C0104", format!("result.{j} out of range"))),
                },
                ([_], _) | (_, None) => Err(("VG-C0104", format!("result.{j} out of range"))),
                (_, Some(t)) => fty_of(t).map_err(|c| (c, format!("{t} in formula"))),
            },
        }
    }

    fn resolve_formula_callee(
        &self,
        prefix: Option<CallPrefix>,
        name: &str,
    ) -> Result<FormulaCallee, (&'static str, String)> {
        if prefix.is_none() && BUILTINS.contains(&name) {
            return Ok(FormulaCallee::Builtin(
                BUILTINS.iter().find(|b| **b == name).unwrap(),
            ));
        }
        if prefix == Some(CallPrefix::Spec)
            && self.node.is_none()
            && !self.query_label.is_empty()
            && name == self.query_label
        {
            return Ok(FormulaCallee::Spec(FnType {
                params: self.query_params.clone(),
                results: self.query_results.clone(),
            }));
        }
        let Some(info) = self.labels.get(name) else {
            return Err(("VG-C0103", format!("unknown function `{name}`")));
        };
        match (prefix, info.kind) {
            (Some(CallPrefix::Spec), ItemKind::Spec | ItemKind::Query) => {
                Ok(FormulaCallee::Spec(info.fn_type.clone()))
            }
            (Some(CallPrefix::Spec), ItemKind::Node) => {
                Err(("VG-C0107", format!("spec:{name} names a NODE, not a SPEC")))
            }
            (Some(CallPrefix::Fold), _) => {
                if !is_valid_fold_spec_kernel(info) {
                    Err((
                        "VG-C0108",
                        format!(
                            "fold:{name} requires a pure fixed-size kernel (A, T, B...) -> (A)"
                        ),
                    ))
                } else {
                    Ok(FormulaCallee::Fold(info.fn_type.clone()))
                }
            }
            (None, ItemKind::Node) => {
                if !is_valid_kernel_label(info) {
                    Err((
                        "VG-C0108",
                        format!(
                            "formula kernel application `{name}` requires a pure fixed-size single-result NODE"
                        ),
                    ))
                } else {
                    Ok(FormulaCallee::Node(info.fn_type.clone()))
                }
            }
            (None, _) => Err(("VG-C0108", format!("`{name}` is not a NODE kernel"))),
        }
    }

    fn check_formula_call_arg(
        &self,
        callee: &FormulaCallee,
        arg_idx: usize,
        arg_ty: &FTy,
        raw: &str,
    ) -> Result<(), Diagnostic> {
        let ok = match callee {
            FormulaCallee::Builtin(b) => match (*b, arg_idx, arg_ty) {
                ("len", 0, FTy::Val(t)) => t.elem().is_some(),
                ("len", 0, FTy::AnyInvAcc) => true,
                ("is_finite" | "is_nan", 0, FTy::Float(_) | FTy::AnyInvAcc) => true,
                ("abs", 0, FTy::Int | FTy::Float(_) | FTy::Num(_) | FTy::AnyInvAcc) => true,
                ("min" | "max", 0 | 1, FTy::Int | FTy::Float(_) | FTy::Num(_) | FTy::AnyInvAcc) => {
                    true
                }
                ("as_bytes", 0, FTy::Bool | FTy::Int | FTy::Float(_) | FTy::AnyInvAcc) => true,
                ("as_bytes", 0, FTy::Val(v)) => has_byte_form(v),
                ("same", 0 | 1, _) => true,
                ("is_ok" | "ok" | "err", 0, FTy::Val(Type::Result(..)) | FTy::AnyInvAcc) => true,
                _ => false,
            },
            FormulaCallee::Node(ft) | FormulaCallee::Spec(ft) => {
                let Some(pt) = ft.params.get(arg_idx) else {
                    return Err(self.err("VG-C0103", raw, "too many arguments in formula call"));
                };
                let p_fty = fty_of(pt).map_err(|c| self.err(c, raw, "invalid param type"))?;
                formula_arg_fits(arg_ty, &p_fty)
            }
            FormulaCallee::Fold(ft) => {
                let want = match arg_idx {
                    0 => {
                        fty_of(&ft.params[0]).map_err(|c| self.err(c, raw, "invalid init type"))?
                    }
                    1 => FTy::Val(Type::Slice(Box::new(ft.params[1].clone()))),
                    2 => FTy::Int,
                    k => {
                        let Some(pt) = ft.params.get(k - 1) else {
                            return Err(self.err(
                                "VG-C0103",
                                raw,
                                "too many arguments in fold call",
                            ));
                        };
                        fty_of(pt).map_err(|c| self.err(c, raw, "invalid broadcast type"))?
                    }
                };
                formula_arg_fits(arg_ty, &want)
            }
        };
        if !ok {
            return Err(self.err(
                "VG-C0101",
                raw,
                format!("formula call argument {arg_idx} has incompatible type {arg_ty:?}"),
            ));
        }
        Ok(())
    }

    fn finish_formula_call(
        &self,
        callee: &FormulaCallee,
        arg_tys: &[FTy],
        raw: &str,
    ) -> Result<FTy, Diagnostic> {
        let n_args = arg_tys.len();
        match callee {
            FormulaCallee::Builtin(b) => {
                let expected = match *b {
                    "min" | "max" | "same" => 2,
                    _ => 1,
                };
                if n_args != expected {
                    return Err(self.err(
                        "VG-C0103",
                        raw,
                        format!("builtin `{b}` expects {expected} arguments, got {n_args}"),
                    ));
                }
                match *b {
                    "len" => Ok(FTy::Int),
                    "is_finite" | "is_nan" | "is_ok" => Ok(FTy::Bool),
                    "same" => {
                        eq_fty_ok(&arg_tys[0], &arg_tys[1])
                            .map_err(|(c, m)| self.err(c, raw, m))?;
                        Ok(FTy::Bool)
                    }
                    "as_bytes" => Ok(FTy::Val(Type::Slice(Box::new(Type::UInt(8))))),
                    "abs" => Ok(arg_tys[0].clone()),
                    "min" | "max" => {
                        unify_fty(&arg_tys[0], &arg_tys[1]).map_err(|(c, m)| self.err(c, raw, m))
                    }
                    "ok" => match &arg_tys[0] {
                        FTy::Val(Type::Result(ok, _)) => {
                            fty_of(ok).map_err(|c| self.err(c, raw, "invalid Result ok type"))
                        }
                        FTy::AnyInvAcc => Ok(FTy::AnyInvAcc),
                        _ => Err(self.err("VG-C0101", raw, "ok() requires Result")),
                    },
                    "err" => match &arg_tys[0] {
                        FTy::Val(Type::Result(_, e)) => {
                            fty_of(e).map_err(|c| self.err(c, raw, "invalid Result err type"))
                        }
                        FTy::AnyInvAcc => Ok(FTy::AnyInvAcc),
                        _ => Err(self.err("VG-C0101", raw, "err() requires Result")),
                    },
                    _ => unreachable!(),
                }
            }
            FormulaCallee::Node(ft) | FormulaCallee::Spec(ft) => {
                if n_args != ft.params.len() {
                    return Err(self.err(
                        "VG-C0103",
                        raw,
                        format!("call expects {} arguments, got {n_args}", ft.params.len()),
                    ));
                }
                if ft.results.len() == 1 {
                    fty_of(&ft.results[0]).map_err(|c| self.err(c, raw, "invalid result type"))
                } else {
                    Ok(FTy::Val(Type::Tuple(ft.results.clone())))
                }
            }
            FormulaCallee::Fold(ft) => {
                if n_args != ft.params.len() + 1 {
                    return Err(self.err(
                        "VG-C0108",
                        raw,
                        format!(
                            "fold expects {} arguments, got {n_args}",
                            ft.params.len() + 1
                        ),
                    ));
                }
                fty_of(&ft.results[0]).map_err(|c| self.err(c, raw, "invalid fold result"))
            }
        }
    }

    fn flush_unaries(&self, frame: &mut FormulaFrame, raw: &str) -> Result<(), Diagnostic> {
        let Some((mut ty, is_res)) = frame.cur.take() else {
            return Ok(());
        };
        while let Some(u) = frame.unaries.pop() {
            match u {
                UnOp::Not => {
                    if ty != FTy::Bool && ty != FTy::AnyInvAcc {
                        return Err(self.err("VG-C0101", raw, "`!` requires Bool"));
                    }
                    ty = FTy::Bool;
                }
                UnOp::Neg => {
                    if !matches!(ty, FTy::Int | FTy::Float(_) | FTy::Num(_) | FTy::AnyInvAcc) {
                        return Err(self.err("VG-C0101", raw, "unary `-` requires number"));
                    }
                }
            }
        }
        frame.cur = Some((ty, is_res));
        Ok(())
    }

    fn complete_formula_frame(
        &mut self,
        mut frame: FormulaFrame,
        tok: &Tok,
        raw: &str,
    ) -> Result<(), Diagnostic> {
        self.flush_unaries(&mut frame, raw)?;
        let (mut cur_ty, _) = frame.cur.take().unwrap();
        while let Some((op, lhs_ty)) = frame.ops.pop() {
            cur_ty = combine_binop(op, &lhs_ty, &cur_ty).map_err(|(c, m)| self.err(c, raw, m))?;
        }
        match frame.cont {
            FormulaCont::QueryPre => {
                let settled = settle_fty(cur_ty).map_err(|(c, m)| self.err(c, raw, m))?;
                if settled != FTy::Bool {
                    return Err(self.err("VG-C0101", raw, "pre formula must be Bool"));
                }
                self.formula_ctx = None;
                self.state = State::QueryPreSemi;
                self.step_main(tok, raw)
            }
            FormulaCont::QueryPost => {
                let settled = settle_fty(cur_ty).map_err(|(c, m)| self.err(c, raw, m))?;
                if settled != FTy::Bool {
                    return Err(self.err("VG-C0101", raw, "post formula must be Bool"));
                }
                self.formula_ctx = None;
                self.state = State::QueryPostSemi;
                self.step_main(tok, raw)
            }
            FormulaCont::Clause { kind, inv_label } => {
                let settled = settle_fty(cur_ty).map_err(|(c, m)| self.err(c, raw, m))?;
                let want = if kind == ClauseKind::Decreases {
                    FTy::Int
                } else {
                    FTy::Bool
                };
                if settled != want && settled != FTy::AnyInvAcc {
                    return Err(self.err(
                        "VG-C0101",
                        raw,
                        format!("clause must be {want:?}, found {settled:?}"),
                    ));
                }
                let _ = inv_label;
                self.formula_ctx = None;
                self.state = State::ClauseSemi;
                self.step_main(tok, raw)
            }
            FormulaCont::Refine { base, cont } => {
                let settled = settle_fty(cur_ty).map_err(|(c, m)| self.err(c, raw, m))?;
                if settled != FTy::Bool {
                    return Err(self.err("VG-C0101", raw, "refinement predicate must be Bool"));
                }
                self.formula_ctx = None;
                self.type_stack.push(TypeParse::RefineClose { base, cont });
                self.step_type(tok, raw)
            }
            FormulaCont::Paren => {
                if *tok != Tok::Punct(")") {
                    return Err(self.err("VG-P0001", raw, format!("expected `)`, found `{raw}`")));
                }
                let Some(FormulaState::Frame(mut outer)) = self.formula_stack.pop() else {
                    unreachable!()
                };
                outer.expect_atom = false;
                outer.cur = Some((cur_ty, false));
                self.formula_stack.push(FormulaState::Frame(outer));
                Ok(())
            }
            FormulaCont::Index { base, dims_left } => {
                let settled = settle_fty(cur_ty).map_err(|(c, m)| self.err(c, raw, m))?;
                if settled != FTy::Int && settled != FTy::AnyInvAcc {
                    return Err(self.err("VG-C0101", raw, "index in formula must be integer"));
                }
                self.formula_stack
                    .push(FormulaState::IndexCommaOrClose { base, dims_left });
                self.step_formula(tok, raw)
            }
            FormulaCont::ForallLo { ranges_done, var } => {
                let settled = settle_fty(cur_ty).map_err(|(c, m)| self.err(c, raw, m))?;
                if settled != FTy::Int && settled != FTy::AnyInvAcc {
                    return Err(self.err("VG-C0101", raw, "forall range bound must be integer"));
                }
                self.formula_stack
                    .push(FormulaState::ForallDots { ranges_done, var });
                self.step_formula(tok, raw)
            }
            FormulaCont::ForallHi {
                mut ranges_done,
                var,
            } => {
                let settled = settle_fty(cur_ty).map_err(|(c, m)| self.err(c, raw, m))?;
                if settled != FTy::Int && settled != FTy::AnyInvAcc {
                    return Err(self.err("VG-C0101", raw, "forall range bound must be integer"));
                }
                self.formula_ctx
                    .as_mut()
                    .unwrap()
                    .bound
                    .push((var.clone(), FTy::Int));
                ranges_done.push(var);
                self.formula_stack
                    .push(FormulaState::ForallCommaOrClose { ranges_done });
                self.step_formula(tok, raw)
            }
            FormulaCont::ForallBody { saved_bound_len } => {
                let settled = settle_fty(cur_ty).map_err(|(c, m)| self.err(c, raw, m))?;
                if settled != FTy::Bool && settled != FTy::AnyInvAcc {
                    return Err(self.err("VG-C0101", raw, "forall body must be Bool"));
                }
                self.formula_ctx
                    .as_mut()
                    .unwrap()
                    .bound
                    .truncate(saved_bound_len);
                self.formula_stack.push(FormulaState::ForallRBrace);
                self.step_formula(tok, raw)
            }
            FormulaCont::CallArg {
                callee,
                mut arg_tys,
            } => {
                let idx = arg_tys.len();
                self.check_formula_call_arg(&callee, idx, &cur_ty, raw)?;
                arg_tys.push(cur_ty);
                self.formula_stack
                    .push(FormulaState::CallCommaOrClose { callee, arg_tys });
                self.step_formula(tok, raw)
            }
        }
    }

    fn step_formula(&mut self, tok: &Tok, raw: &str) -> Result<(), Diagnostic> {
        let top = self.formula_stack.pop().unwrap();
        match top {
            FormulaState::Frame(mut f) => {
                if f.expect_atom {
                    match tok {
                        Tok::Punct("!") => {
                            f.unaries.push(UnOp::Not);
                            self.formula_stack.push(FormulaState::Frame(f));
                            Ok(())
                        }
                        Tok::Punct("-") => {
                            f.unaries.push(UnOp::Neg);
                            self.formula_stack.push(FormulaState::Frame(f));
                            Ok(())
                        }
                        Tok::Punct("(") => {
                            let cont = FormulaCont::Paren;
                            self.formula_stack.push(FormulaState::Frame(f));
                            self.formula_stack.push(FormulaState::Frame(FormulaFrame {
                                unaries: Vec::new(),
                                ops: Vec::new(),
                                expect_atom: true,
                                cur: None,
                                min_allowed_prec: 1,
                                cont,
                            }));
                            Ok(())
                        }
                        Tok::Kw("forall") => {
                            self.formula_stack.push(FormulaState::Frame(f));
                            self.formula_stack.push(FormulaState::ForallOpen);
                            Ok(())
                        }
                        Tok::Kw("spec") => {
                            self.formula_stack.push(FormulaState::Frame(f));
                            self.formula_stack.push(FormulaState::AfterCallPrefix {
                                prefix: CallPrefix::Spec,
                            });
                            Ok(())
                        }
                        Tok::Kw("fold") => {
                            self.formula_stack.push(FormulaState::Frame(f));
                            self.formula_stack.push(FormulaState::AfterCallPrefix {
                                prefix: CallPrefix::Fold,
                            });
                            Ok(())
                        }
                        Tok::Kw("result") => {
                            let ctx = self.formula_ctx.as_ref().unwrap();
                            if !ctx.allow_result {
                                return Err(self.err(
                                    "VG-C0104",
                                    raw,
                                    "`result` is allowed only in POST",
                                ));
                            }
                            let results = if let Some(n) = &self.node {
                                &n.results
                            } else {
                                &self.query_results
                            };
                            if results.len() == 1 {
                                let ty = self
                                    .formula_result_type(None)
                                    .map_err(|(c, m)| self.err(c, raw, m))?;
                                f.expect_atom = false;
                                f.cur = Some((ty, true));
                                self.formula_stack.push(FormulaState::Frame(f));
                            } else {
                                self.formula_stack.push(FormulaState::Frame(f));
                                self.formula_stack.push(FormulaState::AfterResultPendingDot);
                            }
                            Ok(())
                        }
                        Tok::Kw("true") | Tok::Kw("false") => {
                            f.expect_atom = false;
                            f.cur = Some((FTy::Bool, false));
                            self.formula_stack.push(FormulaState::Frame(f));
                            Ok(())
                        }
                        Tok::Port(p) => {
                            let ctx = self.formula_ctx.as_ref().unwrap();
                            if !ctx.allow_ports {
                                return Err(self.err(
                                    "VG-C0105",
                                    raw,
                                    format!("%{p} is a port; only INVARIANT may name ports"),
                                ));
                            }
                            let inv_lbl = ctx.inv_label.clone().unwrap();
                            if let Some(n) = self.node.as_mut() {
                                n.invariants
                                    .get_mut(&inv_lbl)
                                    .unwrap()
                                    .pending_ports
                                    .push((p.clone(), Vec::new()));
                            }
                            f.expect_atom = false;
                            f.cur = Some((FTy::AnyInvAcc, false));
                            self.formula_stack.push(FormulaState::Frame(f));
                            Ok(())
                        }
                        Tok::Ident(name) => {
                            if let Err((code, msg)) = self.formula_lookup_ident(name)
                                && self.resolve_formula_callee(None, name).is_err()
                            {
                                return Err(self.err(code, raw, msg));
                            }
                            self.formula_stack.push(FormulaState::Frame(f));
                            self.formula_stack
                                .push(FormulaState::AfterIdent { name: name.clone() });
                            Ok(())
                        }
                        Tok::Int(s) => {
                            let neg_popped = if f.unaries.last() == Some(&UnOp::Neg) {
                                f.unaries.pop();
                                true
                            } else {
                                false
                            };
                            self.formula_stack.push(FormulaState::Frame(f));
                            self.formula_stack.push(FormulaState::AfterNum {
                                text: s.clone(),
                                is_decimal: false,
                                neg_popped,
                            });
                            Ok(())
                        }
                        Tok::Decimal(s) => {
                            let neg_popped = if f.unaries.last() == Some(&UnOp::Neg) {
                                f.unaries.pop();
                                true
                            } else {
                                false
                            };
                            self.formula_stack.push(FormulaState::Frame(f));
                            self.formula_stack.push(FormulaState::AfterNum {
                                text: s.clone(),
                                is_decimal: true,
                                neg_popped,
                            });
                            Ok(())
                        }
                        Tok::Punct("[") => {
                            self.formula_stack.push(FormulaState::Frame(f));
                            self.formula_stack.push(FormulaState::ArrayElem {
                                stack: vec![Vec::new()],
                                expect_elem: true,
                            });
                            Ok(())
                        }
                        _ => Err(self.kw_or_ident_err(tok, raw, "a formula")),
                    }
                } else {
                    // Expecting postfix `[` / `.`, binary operator, or end of formula frame.
                    match tok {
                        Tok::Punct("[") => {
                            let (cur_ty, _) = f.cur.as_ref().unwrap().clone();
                            let (elem_ty, dims_left) = match &cur_ty {
                                FTy::Val(Type::Array(e, _) | Type::Slice(e)) => (
                                    fty_of(e).map_err(|c| self.err(c, raw, "invalid elem"))?,
                                    Some(0usize),
                                ),
                                FTy::Val(Type::Tensor(e, dims)) => (
                                    fty_of(e).map_err(|c| self.err(c, raw, "invalid elem"))?,
                                    Some(dims.len().saturating_sub(1)),
                                ),
                                FTy::AnyInvAcc => (FTy::AnyInvAcc, None),
                                _ => {
                                    return Err(self.err(
                                        "VG-C0101",
                                        raw,
                                        format!("cannot index {cur_ty:?}"),
                                    ));
                                }
                            };
                            f.cur = Some((elem_ty, false));
                            self.formula_stack.push(FormulaState::Frame(f));
                            self.formula_stack.push(FormulaState::Frame(FormulaFrame {
                                unaries: Vec::new(),
                                ops: Vec::new(),
                                expect_atom: true,
                                cur: None,
                                min_allowed_prec: 1,
                                cont: FormulaCont::Index {
                                    base: cur_ty,
                                    dims_left,
                                },
                            }));
                            Ok(())
                        }
                        Tok::Punct(".") => {
                            let (cur_ty, is_res) = f.cur.as_ref().unwrap().clone();
                            let ok = matches!(cur_ty, FTy::Val(Type::Tuple(_)) | FTy::AnyInvAcc);
                            if !ok {
                                return Err(self.err(
                                    "VG-C0101",
                                    raw,
                                    format!("projection `.` requires Tuple, found {cur_ty:?}"),
                                ));
                            }
                            self.formula_stack.push(FormulaState::Frame(f));
                            self.formula_stack.push(FormulaState::AfterDot {
                                base: cur_ty,
                                is_result_root: is_res,
                            });
                            Ok(())
                        }
                        _ => {
                            if let Some(op) = binop_of(tok)
                                && op.level() >= f.min_allowed_prec
                            {
                                self.flush_unaries(&mut f, raw)?;
                                // Check non-chaining comparison before popping ops:
                                // if an active comparison (level 4) is still open at the top of ops
                                // (or separated only by higher-precedence arithmetic >= 5), a second level-4 op is invalid.
                                let has_open_cmp = f
                                    .ops
                                    .iter()
                                    .rev()
                                    .take_while(|(o, _)| o.level() >= 4)
                                    .any(|(o, _)| o.level() == 4);
                                if op.level() == 4 && has_open_cmp {
                                    return Err(self.err(
                                        "VG-P0001",
                                        raw,
                                        "comparisons do not chain; use parentheses or `&&`",
                                    ));
                                }
                                let (mut rhs_ty, _) = f.cur.take().unwrap();
                                while let Some(&(top_op, _)) = f.ops.last() {
                                    if top_op.level() > op.level()
                                        || (top_op.level() == op.level() && op != BinOp::Implies)
                                    {
                                        let (top_op, lhs_ty) = f.ops.pop().unwrap();
                                        rhs_ty = combine_binop(top_op, &lhs_ty, &rhs_ty)
                                            .map_err(|(c, m)| self.err(c, raw, m))?;
                                    } else {
                                        break;
                                    }
                                }
                                f.ops.push((op, rhs_ty));
                                f.expect_atom = true;
                                self.formula_stack.push(FormulaState::Frame(f));
                                Ok(())
                            } else {
                                self.complete_formula_frame(f, tok, raw)
                            }
                        }
                    }
                }
            }
            FormulaState::AfterResultPendingDot => {
                if *tok != Tok::Punct(".") {
                    return Err(self.err(
                        "VG-C0104",
                        raw,
                        "multi-result node requires `result.j` projection",
                    ));
                }
                self.formula_stack.push(FormulaState::AfterDot {
                    base: FTy::AnyInvAcc,
                    is_result_root: true,
                });
                Ok(())
            }
            FormulaState::AfterIdent { name } => {
                if *tok == Tok::Punct("(") {
                    let callee = self
                        .resolve_formula_callee(None, &name)
                        .map_err(|(c, m)| self.err(c, raw, m))?;
                    self.formula_stack
                        .push(FormulaState::AfterCallName { callee });
                    self.step_formula(tok, raw)
                } else {
                    let ty = self
                        .formula_lookup_ident(&name)
                        .map_err(|(c, m)| self.err(c, raw, m))?;
                    let Some(FormulaState::Frame(mut f)) = self.formula_stack.pop() else {
                        unreachable!()
                    };
                    f.expect_atom = false;
                    f.cur = Some((ty, false));
                    self.formula_stack.push(FormulaState::Frame(f));
                    self.step_formula(tok, raw)
                }
            }
            FormulaState::AfterNum {
                text,
                is_decimal,
                neg_popped,
            } => {
                if *tok == Tok::Punct(":") {
                    self.formula_stack.push(FormulaState::AfterNumColon {
                        snum: SNum {
                            neg: neg_popped,
                            text,
                        },
                    });
                    Ok(())
                } else {
                    let Some(FormulaState::Frame(mut f)) = self.formula_stack.pop() else {
                        unreachable!()
                    };
                    if neg_popped {
                        f.unaries.push(UnOp::Neg);
                    }
                    f.expect_atom = false;
                    f.cur = Some((FTy::Num(is_decimal), false));
                    self.formula_stack.push(FormulaState::Frame(f));
                    self.step_formula(tok, raw)
                }
            }
            FormulaState::AfterNumColon { snum } => match tok {
                Tok::Kw(k) if SCALARS.contains(k) => {
                    let lit = ast::Literal::Num(snum, k);
                    let c = literal_const(&lit, self.span(raw))?;
                    let ty = fty_of(&c.ty).map_err(|code| self.err(code, raw, "scalar type"))?;
                    let Some(FormulaState::Frame(mut f)) = self.formula_stack.pop() else {
                        unreachable!()
                    };
                    f.expect_atom = false;
                    f.cur = Some((ty, false));
                    self.formula_stack.push(FormulaState::Frame(f));
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected a scalar type, found `{raw}`"),
                )),
            },
            FormulaState::ArrayElem {
                mut stack,
                expect_elem,
            } => {
                if expect_elem {
                    match tok {
                        Tok::Punct("[") => {
                            stack.push(Vec::new());
                            self.formula_stack.push(FormulaState::ArrayElem {
                                stack,
                                expect_elem: true,
                            });
                            Ok(())
                        }
                        Tok::Punct("-") => {
                            self.formula_stack.push(FormulaState::ArraySign { stack });
                            Ok(())
                        }
                        Tok::Int(s) | Tok::Decimal(s) => {
                            stack.last_mut().unwrap().push(ast::Elem::Num(SNum {
                                neg: false,
                                text: s.clone(),
                            }));
                            self.formula_stack.push(FormulaState::ArrayElem {
                                stack,
                                expect_elem: false,
                            });
                            Ok(())
                        }
                        Tok::Kw("true") => {
                            stack.last_mut().unwrap().push(ast::Elem::Bool(true));
                            self.formula_stack.push(FormulaState::ArrayElem {
                                stack,
                                expect_elem: false,
                            });
                            Ok(())
                        }
                        Tok::Kw("false") => {
                            stack.last_mut().unwrap().push(ast::Elem::Bool(false));
                            self.formula_stack.push(FormulaState::ArrayElem {
                                stack,
                                expect_elem: false,
                            });
                            Ok(())
                        }
                        _ => Err(self.err(
                            "VG-P0001",
                            raw,
                            format!("expected array element, found `{raw}`"),
                        )),
                    }
                } else {
                    match tok {
                        Tok::Punct(",") => {
                            self.formula_stack.push(FormulaState::ArrayElem {
                                stack,
                                expect_elem: true,
                            });
                            Ok(())
                        }
                        Tok::Punct("]") => {
                            let done = stack.pop().unwrap();
                            if let Some(parent) = stack.last_mut() {
                                parent.push(ast::Elem::Array(done));
                                self.formula_stack.push(FormulaState::ArrayElem {
                                    stack,
                                    expect_elem: false,
                                });
                            } else {
                                self.formula_stack
                                    .push(FormulaState::ArrayColon { elems: done });
                            }
                            Ok(())
                        }
                        _ => Err(self.err(
                            "VG-P0001",
                            raw,
                            format!("expected `,` or `]`, found `{raw}`"),
                        )),
                    }
                }
            }
            FormulaState::ArraySign { mut stack } => match tok {
                Tok::Int(s) | Tok::Decimal(s) => {
                    stack.last_mut().unwrap().push(ast::Elem::Num(SNum {
                        neg: true,
                        text: s.clone(),
                    }));
                    self.formula_stack.push(FormulaState::ArrayElem {
                        stack,
                        expect_elem: false,
                    });
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected number after `-`, found `{raw}`"),
                )),
            },
            FormulaState::ArrayColon { elems } => {
                if *tok == Tok::Punct(":") {
                    self.push_type(false, TypeCont::FormulaArrayLit { elems });
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `:`, found `{raw}`")))
                }
            }
            FormulaState::AfterCallPrefix { prefix } => {
                if *tok == Tok::Punct(":") {
                    self.formula_stack
                        .push(FormulaState::AfterCallColon { prefix });
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `:`, found `{raw}`")))
                }
            }
            FormulaState::AfterCallColon { prefix } => match tok {
                Tok::Ident(name) => {
                    let callee = self
                        .resolve_formula_callee(Some(prefix), name)
                        .map_err(|(c, m)| self.err(c, raw, m))?;
                    self.formula_stack
                        .push(FormulaState::AfterCallName { callee });
                    Ok(())
                }
                _ => Err(self.kw_or_ident_err(tok, raw, "a function name")),
            },
            FormulaState::AfterCallName { callee } => {
                if *tok != Tok::Punct("(") {
                    return Err(self.err("VG-P0001", raw, format!("expected `(`, found `{raw}`")));
                }
                let zero_allowed = matches!(
                    &callee,
                    FormulaCallee::Node(ft) | FormulaCallee::Spec(ft) if ft.params.is_empty()
                );
                if zero_allowed {
                    self.formula_stack.push(FormulaState::CallCommaOrClose {
                        callee,
                        arg_tys: Vec::new(),
                    });
                } else {
                    self.formula_stack.push(FormulaState::Frame(FormulaFrame {
                        unaries: Vec::new(),
                        ops: Vec::new(),
                        expect_atom: true,
                        cur: None,
                        min_allowed_prec: 1,
                        cont: FormulaCont::CallArg {
                            callee,
                            arg_tys: Vec::new(),
                        },
                    }));
                }
                Ok(())
            }
            FormulaState::CallCommaOrClose { callee, arg_tys } => match tok {
                Tok::Punct(",") => {
                    self.formula_stack.push(FormulaState::Frame(FormulaFrame {
                        unaries: Vec::new(),
                        ops: Vec::new(),
                        expect_atom: true,
                        cur: None,
                        min_allowed_prec: 1,
                        cont: FormulaCont::CallArg { callee, arg_tys },
                    }));
                    Ok(())
                }
                Tok::Punct(")") => {
                    let res_ty = self.finish_formula_call(&callee, &arg_tys, raw)?;
                    let Some(FormulaState::Frame(mut f)) = self.formula_stack.pop() else {
                        unreachable!()
                    };
                    f.expect_atom = false;
                    f.cur = Some((res_ty, false));
                    self.formula_stack.push(FormulaState::Frame(f));
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `)`, found `{raw}`"),
                )),
            },
            FormulaState::AfterDot {
                base,
                is_result_root,
            } => match tok {
                Tok::Int(s) => {
                    let j = s.parse::<usize>().unwrap_or(usize::MAX);
                    let res_ty = if is_result_root {
                        self.formula_result_type(Some(j))
                            .map_err(|(c, m)| self.err(c, raw, m))?
                    } else {
                        match base {
                            FTy::Val(Type::Tuple(ms)) => match ms.get(j) {
                                Some(t) => fty_of(t)
                                    .map_err(|c| self.err(c, raw, "invalid tuple member"))?,
                                None => {
                                    return Err(self.err(
                                        "VG-C0101",
                                        raw,
                                        format!("tuple projection `.{j}` out of range"),
                                    ));
                                }
                            },
                            FTy::AnyInvAcc => FTy::AnyInvAcc,
                            _ => {
                                return Err(self.err(
                                    "VG-C0101",
                                    raw,
                                    format!("projection `.{j}` requires Tuple"),
                                ));
                            }
                        }
                    };
                    let Some(FormulaState::Frame(mut f)) = self.formula_stack.pop() else {
                        unreachable!()
                    };
                    f.expect_atom = false;
                    f.cur = Some((res_ty, false));
                    self.formula_stack.push(FormulaState::Frame(f));
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected integer index, found `{raw}`"),
                )),
            },
            FormulaState::IndexCommaOrClose { base, dims_left } => match tok {
                Tok::Punct(",") => {
                    let next_left = match dims_left {
                        Some(0) => {
                            return Err(self.err("VG-C0101", raw, "too many index dimensions"));
                        }
                        Some(d) => Some(d - 1),
                        None => None,
                    };
                    self.formula_stack.push(FormulaState::Frame(FormulaFrame {
                        unaries: Vec::new(),
                        ops: Vec::new(),
                        expect_atom: true,
                        cur: None,
                        min_allowed_prec: 1,
                        cont: FormulaCont::Index {
                            base,
                            dims_left: next_left,
                        },
                    }));
                    Ok(())
                }
                Tok::Punct("]") => {
                    if matches!(dims_left, Some(d) if d > 0) {
                        return Err(self.err(
                            "VG-C0101",
                            raw,
                            "tensor indexing requires one index per dimension",
                        ));
                    }
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `]`, found `{raw}`"),
                )),
            },
            FormulaState::ForallOpen => {
                if *tok == Tok::Punct("(") {
                    self.formula_stack.push(FormulaState::ForallVar {
                        ranges_done: Vec::new(),
                    });
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `(`, found `{raw}`")))
                }
            }
            FormulaState::ForallVar { ranges_done } => match tok {
                Tok::Ident(v) => {
                    self.formula_stack.push(FormulaState::ForallIn {
                        ranges_done,
                        var: v.clone(),
                    });
                    Ok(())
                }
                _ => Err(self.kw_or_ident_err(tok, raw, "a bound variable name")),
            },
            FormulaState::ForallIn { ranges_done, var } => {
                if *tok == Tok::Kw("in") {
                    self.formula_stack.push(FormulaState::Frame(FormulaFrame {
                        unaries: Vec::new(),
                        ops: Vec::new(),
                        expect_atom: true,
                        cur: None,
                        min_allowed_prec: 5,
                        cont: FormulaCont::ForallLo { ranges_done, var },
                    }));
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `in`, found `{raw}`")))
                }
            }
            FormulaState::ForallDots { ranges_done, var } => {
                if *tok == Tok::Punct("..") {
                    self.formula_stack.push(FormulaState::Frame(FormulaFrame {
                        unaries: Vec::new(),
                        ops: Vec::new(),
                        expect_atom: true,
                        cur: None,
                        min_allowed_prec: 5,
                        cont: FormulaCont::ForallHi { ranges_done, var },
                    }));
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `..`, found `{raw}`")))
                }
            }
            FormulaState::ForallCommaOrClose { ranges_done } => match tok {
                Tok::Punct(",") => {
                    self.formula_stack
                        .push(FormulaState::ForallVar { ranges_done });
                    Ok(())
                }
                Tok::Punct(")") => {
                    let saved_bound_len =
                        self.formula_ctx.as_ref().unwrap().bound.len() - ranges_done.len();
                    self.formula_stack
                        .push(FormulaState::ForallLBrace { saved_bound_len });
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `)`, found `{raw}`"),
                )),
            },
            FormulaState::ForallLBrace { saved_bound_len } => {
                if *tok == Tok::Punct("{") {
                    self.formula_stack.push(FormulaState::Frame(FormulaFrame {
                        unaries: Vec::new(),
                        ops: Vec::new(),
                        expect_atom: true,
                        cur: None,
                        min_allowed_prec: 1,
                        cont: FormulaCont::ForallBody { saved_bound_len },
                    }));
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `{{`, found `{raw}`")))
                }
            }
            FormulaState::ForallRBrace => {
                if *tok == Tok::Punct("}") {
                    let Some(FormulaState::Frame(mut f)) = self.formula_stack.pop() else {
                        unreachable!()
                    };
                    f.expect_atom = false;
                    f.cur = Some((FTy::Bool, false));
                    self.formula_stack.push(FormulaState::Frame(f));
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `}}`, found `{raw}`")))
                }
            }
        }
    }

    fn allowed_formula(&self, top: &FormulaState) -> Allowed {
        let mut out = empty_allowed();
        let ctx = self.formula_ctx.as_ref().unwrap();
        match top {
            FormulaState::Frame(f) => {
                if f.expect_atom {
                    out.token_classes.extend([
                        TokenClass::Punct("!"),
                        TokenClass::Punct("-"),
                        TokenClass::Punct("("),
                        TokenClass::Punct("["),
                        TokenClass::Keyword("forall"),
                        TokenClass::Keyword("spec"),
                        TokenClass::Keyword("fold"),
                        TokenClass::Keyword("true"),
                        TokenClass::Keyword("false"),
                        TokenClass::IntLiteral,
                        TokenClass::DecimalLiteral,
                    ]);
                    if ctx.allow_result {
                        out.token_classes.push(TokenClass::Keyword("result"));
                    }
                    if ctx.allow_ports {
                        out.token_classes.push(TokenClass::NewPort);
                    }
                    for b in BUILTINS {
                        out.token_classes.push(TokenClass::Name((*b).to_string()));
                    }
                    for (name, _) in &ctx.bound {
                        out.token_classes.push(TokenClass::Name(name.clone()));
                    }
                    if ctx.allow_resources {
                        for r in RESOURCES {
                            if !(ctx.clause == ClauseKind::Cost && *r == "latency_p99_us") {
                                out.token_classes.push(TokenClass::Name((*r).to_string()));
                            }
                        }
                    }
                    if let Some(n) = &self.node {
                        for (name, ty) in &n.inputs {
                            if fty_of(ty).is_ok() {
                                out.token_classes.push(TokenClass::Name(name.clone()));
                            }
                        }
                    } else if ctx.clause == ClauseKind::Pre || ctx.clause == ClauseKind::Post {
                        for (i, ty) in self.query_params.iter().enumerate() {
                            if fty_of(ty).is_ok() {
                                out.token_classes.push(TokenClass::Name(format!("arg{i}")));
                            }
                        }
                    }
                    for (lname, linfo) in &self.labels {
                        if is_valid_kernel_label(linfo) {
                            out.token_classes.push(TokenClass::Name(lname.clone()));
                        }
                    }
                } else {
                    let (cur_ty, is_res) = f.cur.as_ref().unwrap();
                    if matches!(
                        cur_ty,
                        FTy::Val(Type::Array(..) | Type::Slice(_) | Type::Tensor(..))
                            | FTy::AnyInvAcc
                    ) {
                        out.token_classes.push(TokenClass::Punct("["));
                    }
                    if *is_res || matches!(cur_ty, FTy::Val(Type::Tuple(_)) | FTy::AnyInvAcc) {
                        out.token_classes.push(TokenClass::Punct("."));
                    }
                    let has_open_cmp = f
                        .ops
                        .iter()
                        .rev()
                        .take_while(|(o, _)| o.level() >= 4)
                        .any(|(o, _)| o.level() == 4);
                    if f.min_allowed_prec <= 1 {
                        out.token_classes.extend([
                            TokenClass::Punct("==>"),
                            TokenClass::Punct("||"),
                            TokenClass::Punct("&&"),
                        ]);
                        if !has_open_cmp {
                            out.token_classes.extend([
                                TokenClass::Punct("=="),
                                TokenClass::Punct("!="),
                                TokenClass::Punct("<"),
                                TokenClass::Punct("<="),
                                TokenClass::Punct(">"),
                                TokenClass::Punct(">="),
                            ]);
                        }
                    }
                    out.token_classes.extend([
                        TokenClass::Punct("+"),
                        TokenClass::Punct("-"),
                        TokenClass::Punct("*"),
                        TokenClass::Punct("/"),
                        TokenClass::Punct("%"),
                    ]);
                    match &f.cont {
                        FormulaCont::QueryPre
                        | FormulaCont::QueryPost
                        | FormulaCont::Clause { .. } => {
                            out.token_classes.push(TokenClass::Punct(";"));
                        }
                        FormulaCont::Refine { .. } | FormulaCont::ForallBody { .. } => {
                            out.token_classes.push(TokenClass::Punct("}"));
                        }
                        FormulaCont::Paren => {
                            out.token_classes.push(TokenClass::Punct(")"));
                        }
                        FormulaCont::Index { dims_left, .. } => {
                            if *dims_left == Some(0) {
                                out.token_classes.push(TokenClass::Punct("]"));
                            } else {
                                out.token_classes.push(TokenClass::Punct(","));
                                if dims_left.is_none() {
                                    out.token_classes.push(TokenClass::Punct("]"));
                                }
                            }
                        }
                        FormulaCont::ForallLo { .. } => {
                            out.token_classes.push(TokenClass::Punct(".."));
                        }
                        FormulaCont::ForallHi { .. } | FormulaCont::CallArg { .. } => {
                            out.token_classes.push(TokenClass::Punct(","));
                            out.token_classes.push(TokenClass::Punct(")"));
                        }
                    }
                }
            }
            FormulaState::AfterResultPendingDot => {
                out.token_classes.push(TokenClass::Punct("."));
            }
            FormulaState::AfterIdent { name } => {
                if self.resolve_formula_callee(None, name).is_ok() {
                    out.token_classes.push(TokenClass::Punct("("));
                }
                if self.formula_lookup_ident(name).is_ok() {
                    // Any continuation valid after a primary atom:
                    let dummy_frame = FormulaFrame {
                        unaries: Vec::new(),
                        ops: Vec::new(),
                        expect_atom: false,
                        cur: Some((self.formula_lookup_ident(name).unwrap(), false)),
                        min_allowed_prec: match self.formula_stack.iter().rev().nth(1) {
                            Some(FormulaState::Frame(pf)) => pf.min_allowed_prec,
                            _ => 1,
                        },
                        cont: match self.formula_stack.iter().rev().nth(1) {
                            Some(FormulaState::Frame(pf)) => pf.cont.clone(),
                            _ => FormulaCont::Paren,
                        },
                    };
                    let mut rest = self.allowed_formula(&FormulaState::Frame(dummy_frame));
                    out.token_classes.append(&mut rest.token_classes);
                }
            }
            FormulaState::AfterNum {
                is_decimal,
                neg_popped,
                ..
            } => {
                out.token_classes.push(TokenClass::Punct(":"));
                let _ = (is_decimal, neg_popped);
                let dummy_frame = FormulaFrame {
                    unaries: Vec::new(),
                    ops: Vec::new(),
                    expect_atom: false,
                    cur: Some((FTy::Num(*is_decimal), false)),
                    min_allowed_prec: match self.formula_stack.iter().rev().nth(1) {
                        Some(FormulaState::Frame(pf)) => pf.min_allowed_prec,
                        _ => 1,
                    },
                    cont: match self.formula_stack.iter().rev().nth(1) {
                        Some(FormulaState::Frame(pf)) => pf.cont.clone(),
                        _ => FormulaCont::Paren,
                    },
                };
                let mut rest = self.allowed_formula(&FormulaState::Frame(dummy_frame));
                out.token_classes.append(&mut rest.token_classes);
            }
            FormulaState::AfterNumColon { snum } => {
                for &s in SCALARS {
                    if literal_const(&ast::Literal::Num(snum.clone(), s), Span::default()).is_ok() {
                        out.token_classes.push(TokenClass::Keyword(s));
                    }
                }
            }
            FormulaState::ArrayElem { expect_elem, .. } => {
                if *expect_elem {
                    out.token_classes.extend([
                        TokenClass::Punct("["),
                        TokenClass::Punct("-"),
                        TokenClass::IntLiteral,
                        TokenClass::DecimalLiteral,
                        TokenClass::Keyword("true"),
                        TokenClass::Keyword("false"),
                    ]);
                } else {
                    out.token_classes
                        .extend([TokenClass::Punct(","), TokenClass::Punct("]")]);
                }
            }
            FormulaState::ArraySign { .. } => {
                out.token_classes
                    .extend([TokenClass::IntLiteral, TokenClass::DecimalLiteral]);
            }
            FormulaState::ArrayColon { .. } | FormulaState::AfterCallPrefix { .. } => {
                out.token_classes.push(TokenClass::Punct(":"));
            }
            FormulaState::AfterCallColon { prefix } => {
                if *prefix == CallPrefix::Spec
                    && self.node.is_none()
                    && !self.query_label.is_empty()
                {
                    out.token_classes
                        .push(TokenClass::Name(self.query_label.clone()));
                }
                for (name, info) in &self.labels {
                    let ok = match prefix {
                        CallPrefix::Spec => matches!(info.kind, ItemKind::Spec | ItemKind::Query),
                        CallPrefix::Fold => is_valid_fold_spec_kernel(info),
                    };
                    if ok {
                        out.token_classes.push(TokenClass::Name(name.clone()));
                    }
                }
            }
            FormulaState::AfterCallName { .. } | FormulaState::ForallOpen => {
                out.token_classes.push(TokenClass::Punct("("));
            }
            FormulaState::CallCommaOrClose { callee, arg_tys } => {
                let can_close = match callee {
                    FormulaCallee::Builtin(b) => {
                        arg_tys.len()
                            == if matches!(*b, "min" | "max" | "same") {
                                2
                            } else {
                                1
                            }
                    }
                    FormulaCallee::Node(ft) | FormulaCallee::Spec(ft) => {
                        arg_tys.len() == ft.params.len()
                    }
                    FormulaCallee::Fold(ft) => arg_tys.len() == ft.params.len() + 1,
                };
                let can_more = match callee {
                    FormulaCallee::Builtin(b) => {
                        arg_tys.len()
                            < if matches!(*b, "min" | "max" | "same") {
                                2
                            } else {
                                1
                            }
                    }
                    FormulaCallee::Node(ft) | FormulaCallee::Spec(ft) => {
                        arg_tys.len() < ft.params.len()
                    }
                    FormulaCallee::Fold(ft) => arg_tys.len() < ft.params.len() + 1,
                };
                if can_close {
                    out.token_classes.push(TokenClass::Punct(")"));
                }
                if can_more {
                    out.token_classes.push(TokenClass::Punct(","));
                }
            }
            FormulaState::AfterDot { .. } => {
                out.token_classes.push(TokenClass::IntLiteral);
            }
            FormulaState::IndexCommaOrClose { dims_left, .. } => {
                if *dims_left == Some(0) {
                    out.token_classes.push(TokenClass::Punct("]"));
                } else {
                    out.token_classes.push(TokenClass::Punct(","));
                    if dims_left.is_none() {
                        out.token_classes.push(TokenClass::Punct("]"));
                    }
                }
            }
            FormulaState::ForallVar { .. } => {
                out.token_classes.push(TokenClass::Ident);
            }
            FormulaState::ForallIn { .. } => {
                out.token_classes.push(TokenClass::Keyword("in"));
            }
            FormulaState::ForallDots { .. } => {
                out.token_classes.push(TokenClass::Punct(".."));
            }
            FormulaState::ForallCommaOrClose { .. } => {
                out.token_classes
                    .extend([TokenClass::Punct(","), TokenClass::Punct(")")]);
            }
            FormulaState::ForallLBrace { .. } => {
                out.token_classes.push(TokenClass::Punct("{"));
            }
            FormulaState::ForallRBrace => {
                out.token_classes.push(TokenClass::Punct("}"));
            }
        }
        out.token_classes.sort();
        out.token_classes.dedup();
        out
    }

    // -----------------------------------------------------------------------
    // Body op typing & validation (matching vg-core::lower + vg-types::check)
    // -----------------------------------------------------------------------

    fn op_allowed_for_lhs(&self, info: &OpInfo, binds: &[StmtBind]) -> bool {
        if !info.mvp {
            return false;
        }
        let n = self.node.as_ref().unwrap();
        let n_binds = binds.len();
        // Arity of result ports
        let fixed_res_count = match info.name {
            "FILE_READ" | "FILE_WRITE" | "CLOCK_READ" => Some(2),
            "CALL_HASH" | "ASSERT_GUARD" => None,
            _ => Some(1),
        };
        if let Some(rc) = fixed_res_count
            && n_binds != rc
        {
            return false;
        }
        // Effect requirements known at op position
        match info.name {
            "FILE_READ" => {
                let fr = EffectMask::bit("FILE_READ").unwrap();
                if n.effects.0 & (fr | EffectMask::ARENA) != (fr | EffectMask::ARENA) {
                    return false;
                }
            }
            "FILE_WRITE" => {
                let fw = EffectMask::bit("FILE_WRITE").unwrap();
                if n.effects.0 & fw == 0 {
                    return false;
                }
            }
            "CLOCK_READ" => {
                let cl = EffectMask::bit("CLOCK").unwrap();
                if n.effects.0 & cl == 0 {
                    return false;
                }
            }
            "OK" | "ERR" => {
                let Some(Type::Result(..)) = binds[0].ann else {
                    return false;
                };
            }
            _ => {}
        }
        // Check LHS annotations if fixed by op
        if let Some(ann) = &binds[0].ann {
            match info.name {
                "EQ" | "NE" | "LT" | "LE" | "GT" | "GE" | "BAND" | "BOR" | "BNOT" | "IS_OK" => {
                    if *ann != Type::Bool {
                        return false;
                    }
                }
                "LEN" => {
                    if *ann != Type::UInt(64) {
                        return false;
                    }
                }
                "AS_BYTES" => {
                    if *ann != Type::Slice(Box::new(Type::UInt(8))) {
                        return false;
                    }
                }
                "CAST" => {
                    if !is_valid_scalar(ann) {
                        return false;
                    }
                }
                "FILE_READ" => {
                    let want1 = Type::Result(
                        Box::new(Type::Slice(Box::new(Type::UInt(8)))),
                        Box::new(Type::UInt(32)),
                    );
                    if *ann != Type::Eff
                        || binds
                            .get(1)
                            .and_then(|b| b.ann.as_ref())
                            .is_some_and(|t| *t != want1)
                    {
                        return false;
                    }
                }
                "FILE_WRITE" => {
                    let want1 = Type::Result(Box::new(Type::UInt(64)), Box::new(Type::UInt(32)));
                    if *ann != Type::Eff
                        || binds
                            .get(1)
                            .and_then(|b| b.ann.as_ref())
                            .is_some_and(|t| *t != want1)
                    {
                        return false;
                    }
                }
                "CLOCK_READ" => {
                    if *ann != Type::Eff
                        || binds
                            .get(1)
                            .and_then(|b| b.ann.as_ref())
                            .is_some_and(|t| *t != Type::UInt(64))
                    {
                        return false;
                    }
                }
                _ => {}
            }
        }
        // Check callee availability for combinators/calls
        match info.shape {
            Shape::Kernel => self.labels.values().any(is_valid_kernel_label),
            Shape::Fold => {
                !n.invariants.is_empty() && self.labels.values().any(is_valid_fold_spec_kernel)
            }
            Shape::Guard => self.labels.values().any(|l| {
                l.kind != ItemKind::Spec
                    && (l.effects.0 & !n.effects.0 == 0)
                    && l.fn_type.results.len() == n_binds
            }),
            Shape::Call => self.labels.values().any(|l| {
                l.kind != ItemKind::Spec
                    && (l.effects.0 & !n.effects.0 == 0)
                    && l.fn_type.results.len() == n_binds
            }),
            _ => true,
        }
    }

    fn modes_for_op(&self, info: &OpInfo, binds: &[StmtBind]) -> Vec<String> {
        match info.modes {
            Modes::None => Vec::new(),
            Modes::IntArith => vec!["checked".into(), "wrap".into(), "sat".into()],
            Modes::Shift => vec!["checked".into(), "wrap".into()],
            Modes::Compare => vec!["s".into(), "u".into(), "f".into()],
            Modes::Guard => vec!["branch".into()],
            Modes::Cast => {
                if let Some(ann) = &binds[0].ann {
                    ann.scalar_name()
                        .map(|s| vec![s.to_string()])
                        .unwrap_or_default()
                } else {
                    SCALARS.iter().map(|s| (*s).to_string()).collect()
                }
            }
        }
    }

    fn val_fits_stmt_slot(
        &self,
        call: &StmtCall,
        name_opt: Option<&str>,
        val: &ValMeta,
    ) -> Result<(), (&'static str, String)> {
        let n = self.node.as_ref().unwrap();
        if let Some(arg_name) = call.cur_arg_name.as_deref() {
            match (call.info.shape, arg_name) {
                (Shape::Fold, "init") => {
                    let kernel = &call.callees_vec()[0];
                    if contains_eff_or_handle(&val.ty) {
                        return Err((
                            "VG-T0119",
                            "Eff/Handle not allowed in FOLD_BOUNDED init".into(),
                        ));
                    }
                    if val.ty != kernel.fn_type.params[0] {
                        return Err((
                            "VG-T0106",
                            format!(
                                "FOLD_BOUNDED init expected {}, found {}",
                                kernel.fn_type.params[0], val.ty
                            ),
                        ));
                    }
                    return Ok(());
                }
                _ => return Err(("VG-S0109", "this named argument requires a label".into())),
            }
        }

        let pos = call.args.len();
        let reject_eh = |v: &ValMeta| -> Result<(), (&'static str, String)> {
            if contains_eff_or_handle(&v.ty) {
                Err((
                    "VG-T0119",
                    format!("{} does not allow Eff or Handle operands", call.op_name),
                ))
            } else {
                Ok(())
            }
        };

        match call.op_name.as_str() {
            "IDENTITY" => {
                if pos > 0 {
                    return Err(("VG-S0103", "IDENTITY takes 1 argument".into()));
                }
                if let Some(ann) = &call.binds[0].ann
                    && *ann != val.ty
                {
                    return Err((
                        "VG-T0115",
                        format!("port annotated {ann}, operand is {}", val.ty),
                    ));
                }
                Ok(())
            }
            "TUPLE" => {
                reject_eh(val)?;
                let mut tys: Vec<Type> = call
                    .args
                    .iter()
                    .filter_map(|a| a.val.as_ref().map(|v| v.ty.clone()))
                    .collect();
                tys.push(val.ty.clone());
                validate_type(&Type::Tuple(tys), false, 1, Span::default())
                    .map_err(|d| (d.code, d.message))
            }
            "PROJ" => {
                reject_eh(val)?;
                match pos {
                    0 => {
                        let Type::Tuple(ms) = &val.ty else {
                            return Err((
                                "VG-T0101",
                                format!("PROJ expects Tuple, found {}", val.ty),
                            ));
                        };
                        if let Some(ann) = &call.binds[0].ann
                            && !ms.contains(ann)
                        {
                            return Err((
                                "VG-T0115",
                                format!("tuple has no member of annotated type {ann}"),
                            ));
                        }
                        Ok(())
                    }
                    1 => {
                        if !val.is_literal {
                            return Err(("VG-S0108", "PROJ index must be a u32 literal".into()));
                        }
                        if val.ty != Type::UInt(32) {
                            return Err((
                                "VG-T0101",
                                format!("PROJ index must be u32, found {}", val.ty),
                            ));
                        }
                        let j = val.const_u64.unwrap_or(u64::MAX) as usize;
                        let Type::Tuple(ms) = &call.args[0].val.as_ref().unwrap().ty else {
                            unreachable!()
                        };
                        let Some(mt) = ms.get(j) else {
                            return Err(("VG-T0101", format!("PROJ index {j} out of range")));
                        };
                        if let Some(ann) = &call.binds[0].ann
                            && *ann != *mt
                        {
                            return Err((
                                "VG-T0115",
                                format!("member {j} is {mt}, port annotated {ann}"),
                            ));
                        }
                        Ok(())
                    }
                    _ => Err(("VG-S0103", "PROJ takes 2 arguments".into())),
                }
            }
            "SELECT" => {
                reject_eh(val)?;
                match pos {
                    0 => {
                        if val.ty != Type::Bool {
                            Err((
                                "VG-T0111",
                                format!("SELECT condition must be Bool, found {}", val.ty),
                            ))
                        } else {
                            Ok(())
                        }
                    }
                    1 => {
                        if let Some(ann) = &call.binds[0].ann
                            && *ann != val.ty
                        {
                            return Err((
                                "VG-T0115",
                                format!("SELECT annotated {ann}, arm is {}", val.ty),
                            ));
                        }
                        Ok(())
                    }
                    2 => {
                        let t1 = &call.args[1].val.as_ref().unwrap().ty;
                        if val.ty != *t1 {
                            Err((
                                "VG-T0102",
                                format!("SELECT arms differ: {t1} vs {}", val.ty),
                            ))
                        } else {
                            Ok(())
                        }
                    }
                    _ => Err(("VG-S0103", "SELECT takes 3 arguments".into())),
                }
            }
            "CAST" => {
                reject_eh(val)?;
                if pos > 0 {
                    return Err(("VG-S0103", "CAST takes 1 argument".into()));
                }
                let dst = call.cast_ty.as_ref().unwrap();
                if !is_valid_cast(&val.ty, dst) {
                    Err((
                        "VG-T0113",
                        format!("CAST not allowed from {} to {dst}", val.ty),
                    ))
                } else {
                    Ok(())
                }
            }
            "AS_BYTES" => {
                reject_eh(val)?;
                if pos > 0 {
                    return Err(("VG-S0103", "AS_BYTES takes 1 argument".into()));
                }
                if !has_byte_form(&val.ty) {
                    Err((
                        "VG-T0114",
                        format!("AS_BYTES has no byte form for {}", val.ty),
                    ))
                } else {
                    Ok(())
                }
            }
            "OK" | "ERR" => {
                reject_eh(val)?;
                if pos > 0 {
                    return Err(("VG-S0103", format!("{} takes 1 argument", call.op_name)));
                }
                let Some(Type::Result(ok_t, err_t)) = call.binds[0].ann.as_ref() else {
                    return Err((
                        "VG-T0115",
                        format!("{} requires Result<T, E> port annotation", call.op_name),
                    ));
                };
                let want = if call.op_name == "OK" { ok_t } else { err_t };
                if val.ty != **want {
                    Err((
                        "VG-T0101",
                        format!("{} expected {want}, found {}", call.op_name, val.ty),
                    ))
                } else {
                    Ok(())
                }
            }
            "IS_OK" | "GET_OK" | "GET_ERR" => {
                reject_eh(val)?;
                if pos > 0 {
                    return Err(("VG-S0103", format!("{} takes 1 argument", call.op_name)));
                }
                let Type::Result(ok_t, err_t) = &val.ty else {
                    return Err((
                        "VG-T0101",
                        format!("{} requires Result, found {}", call.op_name, val.ty),
                    ));
                };
                if let Some(ann) = &call.binds[0].ann {
                    let res_t = match call.op_name.as_str() {
                        "IS_OK" => &Type::Bool,
                        "GET_OK" => ok_t.as_ref(),
                        _ => err_t.as_ref(),
                    };
                    if ann != res_t {
                        return Err(("VG-T0115", format!("annotated {ann}, op returns {res_t}")));
                    }
                }
                Ok(())
            }
            "ADD" | "SUB" | "MUL" | "DIV" | "MOD" | "AND" | "OR" | "XOR" => {
                reject_eh(val)?;
                if val.ty.is_float() {
                    return Err((
                        "VG-T0104",
                        format!("integer op {} on float {}", call.op_name, val.ty),
                    ));
                }
                if !val.ty.is_int() {
                    return Err((
                        "VG-T0101",
                        format!("{} requires integer, found {}", call.op_name, val.ty),
                    ));
                }
                if pos == 0 {
                    if let Some(ann) = &call.binds[0].ann
                        && *ann != val.ty
                    {
                        return Err((
                            "VG-T0115",
                            format!("annotated {ann}, operand is {}", val.ty),
                        ));
                    }
                } else if pos == 1 {
                    let t0 = &call.args[0].val.as_ref().unwrap().ty;
                    if val.ty != *t0 {
                        return Err((
                            "VG-T0102",
                            format!("{} operands differ: {t0} vs {}", call.op_name, val.ty),
                        ));
                    }
                } else {
                    return Err(("VG-S0103", format!("{} takes 2 arguments", call.op_name)));
                }
                Ok(())
            }
            "NEG" | "NOT" => {
                reject_eh(val)?;
                if pos > 0 {
                    return Err(("VG-S0103", format!("{} takes 1 argument", call.op_name)));
                }
                if val.ty.is_float() {
                    return Err(("VG-T0104", format!("integer op {} on float", call.op_name)));
                }
                if !val.ty.is_int() {
                    return Err((
                        "VG-T0101",
                        format!("{} requires integer, found {}", call.op_name, val.ty),
                    ));
                }
                if let Some(ann) = &call.binds[0].ann
                    && *ann != val.ty
                {
                    return Err((
                        "VG-T0115",
                        format!("annotated {ann}, operand is {}", val.ty),
                    ));
                }
                Ok(())
            }
            "SHL" | "SHR" | "ROTL" | "ROTR" => {
                reject_eh(val)?;
                match pos {
                    0 => {
                        if val.ty.is_float() {
                            return Err((
                                "VG-T0104",
                                format!("integer op {} on float", call.op_name),
                            ));
                        }
                        if !val.ty.is_int() {
                            return Err((
                                "VG-T0101",
                                format!("{} requires integer, found {}", call.op_name, val.ty),
                            ));
                        }
                        if let Some(ann) = &call.binds[0].ann
                            && *ann != val.ty
                        {
                            return Err((
                                "VG-T0115",
                                format!("annotated {ann}, operand is {}", val.ty),
                            ));
                        }
                        Ok(())
                    }
                    1 => {
                        if val.ty != Type::UInt(32) {
                            Err((
                                "VG-T0112",
                                format!(
                                    "{} shift/rotate amount must be u32, found {}",
                                    call.op_name, val.ty
                                ),
                            ))
                        } else {
                            Ok(())
                        }
                    }
                    _ => Err(("VG-S0103", format!("{} takes 2 arguments", call.op_name))),
                }
            }
            "FADD" | "FSUB" | "FMUL" | "FDIV" | "FMA" | "FNEG" | "FABS" | "FMIN" | "FMAX"
            | "FCLAMP" | "FCOPYSIGN" | "FSQRT" => {
                reject_eh(val)?;
                if is_f16_or_bf16(&val.ty) {
                    return Err(("VG-T0105", "f16 and bf16 are storage-only in MVP".into()));
                }
                if val.ty.is_int() {
                    return Err(("VG-T0104", format!("float op {} on integer", call.op_name)));
                }
                if !is_mvp_arith_float(&val.ty) {
                    return Err(("VG-T0101", format!("{} requires f32 or f64", call.op_name)));
                }
                if pos == 0 {
                    if let Some(ann) = &call.binds[0].ann
                        && *ann != val.ty
                    {
                        return Err((
                            "VG-T0115",
                            format!("annotated {ann}, operand is {}", val.ty),
                        ));
                    }
                } else {
                    let t0 = &call.args[0].val.as_ref().unwrap().ty;
                    if val.ty != *t0 {
                        return Err((
                            "VG-T0102",
                            format!("{} operands differ: {t0} vs {}", call.op_name, val.ty),
                        ));
                    }
                }
                Ok(())
            }
            "EQ" | "NE" => {
                reject_eh(val)?;
                if is_f16_or_bf16(&val.ty) {
                    return Err((
                        "VG-T0105",
                        "f16 and bf16 cannot be compared directly".into(),
                    ));
                }
                let valid = val.ty == Type::Bool || val.ty.is_int() || is_mvp_arith_float(&val.ty);
                if !valid {
                    return Err((
                        "VG-T0101",
                        format!("{} requires integer, f32/f64, or Bool", call.op_name),
                    ));
                }
                if pos == 1 {
                    let t0 = &call.args[0].val.as_ref().unwrap().ty;
                    if val.ty != *t0 {
                        return Err((
                            "VG-T0102",
                            format!("{} operands differ: {t0} vs {}", call.op_name, val.ty),
                        ));
                    }
                } else if pos > 1 {
                    return Err(("VG-S0103", format!("{} takes 2 arguments", call.op_name)));
                }
                Ok(())
            }
            "LT" | "LE" | "GT" | "GE" => {
                reject_eh(val)?;
                if is_f16_or_bf16(&val.ty) {
                    return Err((
                        "VG-T0105",
                        "f16 and bf16 cannot be compared directly".into(),
                    ));
                }
                let valid = val.ty.is_int() || is_mvp_arith_float(&val.ty);
                if !valid {
                    return Err((
                        "VG-T0101",
                        format!("{} requires integer or f32/f64", call.op_name),
                    ));
                }
                let mode_ok = match call.mode.as_deref() {
                    Some("s") => matches!(val.ty, Type::SInt(_)),
                    Some("u") => matches!(val.ty, Type::UInt(_)),
                    Some("f") => is_mvp_arith_float(&val.ty),
                    _ => false,
                };
                if !mode_ok {
                    return Err((
                        "VG-T0103",
                        format!("{} mode does not match {}", call.op_name, val.ty),
                    ));
                }
                if pos == 1 {
                    let t0 = &call.args[0].val.as_ref().unwrap().ty;
                    if val.ty != *t0 {
                        return Err((
                            "VG-T0102",
                            format!("{} operands differ: {t0} vs {}", call.op_name, val.ty),
                        ));
                    }
                } else if pos > 1 {
                    return Err(("VG-S0103", format!("{} takes 2 arguments", call.op_name)));
                }
                Ok(())
            }
            "BAND" | "BOR" | "BNOT" => {
                reject_eh(val)?;
                if val.ty != Type::Bool {
                    Err((
                        "VG-T0101",
                        format!("{} requires Bool, found {}", call.op_name, val.ty),
                    ))
                } else {
                    Ok(())
                }
            }
            "INDEX" => {
                reject_eh(val)?;
                match pos {
                    0 => {
                        let (Type::Array(e, _) | Type::Slice(e)) = &val.ty else {
                            return Err((
                                "VG-T0101",
                                format!("INDEX requires Array or Slice, found {}", val.ty),
                            ));
                        };
                        if let Some(ann) = &call.binds[0].ann
                            && *ann != **e
                        {
                            return Err(("VG-T0115", format!("annotated {ann}, element is {e}")));
                        }
                        Ok(())
                    }
                    1 => {
                        if val.ty != Type::UInt(64) {
                            Err((
                                "VG-T0112",
                                format!("INDEX index must be u64, found {}", val.ty),
                            ))
                        } else {
                            Ok(())
                        }
                    }
                    _ => Err(("VG-S0103", "INDEX takes 2 arguments".into())),
                }
            }
            "UPDATE" => {
                reject_eh(val)?;
                match pos {
                    0 => {
                        match &val.ty {
                            Type::Array(..) => {}
                            Type::Slice(_) => {
                                let name_already_used = name_opt.is_some_and(|nm| {
                                    n.used_counts.get(nm).copied().unwrap_or(0) > 0
                                });
                                let can_in_place = val.owns_fresh_arena_slice && !name_already_used;
                                if !can_in_place && (n.effects.0 & EffectMask::ARENA == 0) {
                                    return Err((
                                        "VG-E0106",
                                        "copying UPDATE of a slice requires ARENA in EFFECTS"
                                            .into(),
                                    ));
                                }
                            }
                            _ => {
                                return Err((
                                    "VG-T0101",
                                    format!("UPDATE requires Array or Slice, found {}", val.ty),
                                ));
                            }
                        }
                        if let Some(ann) = &call.binds[0].ann
                            && *ann != val.ty
                        {
                            return Err((
                                "VG-T0115",
                                format!("annotated {ann}, operand is {}", val.ty),
                            ));
                        }
                        Ok(())
                    }
                    1 => {
                        if val.ty != Type::UInt(64) {
                            Err((
                                "VG-T0112",
                                format!("UPDATE index must be u64, found {}", val.ty),
                            ))
                        } else {
                            Ok(())
                        }
                    }
                    2 => {
                        let elem = call.args[0].val.as_ref().unwrap().ty.elem().unwrap();
                        if val.ty != *elem {
                            Err((
                                "VG-T0101",
                                format!("UPDATE value expected {elem}, found {}", val.ty),
                            ))
                        } else {
                            Ok(())
                        }
                    }
                    _ => Err(("VG-S0103", "UPDATE takes 3 arguments".into())),
                }
            }
            "LEN" => {
                reject_eh(val)?;
                if pos > 0 {
                    return Err(("VG-S0103", "LEN takes 1 argument".into()));
                }
                if !matches!(val.ty, Type::Array(..) | Type::Slice(_)) {
                    Err((
                        "VG-T0101",
                        format!("LEN requires Array or Slice, found {}", val.ty),
                    ))
                } else {
                    Ok(())
                }
            }
            "SLICE" => {
                reject_eh(val)?;
                match pos {
                    0 => {
                        let (Type::Array(e, _) | Type::Slice(e)) = &val.ty else {
                            return Err((
                                "VG-T0101",
                                format!("SLICE requires Array or Slice, found {}", val.ty),
                            ));
                        };
                        let res = Type::Slice(e.clone());
                        if let Some(ann) = &call.binds[0].ann
                            && *ann != res
                        {
                            return Err((
                                "VG-T0115",
                                format!("annotated {ann}, SLICE produces {res}"),
                            ));
                        }
                        Ok(())
                    }
                    1 | 2 => {
                        if val.ty != Type::UInt(64) {
                            Err((
                                "VG-T0112",
                                format!("SLICE bound must be u64, found {}", val.ty),
                            ))
                        } else {
                            Ok(())
                        }
                    }
                    _ => Err(("VG-S0103", "SLICE takes 3 arguments".into())),
                }
            }
            "CONCAT" => {
                reject_eh(val)?;
                match pos {
                    0 => {
                        if !matches!(val.ty, Type::Array(..) | Type::Slice(_)) {
                            return Err((
                                "VG-T0101",
                                format!("CONCAT requires Array or Slice, found {}", val.ty),
                            ));
                        }
                        Ok(())
                    }
                    1 => {
                        if !matches!(val.ty, Type::Array(..) | Type::Slice(_)) {
                            return Err((
                                "VG-T0101",
                                format!("CONCAT requires Array or Slice, found {}", val.ty),
                            ));
                        }
                        let t0 = &call.args[0].val.as_ref().unwrap().ty;
                        let e0 = t0.elem().unwrap();
                        let e1 = val.ty.elem().unwrap();
                        if e0 != e1 {
                            return Err((
                                "VG-T0102",
                                format!("CONCAT element types differ: {e0} vs {e1}"),
                            ));
                        }
                        let res_ty = match (t0, &val.ty) {
                            (Type::Array(_, a), Type::Array(_, b)) => {
                                let Some(sum) = a.checked_add(*b) else {
                                    return Err((
                                        "VG-T0118",
                                        "CONCAT array length exceeds 2^32 - 1".into(),
                                    ));
                                };
                                let rt = Type::Array(Box::new(e0.clone()), sum);
                                validate_type(&rt, false, 1, Span::default())
                                    .map_err(|d| (d.code, d.message))?;
                                rt
                            }
                            _ => {
                                if n.effects.0 & EffectMask::ARENA == 0 {
                                    return Err((
                                        "VG-E0106",
                                        "CONCAT producing a Slice requires ARENA in EFFECTS".into(),
                                    ));
                                }
                                Type::Slice(Box::new(e0.clone()))
                            }
                        };
                        if let Some(ann) = &call.binds[0].ann
                            && *ann != res_ty
                        {
                            return Err((
                                "VG-T0115",
                                format!("annotated {ann}, CONCAT produces {res_ty}"),
                            ));
                        }
                        Ok(())
                    }
                    _ => Err(("VG-S0103", "CONCAT takes 2 arguments".into())),
                }
            }
            "SIMD_MAP" => {
                if pos == 0 {
                    return Err((
                        "VG-S0109",
                        "SIMD_MAP first argument must be a kernel label".into(),
                    ));
                }
                reject_eh(val)?;
                let kernel = &call.callees_vec()[0];
                if pos == 1 {
                    let elem = match &val.ty {
                        Type::Array(e, _) | Type::Tensor(e, _) | Type::Slice(e) => e.as_ref(),
                        _ => {
                            return Err((
                                "VG-T0101",
                                format!(
                                    "SIMD_MAP requires Array, Tensor, or Slice, found {}",
                                    val.ty
                                ),
                            ));
                        }
                    };
                    if kernel.fn_type.params.is_empty() || *elem != kernel.fn_type.params[0] {
                        return Err((
                            "VG-T0106",
                            "SIMD_MAP element type does not match kernel parameter 0".into(),
                        ));
                    }
                    let out_e = Box::new(kernel.fn_type.results[0].clone());
                    let res_ty = match &val.ty {
                        Type::Array(_, len) => Type::Array(out_e, *len),
                        Type::Tensor(_, dims) => Type::Tensor(out_e, dims.clone()),
                        Type::Slice(_) => {
                            if n.effects.0 & EffectMask::ARENA == 0 {
                                return Err((
                                    "VG-E0106",
                                    "SIMD_MAP over a Slice requires ARENA in EFFECTS".into(),
                                ));
                            }
                            Type::Slice(out_e)
                        }
                        _ => unreachable!(),
                    };
                    validate_type(&res_ty, false, 1, Span::default())
                        .map_err(|d| (d.code, d.message))?;
                    if let Some(ann) = &call.binds[0].ann
                        && *ann != res_ty
                    {
                        return Err((
                            "VG-T0115",
                            format!("annotated {ann}, SIMD_MAP produces {res_ty}"),
                        ));
                    }
                    Ok(())
                } else {
                    let Some(pt) = kernel.fn_type.params.get(pos - 1) else {
                        return Err((
                            "VG-T0106",
                            "too many broadcast arguments for SIMD_MAP kernel".into(),
                        ));
                    };
                    if val.ty != *pt {
                        Err((
                            "VG-T0106",
                            format!("SIMD_MAP broadcast expected {pt}, found {}", val.ty),
                        ))
                    } else {
                        Ok(())
                    }
                }
            }
            "FOLD_BOUNDED" => {
                if pos == 0 {
                    return Err((
                        "VG-S0109",
                        "FOLD_BOUNDED first argument must be a kernel label".into(),
                    ));
                }
                reject_eh(val)?;
                let kernel = &call.callees_vec()[0];
                if pos == 1 {
                    let elem = match &val.ty {
                        Type::Array(e, _) | Type::Slice(e) => e.as_ref(),
                        _ => {
                            return Err((
                                "VG-T0101",
                                format!("FOLD_BOUNDED requires Array or Slice, found {}", val.ty),
                            ));
                        }
                    };
                    if kernel.fn_type.params.len() < 2 || *elem != kernel.fn_type.params[1] {
                        return Err((
                            "VG-T0106",
                            "FOLD_BOUNDED element type does not match kernel".into(),
                        ));
                    }
                    Ok(())
                } else {
                    let Some(pt) = kernel.fn_type.params.get(pos) else {
                        return Err((
                            "VG-T0106",
                            "too many broadcast arguments for FOLD_BOUNDED kernel".into(),
                        ));
                    };
                    if val.ty != *pt {
                        Err((
                            "VG-T0106",
                            format!("FOLD_BOUNDED broadcast expected {pt}, found {}", val.ty),
                        ))
                    } else {
                        Ok(())
                    }
                }
            }
            "GENERATE" => {
                if pos == 0 {
                    return Err((
                        "VG-S0109",
                        "GENERATE first argument must be a kernel label".into(),
                    ));
                }
                reject_eh(val)?;
                let kernel = &call.callees_vec()[0];
                if pos == 1 {
                    if val.ty != Type::UInt(64) {
                        return Err((
                            "VG-T0112",
                            format!("GENERATE count must be u64, found {}", val.ty),
                        ));
                    }
                    let out_e = Box::new(kernel.fn_type.results[0].clone());
                    let res_ty = if let Some(cnt) = val.const_u64 {
                        let Ok(n32) = u32::try_from(cnt) else {
                            return Err((
                                "VG-T0118",
                                "GENERATE static count exceeds 2^32 - 1".into(),
                            ));
                        };
                        let rt = Type::Array(out_e, n32);
                        validate_type(&rt, false, 1, Span::default())
                            .map_err(|d| (d.code, d.message))?;
                        rt
                    } else {
                        if n.effects.0 & EffectMask::ARENA == 0 {
                            return Err((
                                "VG-E0106",
                                "dynamic GENERATE requires ARENA in EFFECTS".into(),
                            ));
                        }
                        Type::Slice(out_e)
                    };
                    if let Some(ann) = &call.binds[0].ann
                        && *ann != res_ty
                    {
                        return Err((
                            "VG-T0115",
                            format!("annotated {ann}, GENERATE produces {res_ty}"),
                        ));
                    }
                    Ok(())
                } else {
                    let Some(pt) = kernel.fn_type.params.get(pos - 1) else {
                        return Err((
                            "VG-T0106",
                            "too many broadcast arguments for GENERATE kernel".into(),
                        ));
                    };
                    if val.ty != *pt {
                        Err((
                            "VG-T0106",
                            format!("GENERATE broadcast expected {pt}, found {}", val.ty),
                        ))
                    } else {
                        Ok(())
                    }
                }
            }
            "ASSERT_GUARD" => {
                if pos == 0 {
                    reject_eh(val)?;
                    if val.ty != Type::Bool {
                        return Err((
                            "VG-T0111",
                            format!("ASSERT_GUARD condition must be Bool, found {}", val.ty),
                        ));
                    }
                    return Ok(());
                }
                if pos == 1 {
                    return Err((
                        "VG-S0109",
                        "ASSERT_GUARD second argument must be a branch label".into(),
                    ));
                }
                let callee = &call.callees_vec()[0];
                let a_idx = pos - 2;
                let Some(pt) = callee.fn_type.params.get(a_idx) else {
                    return Err((
                        "VG-T0109",
                        "too many arguments for ASSERT_GUARD branch".into(),
                    ));
                };
                if val.ty != *pt {
                    return Err((
                        "VG-T0109",
                        format!(
                            "ASSERT_GUARD argument {a_idx} expected {pt}, found {}",
                            val.ty
                        ),
                    ));
                }
                if let Type::Handle(hk) = pt
                    && val.valid_handle != Some(*hk)
                {
                    return Err((
                        "VG-T0119",
                        "ASSERT_GUARD handle must originate from an input parameter".into(),
                    ));
                }
                Ok(())
            }
            "CALL_HASH" => {
                if pos == 0 {
                    return Err((
                        "VG-S0109",
                        "CALL_HASH first argument must be a callee label".into(),
                    ));
                }
                let callee = &call.callees_vec()[0];
                let a_idx = pos - 1;
                let Some(pt) = callee.fn_type.params.get(a_idx) else {
                    return Err(("VG-T0109", "too many arguments for CALL_HASH callee".into()));
                };
                if val.ty != *pt {
                    return Err((
                        "VG-T0109",
                        format!("CALL_HASH argument {a_idx} expected {pt}, found {}", val.ty),
                    ));
                }
                if let Type::Handle(hk) = pt
                    && val.valid_handle != Some(*hk)
                {
                    return Err((
                        "VG-T0119",
                        "CALL_HASH handle must originate from an input parameter".into(),
                    ));
                }
                Ok(())
            }
            "FILE_READ" => match pos {
                0 => {
                    if val.ty != Type::Eff {
                        Err((
                            "VG-T0101",
                            format!("FILE_READ operand 0 must be Eff, found {}", val.ty),
                        ))
                    } else {
                        Ok(())
                    }
                }
                1 => {
                    if val.ty != Type::Handle(Handle::FileIn) {
                        Err((
                            "VG-T0101",
                            format!("FILE_READ operand 1 must be FileIn, found {}", val.ty),
                        ))
                    } else if val.valid_handle != Some(Handle::FileIn) {
                        Err((
                            "VG-T0119",
                            "FILE_READ handle must come from an input parameter".into(),
                        ))
                    } else {
                        Ok(())
                    }
                }
                2 => {
                    if val.ty != Type::UInt(64) {
                        Err((
                            "VG-T0112",
                            format!("FILE_READ max size must be u64, found {}", val.ty),
                        ))
                    } else {
                        Ok(())
                    }
                }
                _ => Err(("VG-S0103", "FILE_READ takes 3 arguments".into())),
            },
            "FILE_WRITE" => match pos {
                0 => {
                    if val.ty != Type::Eff {
                        Err((
                            "VG-T0101",
                            format!("FILE_WRITE operand 0 must be Eff, found {}", val.ty),
                        ))
                    } else {
                        Ok(())
                    }
                }
                1 => {
                    if val.ty != Type::Handle(Handle::FileOut) {
                        Err((
                            "VG-T0101",
                            format!("FILE_WRITE operand 1 must be FileOut, found {}", val.ty),
                        ))
                    } else if val.valid_handle != Some(Handle::FileOut) {
                        Err((
                            "VG-T0119",
                            "FILE_WRITE handle must come from an input parameter".into(),
                        ))
                    } else {
                        Ok(())
                    }
                }
                2 => {
                    if val.ty != Type::Slice(Box::new(Type::UInt(8))) {
                        Err((
                            "VG-T0101",
                            format!("FILE_WRITE data must be Slice<u8>, found {}", val.ty),
                        ))
                    } else {
                        Ok(())
                    }
                }
                _ => Err(("VG-S0103", "FILE_WRITE takes 3 arguments".into())),
            },
            "CLOCK_READ" => match pos {
                0 => {
                    if val.ty != Type::Eff {
                        Err((
                            "VG-T0101",
                            format!("CLOCK_READ operand 0 must be Eff, found {}", val.ty),
                        ))
                    } else {
                        Ok(())
                    }
                }
                1 => {
                    if val.ty != Type::Handle(Handle::Clock) {
                        Err((
                            "VG-T0101",
                            format!("CLOCK_READ operand 1 must be Clock, found {}", val.ty),
                        ))
                    } else if val.valid_handle != Some(Handle::Clock) {
                        Err((
                            "VG-T0119",
                            "CLOCK_READ handle must come from an input parameter".into(),
                        ))
                    } else {
                        Ok(())
                    }
                }
                _ => Err(("VG-S0103", "CLOCK_READ takes 2 arguments".into())),
            },
            _ => Ok(()),
        }
    }

    fn label_fits_stmt_slot(
        &self,
        call: &StmtCall,
        label: &str,
        info: Option<&LabelInfo>,
    ) -> Result<(), (&'static str, String)> {
        let n = self.node.as_ref().unwrap();
        if let Some(arg_name) = call.cur_arg_name.as_deref() {
            match (call.info.shape, arg_name) {
                (Shape::Fold, "invariant") => {
                    let Some(inv) = n.invariants.get(label) else {
                        return Err((
                            "VG-S0107",
                            format!("@{label} names no INVARIANT of this node"),
                        ));
                    };
                    let acc_ty = &call.callees_vec()[0].fn_type.results[0];
                    if let Some(prev) = &inv.bound_acc_ty
                        && prev != acc_ty
                    {
                        return Err((
                            "VG-C0101",
                            "INVARIANT used by folds with different accumulator types".into(),
                        ));
                    }
                    return Ok(());
                }
                (Shape::Guard, "else") => {
                    let Some(info) = info else {
                        return Err(("VG-S0005", format!("unknown label @{label}")));
                    };
                    if info.kind == ItemKind::Spec {
                        return Err(("VG-S0110", "SPEC cannot be called by ASSERT_GUARD".into()));
                    }
                    if info.effects.0 & !n.effects.0 != 0 {
                        return Err((
                            "VG-E0101",
                            "else branch requires effects missing from EFFECTS".into(),
                        ));
                    }
                    let then_info = &call.callees_vec()[0];
                    if info.fn_type != then_info.fn_type {
                        return Err((
                            "VG-T0110",
                            "ASSERT_GUARD branches have different signatures".into(),
                        ));
                    }
                    return Ok(());
                }
                _ => {
                    return Err((
                        "VG-T0101",
                        format!("@{label} is a label where a value is expected"),
                    ));
                }
            }
        }
        let pos = call.args.len();
        let is_label_pos = match call.info.shape {
            Shape::Kernel | Shape::Fold | Shape::Call => pos == 0,
            Shape::Guard => pos == 1,
            _ => false,
        };
        if !is_label_pos {
            return Err((
                "VG-T0101",
                format!("@{label} is a label where a value is expected"),
            ));
        }
        if label == n.label {
            return Err((
                "VG-S0007",
                format!("@{label} calls itself; recursion is [ALPHA]"),
            ));
        }
        let Some(info) = info else {
            return Err(("VG-S0005", format!("unknown label @{label}")));
        };
        if info.kind == ItemKind::Spec {
            return Err((
                "VG-S0110",
                format!("@{label} is a SPEC; call via spec:{label}(...)"),
            ));
        }
        match call.info.shape {
            Shape::Kernel => {
                if info.effects.0 != 0 || info.fn_type.results.len() != 1 {
                    return Err((
                        "VG-T0107",
                        "combinator kernel must be pure with 1 result".into(),
                    ));
                }
                if !is_valid_kernel_label(info) {
                    return Err((
                        "VG-T0107",
                        "combinator kernel must use fixed-size types".into(),
                    ));
                }
                if call.op_name == "GENERATE"
                    && info.fn_type.params.first() != Some(&Type::UInt(64))
                {
                    return Err((
                        "VG-T0106",
                        "GENERATE kernel must take u64 as parameter 0".into(),
                    ));
                }
            }
            Shape::Fold => {
                if info.effects.0 != 0
                    || info.fn_type.results.len() != 1
                    || !is_valid_kernel_label(info)
                {
                    return Err((
                        "VG-T0107",
                        "FOLD_BOUNDED kernel must be pure fixed-size single-result".into(),
                    ));
                }
                if info.fn_type.params.len() < 2
                    || info.fn_type.params[0] != info.fn_type.results[0]
                {
                    return Err((
                        "VG-T0106",
                        "FOLD_BOUNDED kernel must have shape (A, T, B...) -> (A)".into(),
                    ));
                }
                if let Some(ann) = &call.binds[0].ann
                    && *ann != info.fn_type.results[0]
                {
                    return Err((
                        "VG-T0115",
                        format!(
                            "annotated {ann}, FOLD_BOUNDED returns {}",
                            info.fn_type.results[0]
                        ),
                    ));
                }
            }
            Shape::Guard | Shape::Call => {
                if info.effects.0 & !n.effects.0 != 0 {
                    return Err((
                        "VG-E0101",
                        "callee requires effects missing from EFFECTS".into(),
                    ));
                }
                if info.fn_type.results.len() != call.binds.len() {
                    return Err((
                        "VG-S0106",
                        format!(
                            "callee produces {} results, {} ports bound",
                            info.fn_type.results.len(),
                            call.binds.len()
                        ),
                    ));
                }
                for (b, rt) in call.binds.iter().zip(&info.fn_type.results) {
                    if let Some(ann) = &b.ann
                        && ann != rt
                    {
                        return Err((
                            "VG-T0115",
                            format!("port annotated {ann}, callee returns {rt}"),
                        ));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn accept_stmt_val(
        &mut self,
        mut call: StmtCall,
        val: ValMeta,
        raw: &str,
    ) -> Result<StmtCall, Diagnostic> {
        // Resolve named argument vs positional argument order
        let name_opt = if !val.is_literal { Some(raw) } else { None };
        // Check Eff linearity & slice in-place consumption when reading a named port/param
        if let Some(nm) = name_opt {
            let n = self.node.as_mut().unwrap();
            if val.ty == Type::Eff && !n.eff_live.remove(nm) {
                return Err(self.err("VG-E0102", raw, format!("Eff value `{nm}` used twice")));
            }
            if n.consumed_in_place.contains(nm) && (n.effects.0 & EffectMask::ARENA == 0) {
                return Err(self.err(
                    "VG-E0106",
                    raw,
                    format!("second use of `{nm}` prevents in-place slice UPDATE without ARENA"),
                ));
            }
        }
        self.val_fits_stmt_slot(&call, name_opt, &val)
            .map_err(|(c, m)| self.err(c, raw, m))?;

        if let Some(nm) = name_opt {
            let n = self.node.as_mut().unwrap();
            let prev = *n.used_counts.get(nm).unwrap_or(&0);
            n.used_counts.insert(nm.to_string(), prev + 1);
            if call.op_name == "UPDATE"
                && call.args.is_empty()
                && matches!(val.ty, Type::Slice(_))
                && val.owns_fresh_arena_slice
                && prev == 0
                && (n.effects.0 & EffectMask::ARENA == 0)
            {
                n.consumed_in_place.insert(nm.to_string());
            }
        }

        let arg_name = call.cur_arg_name.take();
        if let Some(an) = &arg_name {
            call.named_seen.insert(an.clone());
        }
        call.args.push(StmtArg {
            name: arg_name,
            val: Some(val),
            callee: None,
        });
        Ok(call)
    }

    fn accept_stmt_label(
        &mut self,
        mut call: StmtCall,
        label: &str,
        raw: &str,
    ) -> Result<StmtCall, Diagnostic> {
        let info = self.labels.get(label).cloned();
        self.label_fits_stmt_slot(&call, label, info.as_ref())
            .map_err(|(c, m)| self.err(c, raw, m))?;
        let arg_name = call.cur_arg_name.take();
        if let Some(an) = &arg_name {
            call.named_seen.insert(an.clone());
        }
        if arg_name.as_deref() == Some("invariant") {
            let acc_ty = call.callees_vec()[0].fn_type.results[0].clone();
            self.node
                .as_mut()
                .unwrap()
                .invariants
                .get_mut(label)
                .unwrap()
                .bound_acc_ty = Some(acc_ty);
            call.args.push(StmtArg {
                name: arg_name,
                val: None,
                callee: None,
            });
        } else {
            call.args.push(StmtArg {
                name: arg_name,
                val: None,
                callee: Some((label.to_string(), info.unwrap())),
            });
        }
        Ok(call)
    }

    fn can_close_stmt_args(&self, call: &StmtCall) -> bool {
        let pos_count = call.args.iter().filter(|a| a.name.is_none()).count();
        match call.info.shape {
            Shape::Fixed(m) => pos_count == m as usize,
            Shape::AtLeast(m) => pos_count >= m as usize,
            Shape::Kernel => {
                if pos_count < 2 {
                    return false;
                }
                let k = &call.callees_vec()[0];
                pos_count == 1 + k.fn_type.params.len()
            }
            Shape::Fold => {
                if !call.named_seen.contains("init") || !call.named_seen.contains("invariant") {
                    return false;
                }
                let k = &call.callees_vec()[0];
                pos_count == k.fn_type.params.len()
            }
            Shape::Guard => {
                if !call.named_seen.contains("else") || pos_count < 2 {
                    return false;
                }
                let k = &call.callees_vec()[0];
                pos_count == 2 + k.fn_type.params.len()
            }
            Shape::Call => {
                if pos_count < 1 {
                    return false;
                }
                let k = &call.callees_vec()[0];
                pos_count == 1 + k.fn_type.params.len()
            }
        }
    }

    fn finish_stmt_call(&mut self, call: StmtCall, raw: &str) -> Result<(), Diagnostic> {
        let pos_count = call.args.iter().filter(|a| a.name.is_none()).count();
        let n_params = self.node.as_ref().unwrap().params.len();
        match call.info.shape {
            Shape::Fixed(m) if pos_count != m as usize => {
                return Err(self.err(
                    "VG-S0103",
                    raw,
                    format!("{} takes {m} arguments", call.op_name),
                ));
            }
            Shape::AtLeast(m) if pos_count < m as usize => {
                return Err(self.err(
                    "VG-S0103",
                    raw,
                    format!("{} takes at least {m} arguments", call.op_name),
                ));
            }
            Shape::Kernel => {
                if pos_count < 2 {
                    return Err(self.err(
                        "VG-S0103",
                        raw,
                        format!("{} needs kernel and input", call.op_name),
                    ));
                }
                let k = &call.callees_vec()[0];
                if pos_count != 1 + k.fn_type.params.len() {
                    return Err(self.err(
                        "VG-T0106",
                        raw,
                        "missing broadcast arguments for kernel",
                    ));
                }
            }
            Shape::Fold => {
                if !call.named_seen.contains("invariant") {
                    return Err(self.err("VG-S0107", raw, "FOLD_BOUNDED needs invariant=@I"));
                }
                if !call.named_seen.contains("init") {
                    return Err(self.err("VG-S0103", raw, "FOLD_BOUNDED needs init=..."));
                }
                let k = &call.callees_vec()[0];
                if pos_count != k.fn_type.params.len() {
                    return Err(self.err(
                        "VG-T0106",
                        raw,
                        "missing broadcast arguments for FOLD_BOUNDED",
                    ));
                }
            }
            Shape::Guard => {
                if !call.named_seen.contains("else") {
                    return Err(self.err("VG-S0103", raw, "ASSERT_GUARD needs else=@g"));
                }
                let k = &call.callees_vec()[0];
                if pos_count != 2 + k.fn_type.params.len() {
                    return Err(self.err(
                        "VG-T0109",
                        raw,
                        "wrong number of branch arguments for ASSERT_GUARD",
                    ));
                }
            }
            Shape::Call => {
                if pos_count < 1 {
                    return Err(self.err("VG-S0103", raw, "CALL_HASH needs a callee label"));
                }
                let k = &call.callees_vec()[0];
                if pos_count != 1 + k.fn_type.params.len() {
                    return Err(self.err(
                        "VG-T0109",
                        raw,
                        "wrong number of arguments for CALL_HASH",
                    ));
                }
            }
            _ => {}
        }

        // Compute result ValMeta(s) matching vg-types/src/check.rs
        let val_at = |i: usize| call.args[i].val.as_ref().unwrap();
        let make_local = |ty: Type, has_local_slice: bool, from_params: Vec<bool>| ValMeta {
            origin_local_frame: is_frame_value(&ty),
            ty,
            has_local_slice,
            owns_fresh_arena_slice: false,
            from_params,
            valid_handle: None,
            const_u64: None,
            is_literal: false,
        };
        let metas: Vec<ValMeta> = match call.op_name.as_str() {
            "IDENTITY" => vec![val_at(0).clone()],
            "TUPLE" => {
                let mut has_local = false;
                let mut fp = vec![false; n_params];
                let mut tys = Vec::new();
                for a in &call.args {
                    let v = a.val.as_ref().unwrap();
                    has_local |= v.has_local_slice;
                    for (d, &s) in fp.iter_mut().zip(&v.from_params) {
                        *d |= s;
                    }
                    tys.push(v.ty.clone());
                }
                vec![make_local(Type::Tuple(tys), has_local, fp)]
            }
            "PROJ" => {
                let m0 = val_at(0);
                let Type::Tuple(ms) = &m0.ty else {
                    unreachable!()
                };
                let j = val_at(1).const_u64.unwrap() as usize;
                let res_ty = ms[j].clone();
                vec![ValMeta {
                    origin_local_frame: m0.origin_local_frame && is_frame_value(&res_ty),
                    has_local_slice: m0.has_local_slice && type_contains_slice(&res_ty),
                    from_params: if type_contains_slice(&res_ty) {
                        m0.from_params.clone()
                    } else {
                        vec![false; n_params]
                    },
                    ty: res_ty,
                    owns_fresh_arena_slice: false,
                    valid_handle: None,
                    const_u64: None,
                    is_literal: false,
                }]
            }
            "SELECT" => {
                let (m1, m2) = (val_at(1), val_at(2));
                let mut fp = m1.from_params.clone();
                for (d, &s) in fp.iter_mut().zip(&m2.from_params) {
                    *d |= s;
                }
                vec![ValMeta {
                    ty: m1.ty.clone(),
                    origin_local_frame: m1.origin_local_frame || m2.origin_local_frame,
                    has_local_slice: m1.has_local_slice || m2.has_local_slice,
                    owns_fresh_arena_slice: false,
                    from_params: fp,
                    valid_handle: None,
                    const_u64: None,
                    is_literal: false,
                }]
            }
            "CAST" => vec![make_local(
                call.cast_ty.clone().unwrap(),
                false,
                vec![false; n_params],
            )],
            "AS_BYTES" => {
                let m0 = val_at(0);
                let has_local = if matches!(m0.ty, Type::Slice(_)) {
                    m0.has_local_slice
                } else {
                    m0.origin_local_frame || m0.has_local_slice
                };
                vec![ValMeta {
                    ty: Type::Slice(Box::new(Type::UInt(8))),
                    origin_local_frame: false,
                    has_local_slice: has_local,
                    owns_fresh_arena_slice: false,
                    from_params: m0.from_params.clone(),
                    valid_handle: None,
                    const_u64: None,
                    is_literal: false,
                }]
            }
            "OK" | "ERR" => {
                let m0 = val_at(0);
                let res_ty = call.binds[0].ann.clone().unwrap();
                vec![make_local(
                    res_ty,
                    m0.has_local_slice,
                    m0.from_params.clone(),
                )]
            }
            "IS_OK" => vec![make_local(Type::Bool, false, vec![false; n_params])],
            "GET_OK" | "GET_ERR" => {
                let m0 = val_at(0);
                let Type::Result(ok_t, err_t) = &m0.ty else {
                    unreachable!()
                };
                let res_ty = if call.op_name == "GET_OK" {
                    (**ok_t).clone()
                } else {
                    (**err_t).clone()
                };
                vec![ValMeta {
                    origin_local_frame: m0.origin_local_frame && is_frame_value(&res_ty),
                    has_local_slice: m0.has_local_slice && type_contains_slice(&res_ty),
                    from_params: if type_contains_slice(&res_ty) {
                        m0.from_params.clone()
                    } else {
                        vec![false; n_params]
                    },
                    ty: res_ty,
                    owns_fresh_arena_slice: false,
                    valid_handle: None,
                    const_u64: None,
                    is_literal: false,
                }]
            }
            "EQ" | "NE" | "LT" | "LE" | "GT" | "GE" | "BAND" | "BOR" | "BNOT" => {
                vec![make_local(Type::Bool, false, vec![false; n_params])]
            }
            "INDEX" => {
                let m0 = val_at(0);
                let elem = m0.ty.elem().unwrap().clone();
                vec![ValMeta {
                    origin_local_frame: m0.origin_local_frame && is_frame_value(&elem),
                    has_local_slice: m0.has_local_slice && type_contains_slice(&elem),
                    from_params: if type_contains_slice(&elem) {
                        m0.from_params.clone()
                    } else {
                        vec![false; n_params]
                    },
                    ty: elem,
                    owns_fresh_arena_slice: false,
                    valid_handle: None,
                    const_u64: None,
                    is_literal: false,
                }]
            }
            "UPDATE" => {
                let (m0, m2) = (val_at(0), val_at(2));
                match &m0.ty {
                    Type::Array(..) => {
                        let mut fp = m0.from_params.clone();
                        for (d, &s) in fp.iter_mut().zip(&m2.from_params) {
                            *d |= s;
                        }
                        vec![make_local(
                            m0.ty.clone(),
                            m0.has_local_slice || m2.has_local_slice,
                            fp,
                        )]
                    }
                    Type::Slice(e) => {
                        let elem_has_slice = type_contains_slice(e);
                        let mut fp = if elem_has_slice {
                            m0.from_params.clone()
                        } else {
                            vec![false; n_params]
                        };
                        if elem_has_slice {
                            for (d, &s) in fp.iter_mut().zip(&m2.from_params) {
                                *d |= s;
                            }
                        }
                        vec![ValMeta {
                            ty: m0.ty.clone(),
                            origin_local_frame: false,
                            has_local_slice: (elem_has_slice && m0.has_local_slice)
                                || m2.has_local_slice,
                            owns_fresh_arena_slice: true,
                            from_params: fp,
                            valid_handle: None,
                            const_u64: None,
                            is_literal: false,
                        }]
                    }
                    _ => unreachable!(),
                }
            }
            "LEN" => vec![make_local(Type::UInt(64), false, vec![false; n_params])],
            "SLICE" => {
                let m0 = val_at(0);
                let elem = m0.ty.elem().unwrap().clone();
                let has_local = if matches!(m0.ty, Type::Slice(_)) {
                    m0.has_local_slice
                } else {
                    m0.origin_local_frame || m0.has_local_slice
                };
                vec![ValMeta {
                    ty: Type::Slice(Box::new(elem)),
                    origin_local_frame: false,
                    has_local_slice: has_local,
                    owns_fresh_arena_slice: false,
                    from_params: m0.from_params.clone(),
                    valid_handle: None,
                    const_u64: None,
                    is_literal: false,
                }]
            }
            "CONCAT" => {
                let (m0, m1) = (val_at(0), val_at(1));
                let elem = m0.ty.elem().unwrap().clone();
                match (&m0.ty, &m1.ty) {
                    (Type::Array(_, a), Type::Array(_, b)) => {
                        let mut fp = m0.from_params.clone();
                        for (d, &s) in fp.iter_mut().zip(&m1.from_params) {
                            *d |= s;
                        }
                        vec![make_local(
                            Type::Array(Box::new(elem), a + b),
                            m0.has_local_slice || m1.has_local_slice,
                            fp,
                        )]
                    }
                    _ => {
                        let elem_has_slice = type_contains_slice(&elem);
                        let mut fp = vec![false; n_params];
                        if elem_has_slice {
                            for (d, &s) in fp.iter_mut().zip(&m0.from_params) {
                                *d |= s;
                            }
                            for (d, &s) in fp.iter_mut().zip(&m1.from_params) {
                                *d |= s;
                            }
                        }
                        vec![ValMeta {
                            ty: Type::Slice(Box::new(elem)),
                            origin_local_frame: false,
                            has_local_slice: elem_has_slice
                                && (m0.has_local_slice || m1.has_local_slice),
                            owns_fresh_arena_slice: true,
                            from_params: fp,
                            valid_handle: None,
                            const_u64: None,
                            is_literal: false,
                        }]
                    }
                }
            }
            "SIMD_MAP" => {
                let k = &call.callees_vec()[0];
                let out_e = Box::new(k.fn_type.results[0].clone());
                let m_xs = val_at(1);
                let (res_ty, alloc) = match &m_xs.ty {
                    Type::Array(_, len) => (Type::Array(out_e, *len), false),
                    Type::Tensor(_, dims) => (Type::Tensor(out_e, dims.clone()), false),
                    Type::Slice(_) => (Type::Slice(out_e), true),
                    _ => unreachable!(),
                };
                vec![ValMeta {
                    ty: res_ty,
                    origin_local_frame: !alloc,
                    has_local_slice: false,
                    owns_fresh_arena_slice: alloc,
                    from_params: vec![false; n_params],
                    valid_handle: None,
                    const_u64: None,
                    is_literal: false,
                }]
            }
            "FOLD_BOUNDED" => {
                let k = &call.callees_vec()[0];
                vec![make_local(
                    k.fn_type.results[0].clone(),
                    false,
                    vec![false; n_params],
                )]
            }
            "GENERATE" => {
                let k = &call.callees_vec()[0];
                let out_e = Box::new(k.fn_type.results[0].clone());
                let (res_ty, alloc) = if let Some(cnt) = val_at(1).const_u64 {
                    (Type::Array(out_e, cnt as u32), false)
                } else {
                    (Type::Slice(out_e), true)
                };
                vec![ValMeta {
                    ty: res_ty,
                    origin_local_frame: !alloc,
                    has_local_slice: false,
                    owns_fresh_arena_slice: alloc,
                    from_params: vec![false; n_params],
                    valid_handle: None,
                    const_u64: None,
                    is_literal: false,
                }]
            }
            "ASSERT_GUARD" | "CALL_HASH" => {
                let callees = call.callees_vec();
                let arg_start = if call.op_name == "ASSERT_GUARD" { 2 } else { 1 };
                let arg_vals: Vec<&ValMeta> = call.args[arg_start..]
                    .iter()
                    .filter(|a| a.name.is_none())
                    .map(|a| a.val.as_ref().unwrap())
                    .collect();
                let results = &callees[0].fn_type.results;
                let mut out = Vec::with_capacity(results.len());
                for (slot, res_ty) in results.iter().enumerate() {
                    let mut has_local = false;
                    let mut fp = vec![false; n_params];
                    let valid_handle = if let Type::Handle(hk) = res_ty {
                        let all_v = callees.iter().all(|c| {
                            c.result_handle_valid.get(slot).copied().flatten() == Some(*hk)
                        });
                        (all_v && arg_vals.iter().any(|av| av.valid_handle == Some(*hk)))
                            .then_some(*hk)
                    } else {
                        None
                    };
                    for c in &callees {
                        if type_contains_slice(res_ty) {
                            for (p, av) in arg_vals.iter().enumerate() {
                                let views_p = c
                                    .result_views_param
                                    .get(slot)
                                    .and_then(|v| v.get(p))
                                    .copied()
                                    .unwrap_or(false);
                                if views_p {
                                    if av.has_local_slice || av.origin_local_frame {
                                        has_local = true;
                                    }
                                    for (d, &s) in fp.iter_mut().zip(&av.from_params) {
                                        *d |= s;
                                    }
                                }
                            }
                        }
                    }
                    out.push(ValMeta {
                        ty: res_ty.clone(),
                        origin_local_frame: is_frame_value(res_ty),
                        has_local_slice: has_local,
                        owns_fresh_arena_slice: false,
                        from_params: fp,
                        valid_handle,
                        const_u64: None,
                        is_literal: false,
                    });
                }
                out
            }
            "FILE_READ" => vec![
                make_local(Type::Eff, false, vec![false; n_params]),
                ValMeta {
                    ty: Type::Result(
                        Box::new(Type::Slice(Box::new(Type::UInt(8)))),
                        Box::new(Type::UInt(32)),
                    ),
                    origin_local_frame: false,
                    has_local_slice: false,
                    owns_fresh_arena_slice: false,
                    from_params: vec![false; n_params],
                    valid_handle: None,
                    const_u64: None,
                    is_literal: false,
                },
            ],
            "FILE_WRITE" => vec![
                make_local(Type::Eff, false, vec![false; n_params]),
                make_local(
                    Type::Result(Box::new(Type::UInt(64)), Box::new(Type::UInt(32))),
                    false,
                    vec![false; n_params],
                ),
            ],
            "CLOCK_READ" => vec![
                make_local(Type::Eff, false, vec![false; n_params]),
                make_local(Type::UInt(64), false, vec![false; n_params]),
            ],
            _ => vec![make_local(
                val_at(0).ty.clone(),
                false,
                vec![false; n_params],
            )],
        };

        let n = self.node.as_mut().unwrap();
        for (b, m) in call.binds.iter().zip(metas) {
            if let Some(ann) = &b.ann
                && *ann != m.ty
            {
                return Err(self.err(
                    "VG-T0115",
                    raw,
                    format!("port %{} annotated {ann}, op returns {}", b.port, m.ty),
                ));
            }
            let pname = format!("%{}", b.port);
            if m.ty == Type::Eff {
                n.eff_live.insert(pname.clone());
            }
            n.values.insert(pname, m);
        }
        Ok(())
    }

    fn accept_return_val(
        &mut self,
        idx: usize,
        val: ValMeta,
        name_opt: Option<&str>,
        raw: &str,
    ) -> Result<(), Diagnostic> {
        let (res_ty, res_len) = {
            let n = self.node.as_ref().unwrap();
            (n.results.get(idx).cloned(), n.results.len())
        };
        let Some(res_ty) = res_ty else {
            return Err(self.err(
                "VG-T0108",
                raw,
                format!("RETURN has more values than SIGNATURE results ({res_len})"),
            ));
        };
        if val.ty != res_ty {
            return Err(self.err(
                "VG-T0108",
                raw,
                format!("RETURN value {idx} has type {}, expected {res_ty}", val.ty),
            ));
        }
        if let Type::Handle(hk) = res_ty
            && val.valid_handle != Some(hk)
        {
            return Err(self.err(
                "VG-T0119",
                raw,
                format!(
                    "returned handle {} does not originate from an input handle",
                    hk.name()
                ),
            ));
        }
        if type_contains_slice(&res_ty) && val.has_local_slice {
            return Err(self.err(
                "VG-T0120",
                raw,
                format!("returned slice at result {idx} may view a value of this node's frame"),
            ));
        }
        if let Some(nm) = name_opt {
            let (eff_bad, in_place_bad) = {
                let n = self.node.as_mut().unwrap();
                let eb = val.ty == Type::Eff && !n.eff_live.remove(nm);
                let ipb =
                    n.consumed_in_place.contains(nm) && (n.effects.0 & EffectMask::ARENA == 0);
                (eb, ipb)
            };
            if eff_bad {
                return Err(self.err("VG-E0102", raw, format!("Eff value `{nm}` used twice")));
            }
            if in_place_bad {
                return Err(self.err(
                    "VG-E0106",
                    raw,
                    format!("second use of `{nm}` prevents in-place slice UPDATE without ARENA"),
                ));
            }
            let n = self.node.as_mut().unwrap();
            *n.used_counts.entry(nm.to_string()).or_default() += 1;
        }
        let n = self.node.as_mut().unwrap();
        if type_contains_slice(&res_ty) {
            n.result_views_param.push(val.from_params);
        } else {
            n.result_views_param.push(vec![false; n.params.len()]);
        }
        n.result_handle_valid.push(val.valid_handle);
        Ok(())
    }

    fn return_val_fits(&self, idx: usize, name_opt: Option<&str>, val: &ValMeta) -> bool {
        let n = self.node.as_ref().unwrap();
        let Some(res_ty) = n.results.get(idx) else {
            return false;
        };
        if val.ty != *res_ty {
            return false;
        }
        if let Type::Handle(hk) = res_ty
            && val.valid_handle != Some(*hk)
        {
            return false;
        }
        if type_contains_slice(res_ty) && val.has_local_slice {
            return false;
        }
        if let Some(nm) = name_opt {
            if val.ty == Type::Eff && !n.eff_live.contains(nm) {
                return false;
            }
            if n.consumed_in_place.contains(nm) && (n.effects.0 & EffectMask::ARENA == 0) {
                return false;
            }
        }
        true
    }
}

impl StmtCall {
    fn callees_vec(&self) -> Vec<&LabelInfo> {
        self.args
            .iter()
            .filter_map(|a| a.callee.as_ref().map(|(_, i)| i))
            .collect()
    }
}

fn call_allows_lits(call: &StmtCall) -> bool {
    match call.info.shape {
        Shape::Kernel | Shape::Call => !call.args.is_empty(),
        Shape::Fold => {
            call.cur_arg_name.as_deref() == Some("init")
                || (call.cur_arg_name.is_none() && !call.args.is_empty())
        }
        Shape::Guard => call.cur_arg_name.is_none() && call.args.len() != 1,
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Main statement / declaration step & allowed
// ---------------------------------------------------------------------------

impl Prefix {
    fn step_main(&mut self, tok: &Tok, raw: &str) -> Result<(), Diagnostic> {
        let cur = std::mem::replace(&mut self.state, State::TopLevel);
        match cur {
            State::ModuleKw => {
                if *tok == Tok::Kw("module") {
                    self.state = State::ModuleName;
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `module`, found `{raw}`")))
                }
            }
            State::ModuleName => match tok {
                Tok::Ident(_) => {
                    self.state = State::ModuleSemi;
                    Ok(())
                }
                _ => Err(self.kw_or_ident_err(tok, raw, "a module name")),
            },
            State::ModuleSemi => {
                if *tok == Tok::Punct(";") {
                    self.state = State::TopLevel;
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `;`, found `{raw}`")))
                }
            }
            State::TopLevel => match tok {
                Tok::Kw("@MESH_QUERY") => {
                    self.query_params.clear();
                    self.query_results.clear();
                    self.query_effects = EffectMask(0);
                    self.query_label.clear();
                    self.state = State::QueryLabel;
                    Ok(())
                }
                Tok::Kw("NODE") => {
                    self.state = State::ItemLabel {
                        kind: ItemKind::Node,
                    };
                    Ok(())
                }
                Tok::Kw("SPEC") => {
                    self.state = State::ItemLabel {
                        kind: ItemKind::Spec,
                    };
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `@MESH_QUERY`, `NODE`, or `SPEC`, found `{raw}`"),
                )),
            },

            // @MESH_QUERY
            State::QueryLabel => match tok {
                Tok::Label(l) => {
                    if self.labels.contains_key(l) {
                        return Err(self.err("VG-S0006", raw, format!("label @{l} defined twice")));
                    }
                    self.query_label = l.clone();
                    self.state = State::QueryLBrace;
                    Ok(())
                }
                _ => Err(self.err("VG-P0001", raw, format!("expected label, found `{raw}`"))),
            },
            State::QueryLBrace => expect_punct(self, tok, raw, "{", State::QuerySigKw),
            State::QuerySigKw => expect_kw(self, tok, raw, "signature", State::QuerySigColon),
            State::QuerySigColon => expect_punct(self, tok, raw, ":", State::QuerySigLParen),
            State::QuerySigLParen => expect_punct(
                self,
                tok,
                raw,
                "(",
                State::QuerySigParamOrClose { first: true },
            ),
            State::QuerySigParamOrClose { first } => {
                if first && *tok == Tok::Punct(")") {
                    self.state = State::QueryArrow;
                    Ok(())
                } else {
                    self.push_type(false, TypeCont::QueryParam);
                    self.step_type(tok, raw)
                }
            }
            State::QuerySigCommaOrClose => match tok {
                Tok::Punct(",") => {
                    self.state = State::QuerySigParamOrClose { first: false };
                    Ok(())
                }
                Tok::Punct(")") => {
                    self.state = State::QueryArrow;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `)`, found `{raw}`"),
                )),
            },
            State::QueryArrow => expect_punct(self, tok, raw, "->", State::QueryResLParen),
            State::QueryResLParen => expect_punct(self, tok, raw, "(", State::QueryResCommaOrClose),
            State::QueryResCommaOrClose => {
                if self.query_results.is_empty() {
                    self.push_type(false, TypeCont::QueryResult);
                    self.step_type(tok, raw)
                } else {
                    match tok {
                        Tok::Punct(",") => {
                            self.push_type(false, TypeCont::QueryResult);
                            Ok(())
                        }
                        Tok::Punct(")") => {
                            self.state = State::QueryResSemi;
                            Ok(())
                        }
                        _ => Err(self.err(
                            "VG-P0001",
                            raw,
                            format!("expected `,` or `)`, found `{raw}`"),
                        )),
                    }
                }
            }
            State::QueryResSemi => expect_punct(self, tok, raw, ";", State::QueryEffKw),
            State::QueryEffKw => expect_kw(self, tok, raw, "effects", State::QueryEffColon),
            State::QueryEffColon => expect_punct(self, tok, raw, ":", State::QueryEffLBracket),
            State::QueryEffLBracket => expect_punct(
                self,
                tok,
                raw,
                "[",
                State::QueryEffElemOrClose { first: true },
            ),
            State::QueryEffElemOrClose { first } => {
                if first && *tok == Tok::Punct("]") {
                    self.state = State::QueryEffSemi;
                    Ok(())
                } else if let Tok::Kw(k) = tok
                    && EFFECTS.contains(k)
                {
                    self.query_effects.0 |= EffectMask::bit(k).unwrap();
                    self.state = State::QueryEffCommaOrClose;
                    Ok(())
                } else {
                    Err(self.err(
                        "VG-P0001",
                        raw,
                        format!("expected effect name, found `{raw}`"),
                    ))
                }
            }
            State::QueryEffCommaOrClose => match tok {
                Tok::Punct(",") => {
                    self.state = State::QueryEffElemOrClose { first: false };
                    Ok(())
                }
                Tok::Punct("]") => {
                    self.state = State::QueryEffSemi;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `]`, found `{raw}`"),
                )),
            },
            State::QueryEffSemi => expect_punct(self, tok, raw, ";", State::QueryPreOrPostKw),
            State::QueryPreOrPostKw => match tok {
                Tok::Kw("pre") => {
                    self.state = State::QueryPreColon;
                    Ok(())
                }
                Tok::Kw("post") => {
                    self.state = State::QueryPostColon;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `pre` or `post`, found `{raw}`"),
                )),
            },
            State::QueryPreColon => {
                expect_punct(self, tok, raw, ":", State::QueryPreSemi).map(|()| {
                    self.start_formula(
                        FormulaCtx {
                            clause: ClauseKind::Pre,
                            bound: Vec::new(),
                            inv_label: None,
                            allow_ports: false,
                            allow_result: false,
                            allow_resources: false,
                        },
                        FormulaCont::QueryPre,
                    );
                })
            }
            State::QueryPreSemi => expect_punct(self, tok, raw, ";", State::QueryPostKw),
            State::QueryPostKw => expect_kw(self, tok, raw, "post", State::QueryPostColon),
            State::QueryPostColon => {
                if *tok == Tok::Punct(":") {
                    self.start_formula(
                        FormulaCtx {
                            clause: ClauseKind::Post,
                            bound: Vec::new(),
                            inv_label: None,
                            allow_ports: false,
                            allow_result: true,
                            allow_resources: false,
                        },
                        FormulaCont::QueryPost,
                    );
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `:`, found `{raw}`")))
                }
            }
            State::QueryPostSemi => expect_punct(self, tok, raw, ";", State::QueryIntentOrClose),
            State::QueryIntentOrClose => match tok {
                Tok::Kw("intent") => {
                    self.state = State::QueryIntentColon;
                    Ok(())
                }
                Tok::Punct("}") => {
                    self.finish_query();
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `intent` or `}}`, found `{raw}`"),
                )),
            },
            State::QueryIntentColon => expect_punct(self, tok, raw, ":", State::QueryIntentStr),
            State::QueryIntentStr => match tok {
                Tok::Str(_) => {
                    self.state = State::QueryIntentSemi;
                    Ok(())
                }
                _ => Err(self.err("VG-P0001", raw, format!("expected string, found `{raw}`"))),
            },
            State::QueryIntentSemi => expect_punct(self, tok, raw, ";", State::QueryRBrace),
            State::QueryRBrace => {
                if *tok == Tok::Punct("}") {
                    self.finish_query();
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `}}`, found `{raw}`")))
                }
            }

            // NODE / SPEC
            State::ItemLabel { kind } => match tok {
                Tok::Label(l) => {
                    if self.labels.contains_key(l) {
                        return Err(self.err("VG-S0006", raw, format!("label @{l} defined twice")));
                    }
                    self.node = Some(NodeState::new(kind, l.clone()));
                    self.state = if kind == ItemKind::Node {
                        State::NodeAttrOrLBrace
                    } else {
                        State::NodeLBrace
                    };
                    Ok(())
                }
                _ => Err(self.err("VG-P0001", raw, format!("expected label, found `{raw}`"))),
            },
            State::NodeAttrOrLBrace => match tok {
                Tok::Punct("[") => {
                    self.state = State::NodeAttrKey;
                    Ok(())
                }
                Tok::Punct("{") => {
                    self.state = State::SigKw;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `[` or `{{`, found `{raw}`"),
                )),
            },
            State::NodeAttrKey => match tok {
                Tok::Kw("intent") => {
                    self.state = State::NodeAttrIntentColon;
                    Ok(())
                }
                Tok::Kw("export") | Tok::Kw("inline") => {
                    self.state = State::NodeAttrCommaOrClose;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `intent`, `export`, or `inline`, found `{raw}`"),
                )),
            },
            State::NodeAttrIntentColon => {
                expect_punct(self, tok, raw, ":", State::NodeAttrIntentStr)
            }
            State::NodeAttrIntentStr => match tok {
                Tok::Str(_) => {
                    self.state = State::NodeAttrCommaOrClose;
                    Ok(())
                }
                _ => Err(self.err("VG-P0001", raw, format!("expected string, found `{raw}`"))),
            },
            State::NodeAttrCommaOrClose => match tok {
                Tok::Punct(",") => {
                    self.state = State::NodeAttrKey;
                    Ok(())
                }
                Tok::Punct("]") => {
                    self.state = State::NodeLBrace;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `]`, found `{raw}`"),
                )),
            },
            State::NodeLBrace => expect_punct(self, tok, raw, "{", State::SigKw),
            State::SigKw => expect_kw(self, tok, raw, "SIGNATURE", State::SigColon),
            State::SigColon => expect_punct(self, tok, raw, ":", State::SigLParen),
            State::SigLParen => {
                expect_punct(self, tok, raw, "(", State::SigParamOrClose { first: true })
            }
            State::SigParamOrClose { first } => {
                if first && *tok == Tok::Punct(")") {
                    self.state = State::SigArrow;
                    Ok(())
                } else {
                    match tok {
                        Tok::Ident(name) => {
                            if self
                                .node
                                .as_ref()
                                .unwrap()
                                .inputs
                                .iter()
                                .any(|(m, _)| m == name)
                            {
                                return Err(self.err(
                                    "VG-S0001",
                                    raw,
                                    format!("parameter `{name}` declared twice"),
                                ));
                            }
                            self.state = State::SigParamColon { name: name.clone() };
                            Ok(())
                        }
                        _ => Err(self.kw_or_ident_err(tok, raw, "a parameter name")),
                    }
                }
            }
            State::SigParamColon { name } => {
                if *tok == Tok::Punct(":") {
                    self.push_type(true, TypeCont::NodeParam { name });
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `:`, found `{raw}`")))
                }
            }
            State::SigParamCommaOrClose => match tok {
                Tok::Punct(",") => {
                    self.state = State::SigParamOrClose { first: false };
                    Ok(())
                }
                Tok::Punct(")") => {
                    self.state = State::SigArrow;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `)`, found `{raw}`"),
                )),
            },
            State::SigArrow => expect_punct(self, tok, raw, "->", State::SigResLParen),
            State::SigResLParen => expect_punct(self, tok, raw, "(", State::SigResCommaOrClose),
            State::SigResCommaOrClose => {
                if self.node.as_ref().unwrap().results.is_empty() {
                    self.push_type(true, TypeCont::NodeResult);
                    self.step_type(tok, raw)
                } else {
                    match tok {
                        Tok::Punct(",") => {
                            self.push_type(true, TypeCont::NodeResult);
                            Ok(())
                        }
                        Tok::Punct(")") => {
                            self.state = State::SigSemi;
                            Ok(())
                        }
                        _ => Err(self.err(
                            "VG-P0001",
                            raw,
                            format!("expected `,` or `)`, found `{raw}`"),
                        )),
                    }
                }
            }
            State::SigSemi => expect_punct(self, tok, raw, ";", State::EffKw),
            State::EffKw => expect_kw(self, tok, raw, "EFFECTS", State::EffColon),
            State::EffColon => expect_punct(self, tok, raw, ":", State::EffLBracket),
            State::EffLBracket => {
                expect_punct(self, tok, raw, "[", State::EffElemOrClose { first: true })
            }
            State::EffElemOrClose { first } => {
                if first && *tok == Tok::Punct("]") {
                    self.check_node_effects_against_sig(raw)?;
                    self.state = State::EffSemi;
                    Ok(())
                } else if let Tok::Kw(k) = tok
                    && EFFECTS.contains(k)
                {
                    let bit = EffectMask::bit(k).unwrap();
                    let n = self.node.as_mut().unwrap();
                    if n.kind == ItemKind::Spec && bit != EffectMask::ARENA {
                        return Err(self.err("VG-E0101", raw, "SPEC node must be pure"));
                    }
                    n.effects.0 |= bit;
                    self.state = State::EffCommaOrClose;
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected effect, found `{raw}`")))
                }
            }
            State::EffCommaOrClose => match tok {
                Tok::Punct(",") => {
                    self.state = State::EffElemOrClose { first: false };
                    Ok(())
                }
                Tok::Punct("]") => {
                    self.check_node_effects_against_sig(raw)?;
                    self.state = State::EffSemi;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `]`, found `{raw}`"),
                )),
            },
            State::EffSemi => expect_punct(self, tok, raw, ";", State::ContractKw),
            State::ContractKw => expect_kw(self, tok, raw, "CONTRACT", State::ContractLBrace),
            State::ContractLBrace => {
                expect_punct(self, tok, raw, "{", State::ContractClauseOrClose)
            }
            State::ContractClauseOrClose => match tok {
                Tok::Punct("}") => {
                    self.state = State::BodyOrLearnable;
                    Ok(())
                }
                Tok::Kw("PRE") => {
                    self.state = State::ClauseColon {
                        kind: ClauseKind::Pre,
                        inv_label: None,
                    };
                    Ok(())
                }
                Tok::Kw("POST") => {
                    self.state = State::ClauseColon {
                        kind: ClauseKind::Post,
                        inv_label: None,
                    };
                    Ok(())
                }
                Tok::Kw("DECREASES") => {
                    self.state = State::ClauseColon {
                        kind: ClauseKind::Decreases,
                        inv_label: None,
                    };
                    Ok(())
                }
                Tok::Kw("COST") => {
                    self.state = State::ClauseColon {
                        kind: ClauseKind::Cost,
                        inv_label: None,
                    };
                    Ok(())
                }
                Tok::Kw("BUDGET") => {
                    self.state = State::ClauseColon {
                        kind: ClauseKind::Budget,
                        inv_label: None,
                    };
                    Ok(())
                }
                Tok::Kw("INVARIANT") => {
                    self.state = State::InvLabel;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected contract clause or `}}`, found `{raw}`"),
                )),
            },
            State::InvLabel => match tok {
                Tok::Label(l) => {
                    if self.node.as_ref().unwrap().invariants.contains_key(l) {
                        return Err(self.err(
                            "VG-S0006",
                            raw,
                            format!("INVARIANT @{l} defined twice"),
                        ));
                    }
                    self.state = State::InvLParen { label: l.clone() };
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected invariant label, found `{raw}`"),
                )),
            },
            State::InvLParen { label } => expect_punct(self, tok, raw, "(", State::InvK { label }),
            State::InvK { label } => match tok {
                Tok::Ident(k) => {
                    self.state = State::InvComma {
                        label,
                        k: k.clone(),
                    };
                    Ok(())
                }
                _ => Err(self.kw_or_ident_err(tok, raw, "a step count name")),
            },
            State::InvComma { label, k } => {
                expect_punct(self, tok, raw, ",", State::InvAcc { label, k })
            }
            State::InvAcc { label, k } => match tok {
                Tok::Ident(acc) => {
                    if *acc == k {
                        return Err(self.err(
                            "VG-S0001",
                            raw,
                            format!("`{acc}` bound twice in INVARIANT"),
                        ));
                    }
                    self.state = State::InvRParen {
                        label,
                        k,
                        acc: acc.clone(),
                    };
                    Ok(())
                }
                _ => Err(self.kw_or_ident_err(tok, raw, "an accumulator name")),
            },
            State::InvRParen { label, k, acc } => {
                if *tok == Tok::Punct(")") {
                    self.node.as_mut().unwrap().invariants.insert(
                        label.clone(),
                        InvariantInfo {
                            k_name: k,
                            acc_name: acc,
                            bound_acc_ty: None,
                            pending_ports: Vec::new(),
                        },
                    );
                    self.state = State::ClauseColon {
                        kind: ClauseKind::Invariant,
                        inv_label: Some(label),
                    };
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `)`, found `{raw}`")))
                }
            }
            State::ClauseColon { kind, inv_label } => {
                if *tok == Tok::Punct(":") {
                    let mut bound = Vec::new();
                    if let Some(lbl) = &inv_label {
                        let inv = &self.node.as_ref().unwrap().invariants[lbl];
                        bound.push((inv.k_name.clone(), FTy::Int));
                        bound.push((inv.acc_name.clone(), FTy::AnyInvAcc));
                    }
                    self.start_formula(
                        FormulaCtx {
                            clause: kind,
                            bound,
                            inv_label: inv_label.clone(),
                            allow_ports: kind == ClauseKind::Invariant,
                            allow_result: kind == ClauseKind::Post,
                            allow_resources: matches!(kind, ClauseKind::Cost | ClauseKind::Budget),
                        },
                        FormulaCont::Clause { kind, inv_label },
                    );
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `:`, found `{raw}`")))
                }
            }
            State::ClauseSemi => expect_punct(self, tok, raw, ";", State::ContractClauseOrClose),
            State::BodyOrLearnable => match tok {
                Tok::Kw("LEARNABLE") => {
                    self.state = State::LearnableLBrace;
                    Ok(())
                }
                Tok::Kw("BODY") => {
                    self.state = State::BodyLBrace;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `LEARNABLE` or `BODY`, found `{raw}`"),
                )),
            },
            State::LearnableLBrace => expect_punct(
                self,
                tok,
                raw,
                "{",
                State::LearnableEntryOrClose { first: true },
            ),
            State::LearnableEntryOrClose { first } => {
                if !first && *tok == Tok::Punct("}") {
                    self.state = State::BodyOrLearnable;
                    Ok(())
                } else if let Tok::Ident(name) = tok {
                    if self
                        .node
                        .as_ref()
                        .unwrap()
                        .inputs
                        .iter()
                        .any(|(m, _)| m == name)
                    {
                        return Err(self.err("VG-S0001", raw, format!("`{name}` declared twice")));
                    }
                    self.state = State::LearnableColon { name: name.clone() };
                    Ok(())
                } else {
                    Err(self.kw_or_ident_err(tok, raw, "a learnable parameter name"))
                }
            }
            State::LearnableColon { name } => {
                expect_punct(self, tok, raw, ":", State::LearnableFloat { name })
            }
            State::LearnableFloat { name } => match tok {
                Tok::Kw(k) if FLOATS.contains(k) => {
                    self.state = State::LearnableIn { name, ty: k };
                    Ok(())
                }
                Tok::Kw(k) if SCALARS.contains(k) => {
                    Err(self.err("VG-T0117", raw, "LEARNABLE parameter must be a float type"))
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected float type, found `{raw}`"),
                )),
            },
            State::LearnableIn { name, ty } => {
                expect_kw(self, tok, raw, "in", State::LearnableLBracket { name, ty })
            }
            State::LearnableLBracket { name, ty } => {
                expect_punct(self, tok, raw, "[", State::LearnableLoSign { name, ty })
            }
            State::LearnableLoSign { name, ty } => match tok {
                Tok::Punct("-") => {
                    self.state = State::LearnableLoNum {
                        name,
                        ty,
                        neg: true,
                    };
                    Ok(())
                }
                Tok::Int(s) | Tok::Decimal(s) => {
                    let lo = self.parse_learnable_bound(s, false, ty, raw)?;
                    self.state = State::LearnableComma { name, ty, lo };
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected lower bound, found `{raw}`"),
                )),
            },
            State::LearnableLoNum { name, ty, neg } => match tok {
                Tok::Int(s) | Tok::Decimal(s) => {
                    let lo = self.parse_learnable_bound(s, neg, ty, raw)?;
                    self.state = State::LearnableComma { name, ty, lo };
                    Ok(())
                }
                _ => Err(self.err("VG-P0001", raw, format!("expected number, found `{raw}`"))),
            },
            State::LearnableComma { name, ty, lo } => {
                expect_punct(self, tok, raw, ",", State::LearnableHiSign { name, ty, lo })
            }
            State::LearnableHiSign { name, ty, lo } => match tok {
                Tok::Punct("-") => {
                    self.state = State::LearnableHiNum {
                        name,
                        ty,
                        lo,
                        neg: true,
                    };
                    Ok(())
                }
                Tok::Int(s) | Tok::Decimal(s) => {
                    let hi = self.parse_learnable_bound(s, false, ty, raw)?;
                    if lo > hi {
                        return Err(self.err(
                            "VG-T0117",
                            raw,
                            format!("LEARNABLE {name}: lo {lo} > hi {hi}"),
                        ));
                    }
                    self.state = State::LearnableRBracket { name, ty };
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected upper bound, found `{raw}`"),
                )),
            },
            State::LearnableHiNum { name, ty, lo, neg } => match tok {
                Tok::Int(s) | Tok::Decimal(s) => {
                    let hi = self.parse_learnable_bound(s, neg, ty, raw)?;
                    if lo > hi {
                        return Err(self.err(
                            "VG-T0117",
                            raw,
                            format!("LEARNABLE {name}: lo {lo} > hi {hi}"),
                        ));
                    }
                    self.state = State::LearnableRBracket { name, ty };
                    Ok(())
                }
                _ => Err(self.err("VG-P0001", raw, format!("expected number, found `{raw}`"))),
            },
            State::LearnableRBracket { name, ty } => {
                if *tok == Tok::Punct("]") {
                    let t = Type::scalar(ty).unwrap();
                    let n = self.node.as_mut().unwrap();
                    let n_params = n.params.len();
                    n.inputs.push((name.clone(), t.clone()));
                    n.values.insert(
                        name,
                        ValMeta {
                            ty: t,
                            origin_local_frame: false,
                            has_local_slice: false,
                            owns_fresh_arena_slice: false,
                            from_params: vec![false; n_params],
                            valid_handle: None,
                            const_u64: None,
                            is_literal: false,
                        },
                    );
                    self.state = State::LearnableLossOrSemi;
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `]`, found `{raw}`")))
                }
            }
            State::LearnableLossOrSemi => match tok {
                Tok::Kw("loss") => {
                    self.state = State::LearnableLossColon;
                    Ok(())
                }
                Tok::Punct(";") => {
                    self.state = State::LearnableEntryOrClose { first: false };
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `loss` or `;`, found `{raw}`"),
                )),
            },
            State::LearnableLossColon => {
                expect_punct(self, tok, raw, ":", State::LearnableLossLabel)
            }
            State::LearnableLossLabel => match tok {
                Tok::Label(l) => {
                    let Some(info) = self.labels.get(l) else {
                        return Err(self.err("VG-S0005", raw, format!("unknown loss label @{l}")));
                    };
                    if info.kind != ItemKind::Node {
                        return Err(self.err("VG-S0109", raw, "loss must be a NODE"));
                    }
                    if info.effects.0 != 0 {
                        return Err(self.err("VG-T0107", raw, "loss node must be pure"));
                    }
                    self.state = State::LearnableSemi;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected loss label, found `{raw}`"),
                )),
            },
            State::LearnableSemi => expect_punct(
                self,
                tok,
                raw,
                ";",
                State::LearnableEntryOrClose { first: false },
            ),
            State::BodyLBrace => {
                expect_punct(self, tok, raw, "{", State::StmtStart { first: true })
            }

            // BODY statements
            State::StmtStart { first } => {
                let n = self.node.as_ref().unwrap();
                if n.has_returned {
                    return match tok {
                        Tok::Punct("}") => self.finish_node(raw),
                        _ => Err(self.err("VG-S0008", raw, "RETURN must be the last statement")),
                    };
                }
                match tok {
                    Tok::Punct("}") if !first => {
                        Err(self.err("VG-S0008", raw, "BODY must end with RETURN"))
                    }
                    Tok::Kw("RETURN") => {
                        self.state = State::ReturnLParen;
                        Ok(())
                    }
                    Tok::Port(p) => {
                        let key = format!("%{p}");
                        if n.values.contains_key(&key) {
                            return Err(self.err(
                                "VG-S0001",
                                raw,
                                format!("%{p} is assigned twice"),
                            ));
                        }
                        self.state = State::BindAfterPort {
                            binds: Vec::new(),
                            port: p.clone(),
                        };
                        Ok(())
                    }
                    _ => Err(self.err(
                        "VG-P0001",
                        raw,
                        format!("expected port `%n` or `RETURN`, found `{raw}`"),
                    )),
                }
            }
            State::BindAfterPort { mut binds, port } => match tok {
                Tok::Punct(":") => {
                    self.state = State::BindAfterColon { binds };
                    self.push_type(false, TypeCont::Binding { port });
                    Ok(())
                }
                Tok::Punct(",") => {
                    binds.push(StmtBind { port, ann: None });
                    self.state = State::BindNextPort { binds };
                    Ok(())
                }
                _ => {
                    binds.push(StmtBind { port, ann: None });
                    self.state = State::BindCommaOrEq { binds };
                    self.step_main(tok, raw)
                }
            },
            State::BindAfterColon { .. } => unreachable!(),
            State::BindCommaOrEq { binds } => match tok {
                Tok::Punct("=") => {
                    self.state = State::OpName { binds };
                    Ok(())
                }
                Tok::Punct(",") => {
                    self.state = State::BindNextPort { binds };
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `=`, found `{raw}`"),
                )),
            },
            State::BindNextPort { binds } => match tok {
                Tok::Port(p) => {
                    let key = format!("%{p}");
                    let n = self.node.as_ref().unwrap();
                    if n.values.contains_key(&key) || binds.iter().any(|b| b.port == *p) {
                        return Err(self.err("VG-S0001", raw, format!("%{p} is assigned twice")));
                    }
                    self.state = State::BindAfterPort {
                        binds,
                        port: p.clone(),
                    };
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected port `%n`, found `{raw}`"),
                )),
            },
            State::OpName { binds } => {
                let op_name = match tok {
                    Tok::OpName(s) => s.as_str(),
                    Tok::Kw(k) if EFFECTS.contains(k) => *k,
                    _ => {
                        return Err(self.err(
                            "VG-P0001",
                            raw,
                            format!("expected opcode, found `{raw}`"),
                        ));
                    }
                };
                let Some(info) = opcode::by_name(op_name) else {
                    return Err(self.err("VG-S0101", raw, format!("unknown opcode `{op_name}`")));
                };
                if !info.mvp {
                    return Err(self.err(
                        "VG-S0104",
                        raw,
                        format!("`{op_name}` is not available in MVP"),
                    ));
                }
                if !self.op_allowed_for_lhs(info, &binds) {
                    // Emit specific diagnostic code matching docs/15
                    let n = self.node.as_ref().unwrap();
                    if matches!(op_name, "FILE_READ" | "FILE_WRITE" | "CLOCK_READ") {
                        let bit = EffectMask::bit(op_name).unwrap();
                        if n.effects.0 & bit == 0 {
                            return Err(self.err(
                                "VG-E0101",
                                raw,
                                format!("{op_name} requires {op_name} in EFFECTS"),
                            ));
                        }
                        if op_name == "FILE_READ" && (n.effects.0 & EffectMask::ARENA == 0) {
                            return Err(self.err(
                                "VG-E0106",
                                raw,
                                "FILE_READ requires ARENA in EFFECTS",
                            ));
                        }
                    }
                    if matches!(op_name, "OK" | "ERR")
                        && !matches!(binds[0].ann, Some(Type::Result(..)))
                    {
                        return Err(self.err(
                            "VG-T0115",
                            raw,
                            format!("{op_name} requires `%r: Result<T, E>` annotation"),
                        ));
                    }
                    let expected_res = match op_name {
                        "FILE_READ" | "FILE_WRITE" | "CLOCK_READ" => Some(2),
                        "CALL_HASH" | "ASSERT_GUARD" => None,
                        _ => Some(1),
                    };
                    if expected_res.is_some_and(|c| c != binds.len()) {
                        return Err(self.err(
                            "VG-S0106",
                            raw,
                            format!("{op_name} result count mismatch"),
                        ));
                    }
                    return Err(self.err(
                        "VG-T0115",
                        raw,
                        format!("{op_name} incompatible with LHS annotation"),
                    ));
                }
                let call = StmtCall {
                    op_name: op_name.to_string(),
                    info,
                    mode: None,
                    cast_ty: None,
                    binds,
                    args: Vec::new(),
                    named_seen: BTreeSet::new(),
                    cur_arg_name: None,
                };
                self.state = State::OpDotOrLParen { call };
                Ok(())
            }
            State::OpDotOrLParen { call } => match tok {
                Tok::Punct(".") => {
                    if call.info.modes == Modes::None {
                        return Err(self.err(
                            "VG-S0102",
                            raw,
                            format!("{} takes no mode", call.op_name),
                        ));
                    }
                    self.state = State::OpMode { call };
                    Ok(())
                }
                Tok::Punct("(") => {
                    if call.info.mode_byte(None).is_none() {
                        return Err(self.err(
                            "VG-S0102",
                            raw,
                            format!("{} requires a mode", call.op_name),
                        ));
                    }
                    self.state = State::OpArgOrClose { call, first: true };
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `.` or `(`, found `{raw}`"),
                )),
            },
            State::OpMode { mut call } => {
                let m_str = match tok {
                    Tok::Ident(s) => s.clone(),
                    Tok::Kw(k) if SCALARS.contains(k) => (*k).to_string(),
                    _ => {
                        return Err(self.err(
                            "VG-P0001",
                            raw,
                            format!("expected op mode, found `{raw}`"),
                        ));
                    }
                };
                if call.info.mode_byte(Some(&m_str)).is_none() {
                    return Err(self.err(
                        "VG-S0102",
                        raw,
                        format!("{} takes no mode `.{m_str}`", call.op_name),
                    ));
                }
                if call.info.modes == Modes::Cast {
                    let Some(dst) = Type::scalar(&m_str) else {
                        return Err(self.err("VG-S0102", raw, "CAST mode must be a scalar type"));
                    };
                    if let Some(ann) = &call.binds[0].ann
                        && *ann != dst
                    {
                        return Err(self.err(
                            "VG-T0115",
                            raw,
                            format!("CAST.{m_str} disagrees with port annotation {ann}"),
                        ));
                    }
                    call.cast_ty = Some(dst);
                }
                call.mode = Some(m_str);
                self.state = State::OpLParen { call };
                Ok(())
            }
            State::OpLParen { call } => expect_punct(
                self,
                tok,
                raw,
                "(",
                State::OpArgOrClose { call, first: true },
            ),
            State::OpArgOrClose { call, first } => {
                if first && *tok == Tok::Punct(")") {
                    self.finish_stmt_call(call, raw)?;
                    self.state = State::OpSemi;
                    return Ok(());
                }
                match tok {
                    Tok::Label(l) => {
                        let call = self.accept_stmt_label(call, l, raw)?;
                        self.state = State::OpArgCommaOrClose { call };
                        Ok(())
                    }
                    Tok::Port(p) => {
                        let key = format!("%{p}");
                        let Some(val) = self.node.as_ref().unwrap().values.get(&key).cloned()
                        else {
                            return Err(self.err("VG-S0003", raw, format!("unknown port `%{p}`")));
                        };
                        let call = self.accept_stmt_val(call, val, raw)?;
                        self.state = State::OpArgCommaOrClose { call };
                        Ok(())
                    }
                    Tok::Ident(s) => {
                        let can_be_named = if call.cur_arg_name.is_none() {
                            let pos_count = call.args.iter().filter(|x| x.name.is_none()).count();
                            match call.info.shape {
                                Shape::Fold if !call.callees_vec().is_empty() => {
                                    let k = &call.callees_vec()[0];
                                    pos_count == k.fn_type.params.len()
                                        && matches!(s.as_str(), "init" | "invariant")
                                        && !call.named_seen.contains(s)
                                }
                                Shape::Guard if !call.callees_vec().is_empty() => {
                                    let k = &call.callees_vec()[0];
                                    pos_count == 2 + k.fn_type.params.len()
                                        && s == "else"
                                        && !call.named_seen.contains(s)
                                }
                                _ => false,
                            }
                        } else {
                            false
                        };
                        if can_be_named {
                            self.state = State::OpArgAfterIdent {
                                call,
                                ident: s.clone(),
                            };
                            Ok(())
                        } else {
                            if !call.named_seen.is_empty() {
                                return Err(self.err(
                                    "VG-S0105",
                                    raw,
                                    "positional argument follows named one",
                                ));
                            }
                            let Some(val) = self.node.as_ref().unwrap().values.get(s).cloned()
                            else {
                                return Err(self.err(
                                    "VG-S0003",
                                    raw,
                                    format!("unknown parameter `{s}`"),
                                ));
                            };
                            let call = self.accept_stmt_val(call, val, raw)?;
                            self.state = State::OpArgCommaOrClose { call };
                            Ok(())
                        }
                    }
                    Tok::Punct("-") => {
                        if !call_allows_lits(&call) {
                            return Err(self.err(
                                "VG-T0101",
                                raw,
                                "unexpected literal operand here",
                            ));
                        }
                        self.state = State::OpLitSign { call, neg: true };
                        Ok(())
                    }
                    Tok::Int(s) | Tok::Decimal(s) => {
                        if !call_allows_lits(&call) {
                            return Err(self.err(
                                "VG-T0101",
                                raw,
                                "unexpected literal operand here",
                            ));
                        }
                        self.state = State::OpLitColon {
                            call,
                            snum: SNum {
                                neg: false,
                                text: s.clone(),
                            },
                        };
                        Ok(())
                    }
                    Tok::Kw("true") => {
                        let val = bool_val_meta(true, self.node.as_ref().unwrap().params.len());
                        let call = self.accept_stmt_val(call, val, raw)?;
                        self.state = State::OpArgCommaOrClose { call };
                        Ok(())
                    }
                    Tok::Kw("false") => {
                        let val = bool_val_meta(false, self.node.as_ref().unwrap().params.len());
                        let call = self.accept_stmt_val(call, val, raw)?;
                        self.state = State::OpArgCommaOrClose { call };
                        Ok(())
                    }
                    Tok::Punct("[") => {
                        if !call_allows_lits(&call) {
                            return Err(self.err(
                                "VG-T0101",
                                raw,
                                "unexpected array literal operand here",
                            ));
                        }
                        self.state = State::OpArrayElem {
                            call,
                            stack: vec![Vec::new()],
                            expect_elem: true,
                        };
                        Ok(())
                    }
                    _ => Err(self.kw_or_ident_err(tok, raw, "an operand")),
                }
            }
            State::OpArgAfterIdent { mut call, ident } => {
                if *tok == Tok::Punct("=") {
                    let allowed_names: &[&str] = match call.info.shape {
                        Shape::Fold => &["init", "invariant"],
                        Shape::Guard => &["else"],
                        _ => &[],
                    };
                    if !allowed_names.contains(&ident.as_str()) || call.named_seen.contains(&ident)
                    {
                        return Err(self.err(
                            "VG-S0105",
                            raw,
                            format!("{} takes no named argument `{ident}=` here", call.op_name),
                        ));
                    }
                    call.cur_arg_name = Some(ident);
                    self.state = State::OpArgAfterEq { call };
                    Ok(())
                } else {
                    if !call.named_seen.is_empty() {
                        return Err(self.err(
                            "VG-S0105",
                            raw,
                            "positional argument follows named one",
                        ));
                    }
                    let Some(val) = self.node.as_ref().unwrap().values.get(&ident).cloned() else {
                        return Err(self.err(
                            "VG-S0003",
                            &ident,
                            format!("unknown parameter `{ident}`"),
                        ));
                    };
                    let call = self.accept_stmt_val(call, val, &ident)?;
                    self.state = State::OpArgCommaOrClose { call };
                    self.step_main(tok, raw)
                }
            }
            State::OpArgAfterEq { call } => match tok {
                Tok::Label(l) => {
                    let call = self.accept_stmt_label(call, l, raw)?;
                    self.state = State::OpArgCommaOrClose { call };
                    Ok(())
                }
                Tok::Port(p) => {
                    let key = format!("%{p}");
                    let Some(val) = self.node.as_ref().unwrap().values.get(&key).cloned() else {
                        return Err(self.err("VG-S0003", raw, format!("unknown port `%{p}`")));
                    };
                    let call = self.accept_stmt_val(call, val, raw)?;
                    self.state = State::OpArgCommaOrClose { call };
                    Ok(())
                }
                Tok::Ident(s) => {
                    let Some(val) = self.node.as_ref().unwrap().values.get(s).cloned() else {
                        return Err(self.err("VG-S0003", raw, format!("unknown parameter `{s}`")));
                    };
                    let call = self.accept_stmt_val(call, val, raw)?;
                    self.state = State::OpArgCommaOrClose { call };
                    Ok(())
                }
                Tok::Punct("-") => {
                    self.state = State::OpLitSign { call, neg: true };
                    Ok(())
                }
                Tok::Int(s) | Tok::Decimal(s) => {
                    self.state = State::OpLitColon {
                        call,
                        snum: SNum {
                            neg: false,
                            text: s.clone(),
                        },
                    };
                    Ok(())
                }
                Tok::Kw("true") => {
                    let val = bool_val_meta(true, self.node.as_ref().unwrap().params.len());
                    let call = self.accept_stmt_val(call, val, raw)?;
                    self.state = State::OpArgCommaOrClose { call };
                    Ok(())
                }
                Tok::Kw("false") => {
                    let val = bool_val_meta(false, self.node.as_ref().unwrap().params.len());
                    let call = self.accept_stmt_val(call, val, raw)?;
                    self.state = State::OpArgCommaOrClose { call };
                    Ok(())
                }
                Tok::Punct("[") => {
                    self.state = State::OpArrayElem {
                        call,
                        stack: vec![Vec::new()],
                        expect_elem: true,
                    };
                    Ok(())
                }
                _ => Err(self.kw_or_ident_err(tok, raw, "an argument value")),
            },
            State::OpLitSign { call, neg } => match tok {
                Tok::Int(s) | Tok::Decimal(s) => {
                    self.state = State::OpLitColon {
                        call,
                        snum: SNum {
                            neg,
                            text: s.clone(),
                        },
                    };
                    Ok(())
                }
                _ => Err(self.err("VG-P0001", raw, format!("expected number, found `{raw}`"))),
            },
            State::OpLitColon { call, snum } => {
                expect_punct(self, tok, raw, ":", State::OpLitScalar { call, snum })
            }
            State::OpLitScalar { call, snum } => match tok {
                Tok::Kw(k) if SCALARS.contains(k) => {
                    let c = literal_const(&ast::Literal::Num(snum, k), self.span(raw))?;
                    let const_u64 = if c.ty == Type::UInt(64) && c.bytes.len() == 8 {
                        Some(u64::from_le_bytes(c.bytes[..8].try_into().unwrap()))
                    } else if c.ty == Type::UInt(32) && c.bytes.len() == 4 {
                        Some(u64::from(u32::from_le_bytes(
                            c.bytes[..4].try_into().unwrap(),
                        )))
                    } else {
                        None
                    };
                    let val = ValMeta {
                        ty: c.ty,
                        origin_local_frame: false,
                        has_local_slice: false,
                        owns_fresh_arena_slice: false,
                        from_params: vec![false; self.node.as_ref().unwrap().params.len()],
                        valid_handle: None,
                        const_u64,
                        is_literal: true,
                    };
                    let call = self.accept_stmt_val(call, val, raw)?;
                    self.state = State::OpArgCommaOrClose { call };
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected scalar type, found `{raw}`"),
                )),
            },
            State::OpArrayElem {
                call,
                mut stack,
                expect_elem,
            } => {
                if expect_elem {
                    match tok {
                        Tok::Punct("[") => {
                            stack.push(Vec::new());
                            self.state = State::OpArrayElem {
                                call,
                                stack,
                                expect_elem: true,
                            };
                            Ok(())
                        }
                        Tok::Punct("-") => {
                            self.state = State::OpArraySign { call, stack };
                            Ok(())
                        }
                        Tok::Int(s) | Tok::Decimal(s) => {
                            stack.last_mut().unwrap().push(ast::Elem::Num(SNum {
                                neg: false,
                                text: s.clone(),
                            }));
                            self.state = State::OpArrayElem {
                                call,
                                stack,
                                expect_elem: false,
                            };
                            Ok(())
                        }
                        Tok::Kw("true") => {
                            stack.last_mut().unwrap().push(ast::Elem::Bool(true));
                            self.state = State::OpArrayElem {
                                call,
                                stack,
                                expect_elem: false,
                            };
                            Ok(())
                        }
                        Tok::Kw("false") => {
                            stack.last_mut().unwrap().push(ast::Elem::Bool(false));
                            self.state = State::OpArrayElem {
                                call,
                                stack,
                                expect_elem: false,
                            };
                            Ok(())
                        }
                        _ => Err(self.err(
                            "VG-P0001",
                            raw,
                            format!("expected array element, found `{raw}`"),
                        )),
                    }
                } else {
                    match tok {
                        Tok::Punct(",") => {
                            self.state = State::OpArrayElem {
                                call,
                                stack,
                                expect_elem: true,
                            };
                            Ok(())
                        }
                        Tok::Punct("]") => {
                            let done = stack.pop().unwrap();
                            if let Some(parent) = stack.last_mut() {
                                parent.push(ast::Elem::Array(done));
                                self.state = State::OpArrayElem {
                                    call,
                                    stack,
                                    expect_elem: false,
                                };
                            } else {
                                self.state = State::OpArrayColon { call, elems: done };
                            }
                            Ok(())
                        }
                        _ => Err(self.err(
                            "VG-P0001",
                            raw,
                            format!("expected `,` or `]`, found `{raw}`"),
                        )),
                    }
                }
            }
            State::OpArraySign { call, mut stack } => match tok {
                Tok::Int(s) | Tok::Decimal(s) => {
                    stack.last_mut().unwrap().push(ast::Elem::Num(SNum {
                        neg: true,
                        text: s.clone(),
                    }));
                    self.state = State::OpArrayElem {
                        call,
                        stack,
                        expect_elem: false,
                    };
                    Ok(())
                }
                _ => Err(self.err("VG-P0001", raw, format!("expected number, found `{raw}`"))),
            },
            State::OpArrayColon { call, elems } => {
                if *tok == Tok::Punct(":") {
                    self.state = State::OpArrayColon {
                        call,
                        elems: Vec::new(),
                    };
                    let State::OpArrayColon { .. } = &self.state else {
                        unreachable!()
                    };
                    self.push_type(false, TypeCont::ArrayLit { elems });
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `:`, found `{raw}`")))
                }
            }
            State::OpArgCommaOrClose { call } => match tok {
                Tok::Punct(",") => {
                    self.state = State::OpArgOrClose { call, first: false };
                    Ok(())
                }
                Tok::Punct(")") => {
                    self.finish_stmt_call(call, raw)?;
                    self.state = State::OpSemi;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `)`, found `{raw}`"),
                )),
            },
            State::OpSemi => expect_punct(self, tok, raw, ";", State::StmtStart { first: false }),

            // RETURN
            State::ReturnLParen => expect_punct(self, tok, raw, "(", State::ReturnArg { idx: 0 }),
            State::ReturnArg { idx } => match tok {
                Tok::Port(p) => {
                    let key = format!("%{p}");
                    let Some(val) = self.node.as_ref().unwrap().values.get(&key).cloned() else {
                        return Err(self.err("VG-S0003", raw, format!("unknown port `%{p}`")));
                    };
                    self.accept_return_val(idx, val, Some(raw), raw)?;
                    self.state = State::ReturnCommaOrClose { idx };
                    Ok(())
                }
                Tok::Ident(s) => {
                    let Some(val) = self.node.as_ref().unwrap().values.get(s).cloned() else {
                        return Err(self.err("VG-S0003", raw, format!("unknown parameter `{s}`")));
                    };
                    self.accept_return_val(idx, val, Some(raw), raw)?;
                    self.state = State::ReturnCommaOrClose { idx };
                    Ok(())
                }
                Tok::Punct("-") => {
                    self.state = State::ReturnLitSign { idx, neg: true };
                    Ok(())
                }
                Tok::Int(s) | Tok::Decimal(s) => {
                    self.state = State::ReturnLitColon {
                        idx,
                        snum: SNum {
                            neg: false,
                            text: s.clone(),
                        },
                    };
                    Ok(())
                }
                Tok::Kw("true") => {
                    let val = bool_val_meta(true, self.node.as_ref().unwrap().params.len());
                    self.accept_return_val(idx, val, None, raw)?;
                    self.state = State::ReturnCommaOrClose { idx };
                    Ok(())
                }
                Tok::Kw("false") => {
                    let val = bool_val_meta(false, self.node.as_ref().unwrap().params.len());
                    self.accept_return_val(idx, val, None, raw)?;
                    self.state = State::ReturnCommaOrClose { idx };
                    Ok(())
                }
                Tok::Punct("[") => {
                    self.state = State::ReturnArrayElem {
                        idx,
                        stack: vec![Vec::new()],
                        expect_elem: true,
                    };
                    Ok(())
                }
                _ => Err(self.kw_or_ident_err(tok, raw, "a return value")),
            },
            State::ReturnLitSign { idx, neg } => match tok {
                Tok::Int(s) | Tok::Decimal(s) => {
                    self.state = State::ReturnLitColon {
                        idx,
                        snum: SNum {
                            neg,
                            text: s.clone(),
                        },
                    };
                    Ok(())
                }
                _ => Err(self.err("VG-P0001", raw, format!("expected number, found `{raw}`"))),
            },
            State::ReturnLitColon { idx, snum } => {
                expect_punct(self, tok, raw, ":", State::ReturnLitScalar { idx, snum })
            }
            State::ReturnLitScalar { idx, snum } => match tok {
                Tok::Kw(k) if SCALARS.contains(k) => {
                    let c = literal_const(&ast::Literal::Num(snum, k), self.span(raw))?;
                    let val = ValMeta {
                        ty: c.ty,
                        origin_local_frame: false,
                        has_local_slice: false,
                        owns_fresh_arena_slice: false,
                        from_params: vec![false; self.node.as_ref().unwrap().params.len()],
                        valid_handle: None,
                        const_u64: None,
                        is_literal: true,
                    };
                    self.accept_return_val(idx, val, None, raw)?;
                    self.state = State::ReturnCommaOrClose { idx };
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected scalar type, found `{raw}`"),
                )),
            },
            State::ReturnArrayElem {
                idx,
                mut stack,
                expect_elem,
            } => {
                if expect_elem {
                    match tok {
                        Tok::Punct("[") => {
                            stack.push(Vec::new());
                            self.state = State::ReturnArrayElem {
                                idx,
                                stack,
                                expect_elem: true,
                            };
                            Ok(())
                        }
                        Tok::Punct("-") => {
                            self.state = State::ReturnArraySign { idx, stack };
                            Ok(())
                        }
                        Tok::Int(s) | Tok::Decimal(s) => {
                            stack.last_mut().unwrap().push(ast::Elem::Num(SNum {
                                neg: false,
                                text: s.clone(),
                            }));
                            self.state = State::ReturnArrayElem {
                                idx,
                                stack,
                                expect_elem: false,
                            };
                            Ok(())
                        }
                        Tok::Kw("true") => {
                            stack.last_mut().unwrap().push(ast::Elem::Bool(true));
                            self.state = State::ReturnArrayElem {
                                idx,
                                stack,
                                expect_elem: false,
                            };
                            Ok(())
                        }
                        Tok::Kw("false") => {
                            stack.last_mut().unwrap().push(ast::Elem::Bool(false));
                            self.state = State::ReturnArrayElem {
                                idx,
                                stack,
                                expect_elem: false,
                            };
                            Ok(())
                        }
                        _ => Err(self.err(
                            "VG-P0001",
                            raw,
                            format!("expected array element, found `{raw}`"),
                        )),
                    }
                } else {
                    match tok {
                        Tok::Punct(",") => {
                            self.state = State::ReturnArrayElem {
                                idx,
                                stack,
                                expect_elem: true,
                            };
                            Ok(())
                        }
                        Tok::Punct("]") => {
                            let done = stack.pop().unwrap();
                            if let Some(parent) = stack.last_mut() {
                                parent.push(ast::Elem::Array(done));
                                self.state = State::ReturnArrayElem {
                                    idx,
                                    stack,
                                    expect_elem: false,
                                };
                            } else {
                                self.state = State::ReturnArrayColon { idx, elems: done };
                            }
                            Ok(())
                        }
                        _ => Err(self.err(
                            "VG-P0001",
                            raw,
                            format!("expected `,` or `]`, found `{raw}`"),
                        )),
                    }
                }
            }
            State::ReturnArraySign { idx, mut stack } => match tok {
                Tok::Int(s) | Tok::Decimal(s) => {
                    stack.last_mut().unwrap().push(ast::Elem::Num(SNum {
                        neg: true,
                        text: s.clone(),
                    }));
                    self.state = State::ReturnArrayElem {
                        idx,
                        stack,
                        expect_elem: false,
                    };
                    Ok(())
                }
                _ => Err(self.err("VG-P0001", raw, format!("expected number, found `{raw}`"))),
            },
            State::ReturnArrayColon { idx, elems } => {
                if *tok == Tok::Punct(":") {
                    self.state = State::ReturnArrayColon {
                        idx,
                        elems: Vec::new(),
                    };
                    self.push_type(false, TypeCont::ArrayLit { elems });
                    Ok(())
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `:`, found `{raw}`")))
                }
            }
            State::ReturnCommaOrClose { idx } => match tok {
                Tok::Punct(",") => {
                    if idx + 1 >= self.node.as_ref().unwrap().results.len() {
                        return Err(self.err("VG-T0108", raw, "too many values in RETURN"));
                    }
                    self.state = State::ReturnArg { idx: idx + 1 };
                    Ok(())
                }
                Tok::Punct(")") => {
                    let (res_len, eff_empty) = {
                        let n = self.node.as_ref().unwrap();
                        (n.results.len(), n.eff_live.is_empty())
                    };
                    if idx + 1 != res_len {
                        return Err(self.err(
                            "VG-T0108",
                            raw,
                            format!("RETURN has {} values; SIGNATURE has {}", idx + 1, res_len),
                        ));
                    }
                    if !eff_empty {
                        return Err(self.err(
                            "VG-E0102",
                            raw,
                            "Eff value dropped without being used once",
                        ));
                    }
                    self.node.as_mut().unwrap().has_returned = true;
                    self.state = State::ReturnSemi;
                    Ok(())
                }
                _ => Err(self.err(
                    "VG-P0001",
                    raw,
                    format!("expected `,` or `)`, found `{raw}`"),
                )),
            },
            State::ReturnSemi => expect_punct(self, tok, raw, ";", State::BodyRBrace),
            State::BodyRBrace => expect_punct(self, tok, raw, "}", State::NodeRBrace),
            State::NodeRBrace => {
                if *tok == Tok::Punct("}") {
                    self.finish_node(raw)
                } else {
                    Err(self.err("VG-P0001", raw, format!("expected `}}`, found `{raw}`")))
                }
            }
        }
    }

    fn check_node_effects_against_sig(&self, raw: &str) -> Result<(), Diagnostic> {
        let n = self.node.as_ref().unwrap();
        if n.effects.0 & EffectMask::RESERVED != 0 {
            return Err(self.err("VG-E0105", raw, "reserved effect bit set"));
        }
        for (_, ty) in &n.params {
            if let Type::Handle(hk) = ty
                && let Err(req) = required_handle_effect_ok(*hk, n.effects)
            {
                return Err(self.err(
                    "VG-E0104",
                    raw,
                    format!("handle parameter `{}` requires {req} in EFFECTS", hk.name()),
                ));
            }
        }
        if !n.effects.is_pure() {
            let first_p = n.params.first().map(|(_, t)| t) == Some(&Type::Eff);
            let first_r = n.results.first() == Some(&Type::Eff);
            if !first_p || !first_r {
                return Err(self.err(
                    "VG-E0103",
                    raw,
                    "effectful node must have Eff as its first parameter and first result",
                ));
            }
        } else {
            if n.params.first().map(|(_, t)| t) == Some(&Type::Eff) {
                return Err(self.err("VG-T0119", raw, "pure node cannot take an Eff parameter"));
            }
            if n.results.first() == Some(&Type::Eff) {
                return Err(self.err("VG-T0119", raw, "pure node cannot return an Eff result"));
            }
        }
        Ok(())
    }

    fn parse_learnable_bound(
        &self,
        text: &str,
        neg: bool,
        ty_name: &'static str,
        raw: &str,
    ) -> Result<f64, Diagnostic> {
        let ty = Type::scalar(ty_name).unwrap();
        let Some(bits) = float::parse(text, neg, &ty) else {
            return Err(self.err("VG-P0006", raw, format!("{text} overflows {ty_name}")));
        };
        if float::is_nan(&ty, bits) || float::is_inf(&ty, bits) {
            return Err(self.err("VG-T0117", raw, "LEARNABLE bound must be finite"));
        }
        Ok(float::to_f64(&ty, bits))
    }

    fn finish_query(&mut self) {
        let params = std::mem::take(&mut self.query_params);
        let results = std::mem::take(&mut self.query_results);
        let effects = self.query_effects;
        let label = std::mem::take(&mut self.query_label);
        let np = params.len();
        let nr = results.len();
        self.labels.insert(
            label,
            LabelInfo {
                kind: ItemKind::Query,
                fn_type: FnType { params, results },
                effects,
                result_views_param: vec![vec![true; np]; nr],
                result_handle_valid: vec![None; nr],
            },
        );
        self.state = State::TopLevel;
    }

    fn finish_node(&mut self, raw: &str) -> Result<(), Diagnostic> {
        let n = self.node.take().unwrap();
        for inv in n.invariants.values() {
            for (port, _) in &inv.pending_ports {
                let key = format!("%{port}");
                if !n.values.contains_key(&key) {
                    return Err(Diagnostic::error(
                        "VG-S0003",
                        self.span(raw),
                        format!("INVARIANT references unknown port `%{port}`"),
                    ));
                }
            }
        }
        self.labels.insert(
            n.label,
            LabelInfo {
                kind: n.kind,
                fn_type: FnType {
                    params: n.params.into_iter().map(|(_, t)| t).collect(),
                    results: n.results,
                },
                effects: n.effects,
                result_views_param: n.result_views_param,
                result_handle_valid: n.result_handle_valid,
            },
        );
        self.state = State::TopLevel;
        Ok(())
    }

    fn allowed_main(&self) -> Allowed {
        match &self.state {
            State::ModuleKw => kw_allowed(&["module"], &[]),
            State::ModuleName => Allowed {
                token_classes: vec![TokenClass::Ident],
                ports: Vec::new(),
                labels: Vec::new(),
                opcodes: Vec::new(),
            },
            State::ModuleSemi => kw_allowed(&[], &[";"]),
            State::TopLevel => kw_allowed(&["@MESH_QUERY", "NODE", "SPEC"], &[]),

            State::QueryLabel | State::ItemLabel { .. } => Allowed {
                token_classes: vec![TokenClass::NewLabel],
                ports: Vec::new(),
                labels: Vec::new(),
                opcodes: Vec::new(),
            },
            State::QueryLBrace
            | State::NodeLBrace
            | State::ContractLBrace
            | State::LearnableLBrace
            | State::BodyLBrace => kw_allowed(&[], &["{"]),
            State::QuerySigKw => kw_allowed(&["signature"], &[]),
            State::QuerySigColon
            | State::QueryEffColon
            | State::QueryPreColon
            | State::QueryPostColon
            | State::QueryIntentColon
            | State::NodeAttrIntentColon
            | State::SigColon
            | State::SigParamColon { .. }
            | State::EffColon
            | State::ClauseColon { .. }
            | State::LearnableColon { .. }
            | State::LearnableLossColon
            | State::OpLitColon { .. }
            | State::OpArrayColon { .. }
            | State::ReturnLitColon { .. }
            | State::ReturnArrayColon { .. } => kw_allowed(&[], &[":"]),
            State::QuerySigLParen
            | State::QueryResLParen
            | State::SigLParen
            | State::SigResLParen
            | State::InvLParen { .. }
            | State::OpLParen { .. }
            | State::ReturnLParen => kw_allowed(&[], &["("]),
            State::QuerySigParamOrClose { first } => {
                let mut a = self.allowed_type(&TypeParse::Start {
                    allow_refine: false,
                    cont: TypeCont::QueryParam,
                });
                if *first {
                    a.token_classes.push(TokenClass::Punct(")"));
                }
                a.token_classes.sort();
                a
            }
            State::QuerySigCommaOrClose | State::SigParamCommaOrClose => {
                kw_allowed(&[], &[",", ")"])
            }
            State::QueryResCommaOrClose => {
                if self.query_results.is_empty() {
                    self.allowed_type(&TypeParse::Start {
                        allow_refine: false,
                        cont: TypeCont::QueryResult,
                    })
                } else {
                    kw_allowed(&[], &[",", ")"])
                }
            }
            State::SigResCommaOrClose => {
                if self.node.as_ref().unwrap().results.is_empty() {
                    self.allowed_type(&TypeParse::Start {
                        allow_refine: true,
                        cont: TypeCont::NodeResult,
                    })
                } else {
                    kw_allowed(&[], &[",", ")"])
                }
            }
            State::QueryArrow | State::SigArrow => kw_allowed(&[], &["->"]),
            State::QueryResSemi
            | State::QueryEffSemi
            | State::QueryPreSemi
            | State::QueryPostSemi
            | State::QueryIntentSemi
            | State::SigSemi
            | State::EffSemi
            | State::ClauseSemi
            | State::LearnableSemi
            | State::OpSemi
            | State::ReturnSemi => kw_allowed(&[], &[";"]),
            State::QueryEffKw => kw_allowed(&["effects"], &[]),
            State::QueryEffLBracket | State::EffLBracket | State::LearnableLBracket { .. } => {
                kw_allowed(&[], &["["])
            }
            State::QueryEffElemOrClose { first } => {
                let puncts: &[&str] = if *first { &["]"] } else { &[] };
                kw_allowed(EFFECTS, puncts)
            }
            State::EffElemOrClose { first } => {
                let mut kws = EFFECTS.to_vec();
                if self.node.as_ref().is_some_and(|n| n.kind == ItemKind::Spec) {
                    kws = vec!["ARENA"];
                }
                let allow_close = *first && self.check_node_effects_against_sig("]").is_ok();
                let puncts: &[&str] = if allow_close { &["]"] } else { &[] };
                kw_allowed(&kws, puncts)
            }
            State::QueryEffCommaOrClose | State::NodeAttrCommaOrClose => {
                kw_allowed(&[], &[",", "]"])
            }
            State::EffCommaOrClose => {
                let puncts: &[&str] = if self.check_node_effects_against_sig("]").is_ok() {
                    &[",", "]"]
                } else {
                    &[","]
                };
                kw_allowed(&[], puncts)
            }
            State::QueryPreOrPostKw => kw_allowed(&["pre", "post"], &[]),
            State::QueryPostKw => kw_allowed(&["post"], &[]),
            State::QueryIntentOrClose => kw_allowed(&["intent"], &["}"]),
            State::QueryIntentStr | State::NodeAttrIntentStr => Allowed {
                token_classes: vec![TokenClass::StringLiteral],
                ports: Vec::new(),
                labels: Vec::new(),
                opcodes: Vec::new(),
            },
            State::QueryRBrace | State::BodyRBrace | State::NodeRBrace => kw_allowed(&[], &["}"]),

            State::NodeAttrOrLBrace => kw_allowed(&[], &["[", "{"]),
            State::NodeAttrKey => kw_allowed(&["intent", "export", "inline"], &[]),
            State::SigKw => kw_allowed(&["SIGNATURE"], &[]),
            State::SigParamOrClose { first } => {
                let mut tc = vec![TokenClass::Ident];
                if *first {
                    tc.push(TokenClass::Punct(")"));
                }
                Allowed {
                    token_classes: tc,
                    ports: Vec::new(),
                    labels: Vec::new(),
                    opcodes: Vec::new(),
                }
            }
            State::EffKw => kw_allowed(&["EFFECTS"], &[]),
            State::ContractKw => kw_allowed(&["CONTRACT"], &[]),
            State::ContractClauseOrClose => kw_allowed(
                &["PRE", "POST", "INVARIANT", "DECREASES", "COST", "BUDGET"],
                &["}"],
            ),
            State::InvLabel => Allowed {
                token_classes: vec![TokenClass::NewLabel],
                ports: Vec::new(),
                labels: Vec::new(),
                opcodes: Vec::new(),
            },
            State::InvK { .. } | State::InvAcc { .. } => Allowed {
                token_classes: vec![TokenClass::Ident],
                ports: Vec::new(),
                labels: Vec::new(),
                opcodes: Vec::new(),
            },
            State::InvComma { .. } => kw_allowed(&[], &[","]),
            State::InvRParen { .. } => kw_allowed(&[], &[")"]),
            State::BodyOrLearnable => kw_allowed(&["LEARNABLE", "BODY"], &[]),
            State::LearnableEntryOrClose { first } => {
                let mut tc = vec![TokenClass::Ident];
                if !first {
                    tc.push(TokenClass::Punct("}"));
                }
                Allowed {
                    token_classes: tc,
                    ports: Vec::new(),
                    labels: Vec::new(),
                    opcodes: Vec::new(),
                }
            }
            State::LearnableFloat { .. } => kw_allowed(FLOATS, &[]),
            State::LearnableIn { .. } => kw_allowed(&["in"], &[]),
            State::LearnableLoSign { .. } | State::LearnableHiSign { .. } => Allowed {
                token_classes: vec![
                    TokenClass::Punct("-"),
                    TokenClass::IntLiteral,
                    TokenClass::DecimalLiteral,
                ],
                ports: Vec::new(),
                labels: Vec::new(),
                opcodes: Vec::new(),
            },
            State::LearnableLoNum { .. } | State::LearnableHiNum { .. } => Allowed {
                token_classes: vec![TokenClass::IntLiteral, TokenClass::DecimalLiteral],
                ports: Vec::new(),
                labels: Vec::new(),
                opcodes: Vec::new(),
            },
            State::LearnableComma { .. } => kw_allowed(&[], &[","]),
            State::LearnableRBracket { .. } => kw_allowed(&[], &["]"]),
            State::LearnableLossOrSemi => kw_allowed(&["loss"], &[";"]),
            State::LearnableLossLabel => {
                let mut a = empty_allowed();
                for (name, info) in &self.labels {
                    if info.kind == ItemKind::Node && info.effects.0 == 0 {
                        a.labels.push((format!("@{name}"), info.fn_type.clone()));
                    }
                }
                a
            }

            // BODY statements
            State::StmtStart { .. } => {
                let n = self.node.as_ref().unwrap();
                if n.has_returned {
                    kw_allowed(&[], &["}"])
                } else {
                    Allowed {
                        token_classes: vec![TokenClass::NewPort, TokenClass::Keyword("RETURN")],
                        ports: Vec::new(),
                        labels: Vec::new(),
                        opcodes: Vec::new(),
                    }
                }
            }
            State::BindAfterPort { .. } => kw_allowed(&[], &[":", ",", "="]),
            State::BindAfterColon { .. } => empty_allowed(),
            State::BindCommaOrEq { .. } => kw_allowed(&[], &[",", "="]),
            State::BindNextPort { .. } => Allowed {
                token_classes: vec![TokenClass::NewPort],
                ports: Vec::new(),
                labels: Vec::new(),
                opcodes: Vec::new(),
            },
            State::OpName { binds } => {
                let mut a = empty_allowed();
                for info in opcode::OPS {
                    if self.op_allowed_for_lhs(info, binds) {
                        a.opcodes
                            .push((info.name.to_string(), self.modes_for_op(info, binds)));
                    }
                }
                a.opcodes.sort();
                a
            }
            State::OpDotOrLParen { call } => match call.info.modes {
                Modes::None => kw_allowed(&[], &["("]),
                Modes::Guard => kw_allowed(&[], &[".", "("]),
                _ => kw_allowed(&[], &["."]),
            },
            State::OpMode { call } => {
                let modes = self.modes_for_op(call.info, &call.binds);
                let mut a = empty_allowed();
                for m in modes {
                    if call.info.modes == Modes::Cast {
                        if let Some(&kw) = SCALARS.iter().find(|s| **s == m) {
                            a.token_classes.push(TokenClass::Keyword(kw));
                        }
                    } else {
                        a.token_classes.push(TokenClass::Name(m));
                    }
                }
                a.token_classes.sort();
                a
            }
            State::OpArgOrClose { call, first } => self.allowed_stmt_arg(call, *first),
            State::OpArgAfterIdent { call, ident } => {
                let mut a = empty_allowed();
                let allowed_names: &[&str] = match call.info.shape {
                    Shape::Fold => &["init", "invariant"],
                    Shape::Guard => &["else"],
                    _ => &[],
                };
                if allowed_names.contains(&ident.as_str()) && !call.named_seen.contains(ident) {
                    a.token_classes.push(TokenClass::Punct("="));
                }
                if call.named_seen.is_empty()
                    && let Some(val) = self.node.as_ref().unwrap().values.get(ident)
                    && self.val_fits_stmt_slot(call, Some(ident), val).is_ok()
                {
                    let mut after = call.clone();
                    after.args.push(StmtArg {
                        name: None,
                        val: Some(val.clone()),
                        callee: None,
                    });
                    let mut cont = self.allowed_main_after_arg(&after);
                    a.token_classes.append(&mut cont.token_classes);
                }
                a.token_classes.sort();
                a.token_classes.dedup();
                a
            }
            State::OpArgAfterEq { call } => self.allowed_stmt_arg(call, false),
            State::OpLitSign { .. } | State::ReturnLitSign { .. } => Allowed {
                token_classes: vec![TokenClass::IntLiteral, TokenClass::DecimalLiteral],
                ports: Vec::new(),
                labels: Vec::new(),
                opcodes: Vec::new(),
            },
            State::OpLitScalar { call, snum } => {
                let mut a = empty_allowed();
                let n_params = self.node.as_ref().unwrap().params.len();
                for &s in SCALARS {
                    if let Ok(c) =
                        literal_const(&ast::Literal::Num(snum.clone(), s), Span::default())
                    {
                        let const_u64 = if c.ty == Type::UInt(64) && c.bytes.len() == 8 {
                            Some(u64::from_le_bytes(c.bytes[..8].try_into().unwrap()))
                        } else if c.ty == Type::UInt(32) && c.bytes.len() == 4 {
                            Some(u64::from(u32::from_le_bytes(
                                c.bytes[..4].try_into().unwrap(),
                            )))
                        } else {
                            None
                        };
                        let val = ValMeta {
                            ty: c.ty,
                            origin_local_frame: false,
                            has_local_slice: false,
                            owns_fresh_arena_slice: false,
                            from_params: vec![false; n_params],
                            valid_handle: None,
                            const_u64,
                            is_literal: true,
                        };
                        if self.val_fits_stmt_slot(call, None, &val).is_ok() {
                            a.token_classes.push(TokenClass::Keyword(s));
                        }
                    }
                }
                a
            }
            State::OpArrayElem { expect_elem, .. } | State::ReturnArrayElem { expect_elem, .. } => {
                if *expect_elem {
                    kw_allowed(&["true", "false"], &["[", "-"]).with_lits(true, true)
                } else {
                    kw_allowed(&[], &[",", "]"])
                }
            }
            State::OpArraySign { .. } | State::ReturnArraySign { .. } => Allowed {
                token_classes: vec![TokenClass::IntLiteral, TokenClass::DecimalLiteral],
                ports: Vec::new(),
                labels: Vec::new(),
                opcodes: Vec::new(),
            },
            State::OpArgCommaOrClose { call } => self.allowed_main_after_arg(call),

            State::ReturnArg { idx } => {
                let mut a = empty_allowed();
                let n = self.node.as_ref().unwrap();
                for (name, val) in &n.values {
                    if self.return_val_fits(*idx, Some(name), val) {
                        a.ports.push((name.clone(), val.ty.clone()));
                    }
                }
                if let Some(res_ty) = n.results.get(*idx) {
                    add_literal_starters(&mut a.token_classes, res_ty);
                }
                a.token_classes.sort();
                a.token_classes.dedup();
                a
            }
            State::ReturnLitScalar { idx, snum } => {
                let mut a = empty_allowed();
                let n = self.node.as_ref().unwrap();
                if let Some(res_ty) = n.results.get(*idx)
                    && let Some(sn) = res_ty.scalar_name()
                    && literal_const(&ast::Literal::Num(snum.clone(), sn), Span::default()).is_ok()
                {
                    a.token_classes.push(TokenClass::Keyword(
                        SCALARS.iter().find(|k| **k == sn).unwrap(),
                    ));
                }
                a
            }
            State::ReturnCommaOrClose { idx } => {
                let n = self.node.as_ref().unwrap();
                if idx + 1 < n.results.len() {
                    kw_allowed(&[], &[","])
                } else {
                    kw_allowed(&[], &[")"])
                }
            }
        }
    }

    fn allowed_main_after_arg(&self, call: &StmtCall) -> Allowed {
        let mut a = empty_allowed();
        if self.can_close_stmt_args(call) {
            a.token_classes.push(TokenClass::Punct(")"));
        }
        // Check if another arg could follow
        let can_comma = match call.info.shape {
            Shape::Fixed(m) => call.args.len() < m as usize,
            Shape::AtLeast(_) => true,
            Shape::Kernel | Shape::Call => {
                call.callees_vec().is_empty()
                    || call.args.len() < 1 + call.callees_vec()[0].fn_type.params.len()
            }
            Shape::Fold => {
                !call.named_seen.contains("init") || !call.named_seen.contains("invariant")
            }
            Shape::Guard => !call.named_seen.contains("else"),
        };
        if can_comma {
            a.token_classes.push(TokenClass::Punct(","));
        }
        a
    }

    fn allowed_stmt_arg(&self, call: &StmtCall, first: bool) -> Allowed {
        let mut a = empty_allowed();
        if first && self.can_close_stmt_args(call) {
            a.token_classes.push(TokenClass::Punct(")"));
        }
        let n = self.node.as_ref().unwrap();
        // Named arguments
        if call.cur_arg_name.is_none() {
            let pos_count = call.args.iter().filter(|x| x.name.is_none()).count();
            match call.info.shape {
                Shape::Fold if !call.callees_vec().is_empty() => {
                    let k = &call.callees_vec()[0];
                    if pos_count == k.fn_type.params.len() {
                        if !call.named_seen.contains("init") {
                            a.token_classes.push(TokenClass::Name("init".into()));
                        }
                        if !call.named_seen.contains("invariant") {
                            a.token_classes.push(TokenClass::Name("invariant".into()));
                        }
                    }
                }
                Shape::Guard if !call.callees_vec().is_empty() => {
                    let k = &call.callees_vec()[0];
                    if pos_count == 2 + k.fn_type.params.len() && !call.named_seen.contains("else")
                    {
                        a.token_classes.push(TokenClass::Name("else".into()));
                    }
                }
                _ => {}
            }
        }
        // Labels
        if call.cur_arg_name.as_deref() == Some("invariant") {
            for (lbl, inv) in &n.invariants {
                if self.label_fits_stmt_slot(call, lbl, None).is_ok() {
                    let acc_t = inv.bound_acc_ty.clone().unwrap_or(Type::Int);
                    a.labels.push((
                        format!("@{lbl}"),
                        FnType {
                            params: vec![Type::Int, acc_t],
                            results: vec![Type::Bool],
                        },
                    ));
                }
            }
        } else {
            for (lbl, info) in &self.labels {
                if self.label_fits_stmt_slot(call, lbl, Some(info)).is_ok() {
                    a.labels.push((format!("@{lbl}"), info.fn_type.clone()));
                }
            }
        }
        // Values (ports + parameters)
        for (name, val) in &n.values {
            if val.ty == Type::Eff && !n.eff_live.contains(name) {
                continue;
            }
            if n.consumed_in_place.contains(name) && (n.effects.0 & EffectMask::ARENA == 0) {
                continue;
            }
            if self.val_fits_stmt_slot(call, Some(name), val).is_ok() {
                a.ports.push((name.clone(), val.ty.clone()));
            }
        }
        // Compatible literal starters
        if call_allows_lits(call) {
            a.token_classes.extend([
                TokenClass::Punct("-"),
                TokenClass::Punct("["),
                TokenClass::IntLiteral,
                TokenClass::DecimalLiteral,
                TokenClass::Keyword("true"),
                TokenClass::Keyword("false"),
            ]);
        }
        a.token_classes.sort();
        a.token_classes.dedup();
        a
    }
}

impl Allowed {
    fn with_lits(mut self, int_lit: bool, dec_lit: bool) -> Self {
        if int_lit {
            self.token_classes.push(TokenClass::IntLiteral);
        }
        if dec_lit {
            self.token_classes.push(TokenClass::DecimalLiteral);
        }
        self.token_classes.sort();
        self.token_classes.dedup();
        self
    }
}

fn add_literal_starters(tc: &mut Vec<TokenClass>, ty: &Type) {
    match ty {
        Type::Bool => tc.extend([TokenClass::Keyword("true"), TokenClass::Keyword("false")]),
        Type::UInt(_) => tc.push(TokenClass::IntLiteral),
        Type::SInt(_) => tc.extend([TokenClass::Punct("-"), TokenClass::IntLiteral]),
        Type::Float(_) | Type::BFloat16 => tc.extend([
            TokenClass::Punct("-"),
            TokenClass::IntLiteral,
            TokenClass::DecimalLiteral,
        ]),
        Type::Array(..) | Type::Tensor(..) => tc.push(TokenClass::Punct("[")),
        _ => {}
    }
}

fn bool_val_meta(b: bool, n_params: usize) -> ValMeta {
    let _ = b;
    ValMeta {
        ty: Type::Bool,
        origin_local_frame: false,
        has_local_slice: false,
        owns_fresh_arena_slice: false,
        from_params: vec![false; n_params],
        valid_handle: None,
        const_u64: None,
        is_literal: true,
    }
}

fn expect_punct(
    p: &mut Prefix,
    tok: &Tok,
    raw: &str,
    want: &'static str,
    next: State,
) -> Result<(), Diagnostic> {
    if *tok == Tok::Punct(want) {
        p.state = next;
        Ok(())
    } else {
        Err(p.err("VG-P0001", raw, format!("expected `{want}`, found `{raw}`")))
    }
}

fn expect_kw(
    p: &mut Prefix,
    tok: &Tok,
    raw: &str,
    want: &'static str,
    next: State,
) -> Result<(), Diagnostic> {
    if *tok == Tok::Kw(want) {
        p.state = next;
        Ok(())
    } else {
        Err(p.err("VG-P0001", raw, format!("expected `{want}`, found `{raw}`")))
    }
}
