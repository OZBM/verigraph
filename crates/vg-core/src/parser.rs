//! Recursive-descent parser for the grammar of docs/03. It accepts exactly the language of
//! tools/vgsym.lark; where that grammar is ambiguous, `-128:i8` is one literal (docs/03).

use crate::ast::*;
use crate::diag::{Diagnostic, Span};
use crate::lexer::{Tok, Token, lex};
use crate::types::Handle;

pub fn parse(src: &str) -> Result<Module, Diagnostic> {
    let mut p = Parser {
        toks: lex(src)?,
        i: 0,
        depth: 0,
    };
    let m = p.module()?;
    Ok(m)
}

struct Parser {
    toks: Vec<Token>,
    i: usize,
    /// Nesting of formulas, types and array literals, bounded so no pass over a tree can exhaust
    /// the stack; a chain of binary operators counts one level per operator.
    depth: u32,
}

/// Deepest recursion the parser enters: parentheses, unary operators, calls, types, arrays.
const MAX_DEPTH: u32 = 64;
/// Deepest formula tree, which chains of binary operators also deepen.
pub const MAX_FORMULA_DEPTH: u16 = 128;

type R<T> = Result<T, Diagnostic>;

const SCALARS: &[&str] = &[
    "u8", "u16", "u32", "u64", "i8", "i16", "i32", "i64", "f16", "bf16", "f32", "f64",
];
const FLOATS: &[&str] = &["f16", "bf16", "f32", "f64"];
const EFFECTS: &[&str] = &[
    "ARENA",
    "CLOCK",
    "FILE_READ",
    "FILE_WRITE",
    "NET_READ",
    "NET_WRITE",
    "GPU",
    "NONDET",
    "PARAM_UPDATE",
    "TELEMETRY",
    "HOST_CALL",
    "SPAWN",
];

fn describe(t: &Tok) -> String {
    match t {
        Tok::Ident(s) => format!("name `{s}`"),
        Tok::OpName(s) => format!("op name `{s}`"),
        Tok::Kw(k) => format!("`{k}`"),
        Tok::Port(s) => format!("port `%{s}`"),
        Tok::Label(s) => format!("label `@{s}`"),
        Tok::Int(s) | Tok::Decimal(s) => format!("number `{s}`"),
        Tok::Str(_) => "a string".into(),
        Tok::Punct(p) => format!("`{p}`"),
        Tok::Eof => "the end of the file".into(),
    }
}

impl Parser {
    fn enter(&mut self) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(Diagnostic::error(
                "VG-P0001",
                self.span(),
                format!("nested deeper than {MAX_DEPTH} levels"),
            ));
        }
        Ok(())
    }

    fn peek(&self, k: usize) -> &Tok {
        &self.toks[(self.i + k).min(self.toks.len() - 1)].tok
    }

    fn span(&self) -> Span {
        self.toks[self.i].span
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.i].clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }

    fn err<T>(&self, expected: &str) -> R<T> {
        let t = self.peek(0);
        let code = match t {
            Tok::Kw(k)
                if expected.contains("name")
                    && k.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) =>
            {
                "VG-P0004"
            }
            _ => "VG-P0001",
        };
        Err(Diagnostic::error(
            code,
            self.span(),
            format!("expected {expected}, found {}", describe(t)),
        ))
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(self.peek(0), Tok::Punct(q) if *q == p)
    }

    fn is_kw(&self, k: &str) -> bool {
        matches!(self.peek(0), Tok::Kw(q) if *q == k)
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        let yes = self.is_punct(p);
        if yes {
            self.bump();
        }
        yes
    }

    fn eat_kw(&mut self, k: &str) -> bool {
        let yes = self.is_kw(k);
        if yes {
            self.bump();
        }
        yes
    }

    fn punct(&mut self, p: &str) -> R<()> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            self.err(&format!("`{p}`"))
        }
    }

    fn kw(&mut self, k: &str) -> R<()> {
        if self.eat_kw(k) {
            Ok(())
        } else {
            self.err(&format!("`{k}`"))
        }
    }

    fn ident(&mut self) -> R<Name> {
        match self.peek(0).clone() {
            Tok::Ident(s) => {
                let span = self.bump().span;
                Ok(Name { text: s, span })
            }
            _ => self.err("a name"),
        }
    }

    fn label(&mut self) -> R<Name> {
        match self.peek(0).clone() {
            Tok::Label(s) => {
                let span = self.bump().span;
                Ok(Name { text: s, span })
            }
            _ => self.err("a label such as `@f`"),
        }
    }

    fn string(&mut self) -> R<String> {
        match self.peek(0).clone() {
            Tok::Str(s) => {
                self.bump();
                Ok(s)
            }
            _ => self.err("a string"),
        }
    }

    fn int(&mut self) -> R<String> {
        match self.peek(0).clone() {
            Tok::Int(s) => {
                self.bump();
                Ok(s)
            }
            _ => self.err("an integer"),
        }
    }

    fn kw_in(&mut self, set: &[&'static str], what: &str) -> R<&'static str> {
        match self.peek(0) {
            Tok::Kw(k) if set.contains(k) => {
                let k = *k;
                self.bump();
                Ok(k)
            }
            _ => self.err(what),
        }
    }

    /// A span from `start` to the end of the previous token.
    fn since(&self, start: Span) -> Span {
        let prev = self.toks[self.i.saturating_sub(1)].span;
        Span {
            len: (prev.offset + prev.len).saturating_sub(start.offset),
            ..start
        }
    }

    fn module(&mut self) -> R<Module> {
        self.kw("module")?;
        let name = self.ident()?.text;
        self.punct(";")?;
        let mut items = Vec::new();
        loop {
            match self.peek(0) {
                Tok::Eof => break,
                Tok::Kw("@MESH_QUERY") => items.push(Item::Query(self.mesh_query()?)),
                Tok::Kw("NODE") | Tok::Kw("SPEC") => items.push(Item::Node(self.node_def()?)),
                _ => return self.err("`NODE`, `SPEC`, `@MESH_QUERY` or the end of the file"),
            }
        }
        Ok(Module { name, items })
    }

    fn mesh_query(&mut self) -> R<MeshQuery> {
        let start = self.span();
        self.kw("@MESH_QUERY")?;
        let label = self.label()?.text;
        self.punct("{")?;
        self.kw("signature")?;
        self.punct(":")?;
        self.punct("(")?;
        let mut params = Vec::new();
        if !self.is_punct(")") {
            params.push(self.ty()?);
            while self.eat_punct(",") {
                params.push(self.ty()?);
            }
        }
        self.punct(")")?;
        self.punct("->")?;
        let results = self.results()?;
        self.punct(";")?;
        self.kw("effects")?;
        self.punct(":")?;
        let effects = self.effects()?;
        self.punct(";")?;
        let mut pre = None;
        if self.eat_kw("pre") {
            self.punct(":")?;
            pre = Some(self.formula()?);
            self.punct(";")?;
        }
        self.kw("post")?;
        self.punct(":")?;
        let post = self.formula()?;
        self.punct(";")?;
        let mut intent = None;
        if self.eat_kw("intent") {
            self.punct(":")?;
            intent = Some(self.string()?);
            self.punct(";")?;
        }
        self.punct("}")?;
        Ok(MeshQuery {
            label,
            params,
            results,
            effects,
            pre,
            post,
            intent,
            span: self.since(start),
        })
    }

    fn node_def(&mut self) -> R<NodeDef> {
        let start = self.span();
        let spec = self.eat_kw("SPEC");
        if !spec {
            self.kw("NODE")?;
        }
        let label = self.label()?.text;
        let mut attrs = Vec::new();
        if !spec && self.eat_punct("[") {
            loop {
                attrs.push(if self.eat_kw("intent") {
                    self.punct(":")?;
                    Attr::Intent(self.string()?)
                } else if self.eat_kw("export") {
                    Attr::Export
                } else if self.eat_kw("inline") {
                    Attr::Inline
                } else {
                    return self.err("`intent`, `export` or `inline`");
                });
                if !self.eat_punct(",") {
                    break;
                }
            }
            self.punct("]")?;
        }
        self.punct("{")?;
        self.kw("SIGNATURE")?;
        self.punct(":")?;
        self.punct("(")?;
        let mut params = Vec::new();
        if !self.is_punct(")") {
            loop {
                let name = self.ident()?;
                self.punct(":")?;
                params.push(Param {
                    name,
                    ty: self.ty()?,
                });
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        self.punct(")")?;
        self.punct("->")?;
        let results = self.results()?;
        self.punct(";")?;
        self.kw("EFFECTS")?;
        self.punct(":")?;
        let effects = self.effects()?;
        self.punct(";")?;
        self.kw("CONTRACT")?;
        self.punct("{")?;
        let mut clauses = Vec::new();
        while !self.eat_punct("}") {
            clauses.push(self.clause()?);
        }
        let mut learnable = None;
        if self.eat_kw("LEARNABLE") {
            self.punct("{")?;
            let mut entries = Vec::new();
            loop {
                let name = self.ident()?;
                self.punct(":")?;
                let ty = self.kw_in(FLOATS, "a float type")?;
                self.kw("in")?;
                self.punct("[")?;
                let lo = self.signed()?;
                self.punct(",")?;
                let hi = self.signed()?;
                self.punct("]")?;
                let mut loss = None;
                if self.eat_kw("loss") {
                    self.punct(":")?;
                    loss = Some(self.label()?);
                }
                self.punct(";")?;
                entries.push(Learn {
                    name,
                    ty,
                    lo,
                    hi,
                    loss,
                });
                if self.eat_punct("}") {
                    break;
                }
            }
            learnable = Some(entries);
        }
        self.kw("BODY")?;
        self.punct("{")?;
        let mut body = vec![self.stmt()?];
        while !self.eat_punct("}") {
            body.push(self.stmt()?);
        }
        self.punct("}")?;
        Ok(NodeDef {
            spec,
            label,
            attrs,
            params,
            results,
            effects,
            clauses,
            learnable,
            body,
            span: self.since(start),
        })
    }

    fn results(&mut self) -> R<Vec<TypeExpr>> {
        self.punct("(")?;
        let mut v = vec![self.ty()?];
        while self.eat_punct(",") {
            v.push(self.ty()?);
        }
        self.punct(")")?;
        Ok(v)
    }

    fn effects(&mut self) -> R<Vec<Name>> {
        self.punct("[")?;
        let mut v = Vec::new();
        if !self.is_punct("]") {
            loop {
                let span = self.span();
                let e = self.kw_in(EFFECTS, "an effect such as `FILE_READ`")?;
                v.push(Name {
                    text: e.to_string(),
                    span,
                });
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        self.punct("]")?;
        Ok(v)
    }

    fn clause(&mut self) -> R<Clause> {
        let span = self.span();
        let kind =
            match self.peek(0) {
                Tok::Kw("PRE") => ClauseKind::Pre,
                Tok::Kw("POST") => ClauseKind::Post,
                Tok::Kw("DECREASES") => ClauseKind::Decreases,
                Tok::Kw("COST") => ClauseKind::Cost,
                Tok::Kw("BUDGET") => ClauseKind::Budget,
                Tok::Kw("INVARIANT") => {
                    self.bump();
                    let label = self.label()?;
                    self.punct("(")?;
                    let k = self.ident()?;
                    self.punct(",")?;
                    let acc = self.ident()?;
                    self.punct(")")?;
                    self.punct(":")?;
                    let formula = self.formula()?;
                    self.punct(";")?;
                    return Ok(Clause {
                        kind: ClauseKind::Invariant { label, k, acc },
                        formula,
                        span,
                    });
                }
                _ => return self.err(
                    "a clause (`PRE`, `POST`, `INVARIANT`, `DECREASES`, `COST`, `BUDGET`) or `}`",
                ),
            };
        self.bump();
        self.punct(":")?;
        let formula = self.formula()?;
        self.punct(";")?;
        Ok(Clause {
            kind,
            formula,
            span,
        })
    }

    fn stmt(&mut self) -> R<Stmt> {
        let start = self.span();
        if self.eat_kw("RETURN") {
            self.punct("(")?;
            let mut values = vec![self.value()?];
            while self.eat_punct(",") {
                values.push(self.value()?);
            }
            self.punct(")")?;
            self.punct(";")?;
            return Ok(Stmt::Return {
                values,
                span: self.since(start),
            });
        }
        let mut binds = Vec::new();
        loop {
            let port = match self.peek(0).clone() {
                Tok::Port(p) => Name {
                    text: p,
                    span: self.bump().span,
                },
                _ => return self.err("a port such as `%0`, `RETURN` or `}`"),
            };
            let ty = if self.eat_punct(":") {
                Some(self.ty()?)
            } else {
                None
            };
            binds.push(Bind { port, ty });
            if !self.eat_punct(",") {
                break;
            }
        }
        self.punct("=")?;
        let op = match self.peek(0).clone() {
            Tok::OpName(s) => Name {
                text: s,
                span: self.bump().span,
            },
            Tok::Kw(k) if EFFECTS.contains(&k) => Name {
                text: k.to_string(),
                span: self.bump().span,
            },
            _ => return self.err("an op name such as `ADD`"),
        };
        let mut mode = None;
        if self.eat_punct(".") {
            let span = self.span();
            mode = Some(match self.peek(0).clone() {
                Tok::Ident(s) => {
                    self.bump();
                    Name { text: s, span }
                }
                _ => Name {
                    text: self.kw_in(SCALARS, "a mode")?.to_string(),
                    span,
                },
            });
        }
        self.punct("(")?;
        let mut args = Vec::new();
        if !self.is_punct(")") {
            loop {
                let span = self.span();
                let name = if matches!(self.peek(0), Tok::Ident(_))
                    && matches!(self.peek(1), Tok::Punct("="))
                {
                    let n = self.ident()?;
                    self.bump();
                    Some(n)
                } else {
                    None
                };
                let val = if let Tok::Label(_) = self.peek(0) {
                    ArgVal::Label(self.label()?)
                } else {
                    ArgVal::Value(self.value()?)
                };
                args.push(Arg {
                    name,
                    val,
                    span: self.since(span),
                });
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        self.punct(")")?;
        self.punct(";")?;
        Ok(Stmt::Assign {
            binds,
            op,
            mode,
            args,
            span: self.since(start),
        })
    }

    fn value(&mut self) -> R<Value> {
        let span = self.span();
        match self.peek(0).clone() {
            Tok::Port(p) => {
                self.bump();
                Ok(Value::Port(Name { text: p, span }))
            }
            Tok::Ident(s) => {
                self.bump();
                Ok(Value::Ident(Name { text: s, span }))
            }
            _ => {
                let lit = self.literal()?;
                Ok(Value::Lit(lit, self.since(span)))
            }
        }
    }

    /// `Signed ":" ScalarType | "true" | "false" | Array ":" Type`.
    fn literal(&mut self) -> R<Literal> {
        if self.eat_kw("true") {
            return Ok(Literal::Bool(true));
        }
        if self.eat_kw("false") {
            return Ok(Literal::Bool(false));
        }
        if self.is_punct("[") {
            let elems = self.array()?;
            self.punct(":")?;
            return Ok(Literal::Array(elems, self.ty()?));
        }
        let num = match self.signed() {
            Ok(n) => n,
            Err(_) => return self.err("a port, a name or a typed literal such as `1:u32`"),
        };
        self.punct(":")?;
        Ok(Literal::Num(num, self.kw_in(SCALARS, "a scalar type")?))
    }

    fn signed(&mut self) -> R<SNum> {
        let neg = self.eat_punct("-");
        match self.peek(0).clone() {
            Tok::Int(s) | Tok::Decimal(s) => {
                self.bump();
                Ok(SNum { neg, text: s })
            }
            _ => self.err("a number"),
        }
    }

    fn array(&mut self) -> R<Vec<Elem>> {
        self.enter()?;
        self.punct("[")?;
        let mut v = Vec::new();
        loop {
            v.push(if self.eat_kw("true") {
                Elem::Bool(true)
            } else if self.eat_kw("false") {
                Elem::Bool(false)
            } else if self.is_punct("[") {
                Elem::Array(self.array()?)
            } else {
                Elem::Num(self.signed()?)
            });
            if !self.eat_punct(",") {
                break;
            }
        }
        self.punct("]")?;
        self.depth -= 1;
        Ok(v)
    }

    fn ty(&mut self) -> R<TypeExpr> {
        self.enter()?;
        let t = self.ty_inner();
        self.depth -= 1;
        t
    }

    fn ty_inner(&mut self) -> R<TypeExpr> {
        let t = match self.peek(0).clone() {
            Tok::Kw(k) if SCALARS.contains(&k) => {
                self.bump();
                return Ok(TypeExpr::Scalar(k));
            }
            Tok::Punct("{") => {
                self.bump();
                let name = self.ident()?;
                self.punct(":")?;
                let base = self.ty()?;
                self.punct("|")?;
                let f = self.formula()?;
                self.punct("}")?;
                return Ok(TypeExpr::Refine(name, Box::new(base), Box::new(f)));
            }
            Tok::Kw(k) => k,
            _ => return self.err("a type"),
        };
        self.bump();
        Ok(match t {
            "Bool" => TypeExpr::Bool,
            "Eff" => TypeExpr::Eff,
            "FileIn" => TypeExpr::Handle(Handle::FileIn),
            "FileOut" => TypeExpr::Handle(Handle::FileOut),
            "Socket" => TypeExpr::Handle(Handle::Socket),
            "Clock" => TypeExpr::Handle(Handle::Clock),
            "GpuQueue" => TypeExpr::Handle(Handle::GpuQueue),
            "Array" | "Tensor" => {
                self.punct("<")?;
                let elem = Box::new(self.ty()?);
                self.punct(",")?;
                let shape = self.shape()?;
                self.punct(">")?;
                if t == "Array" {
                    TypeExpr::Array(elem, shape)
                } else {
                    TypeExpr::Tensor(elem, shape)
                }
            }
            "Slice" => {
                self.punct("<")?;
                let e = self.ty()?;
                self.punct(">")?;
                TypeExpr::Slice(Box::new(e))
            }
            "Tuple" => {
                self.punct("<")?;
                let mut v = vec![self.ty()?];
                while self.eat_punct(",") {
                    v.push(self.ty()?);
                }
                self.punct(">")?;
                TypeExpr::Tuple(v)
            }
            "Result" => {
                self.punct("<")?;
                let a = self.ty()?;
                self.punct(",")?;
                let b = self.ty()?;
                self.punct(">")?;
                TypeExpr::Result(Box::new(a), Box::new(b))
            }
            "Arena" => {
                self.punct("<")?;
                let n = self.int()?;
                self.punct(">")?;
                TypeExpr::Arena(n)
            }
            _ => {
                self.i -= 1;
                return self.err("a type");
            }
        })
    }

    fn shape(&mut self) -> R<Shape> {
        let bracketed = self.eat_punct("[");
        let mut dims = vec![self.dim()?];
        if bracketed {
            while self.eat_punct(",") {
                dims.push(self.dim()?);
            }
            self.punct("]")?;
        }
        Ok(Shape { bracketed, dims })
    }

    fn dim(&mut self) -> R<Dim> {
        match self.peek(0).clone() {
            Tok::Int(s) => {
                self.bump();
                Ok(Dim::Int(s))
            }
            Tok::Ident(s) => {
                self.bump();
                Ok(Dim::Ident(s))
            }
            _ => self.err("a dimension"),
        }
    }

    // Formulas, by binding strength: ==> (right), ||, &&, one comparison, + -, * / %, unary, postfix.

    /// A formula node, rejected when the tree grows deeper than MAX_FORMULA_DEPTH.
    fn mk(&self, kind: FKind, start: Span) -> R<Formula> {
        let f = Formula::at(kind, self.since(start));
        if f.depth > MAX_FORMULA_DEPTH {
            return Err(Diagnostic::error(
                "VG-P0001",
                f.span,
                format!("formula nested deeper than {MAX_FORMULA_DEPTH} levels"),
            ));
        }
        Ok(f)
    }

    fn formula(&mut self) -> R<Formula> {
        self.enter()?;
        let start = self.span();
        let lhs = self.binary(2)?;
        let f = if self.eat_punct("==>") {
            let rhs = self.formula()?;
            self.mk(
                FKind::Binary(BinOp::Implies, Box::new(lhs), Box::new(rhs)),
                start,
            )?
        } else {
            lhs
        };
        self.depth -= 1;
        Ok(f)
    }

    fn binop(&self) -> Option<BinOp> {
        Some(match self.peek(0) {
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

    /// Precedence climbing over the left-associative levels min to 6. Comparisons do not chain:
    /// `a < b < c` is an error, as in the grammar, while `(a < b) < c` is not.
    fn binary(&mut self, min: u8) -> R<Formula> {
        let start = self.span();
        let mut lhs = self.unary()?;
        let mut compared = false;
        while let Some(op) = self.binop().filter(|op| op.level() >= min) {
            if compared && op.level() == 4 {
                return self
                    .err("an operator other than a second comparison; comparisons do not chain");
            }
            self.bump();
            let rhs = self.binary(op.level() + 1)?;
            lhs = self.mk(FKind::Binary(op, Box::new(lhs), Box::new(rhs)), start)?;
            compared = op.level() == 4;
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> R<Formula> {
        let start = self.span();
        let typed_negative = matches!(self.peek(1), Tok::Int(_) | Tok::Decimal(_))
            && matches!(self.peek(2), Tok::Punct(":"))
            && matches!(self.peek(3), Tok::Kw(k) if SCALARS.contains(k));
        let op = match self.peek(0) {
            Tok::Punct("!") => UnOp::Not,
            Tok::Punct("-") if !typed_negative => UnOp::Neg,
            _ => return self.postfix(),
        };
        self.bump();
        self.enter()?;
        let e = self.unary()?;
        self.depth -= 1;
        self.mk(FKind::Unary(op, Box::new(e)), start)
    }

    fn postfix(&mut self) -> R<Formula> {
        let start = self.span();
        let mut e = self.primary()?;
        loop {
            if self.eat_punct("[") {
                let mut idx = vec![self.formula()?];
                while self.eat_punct(",") {
                    idx.push(self.formula()?);
                }
                self.punct("]")?;
                e = self.mk(FKind::Index(Box::new(e), idx), start)?;
            } else if self.eat_punct(".") {
                let j = self.int()?;
                e = self.mk(FKind::Proj(Box::new(e), j), start)?;
            } else {
                return Ok(e);
            }
        }
    }

    fn primary(&mut self) -> R<Formula> {
        let start = self.span();
        let kind = match self.peek(0).clone() {
            Tok::Port(_) => FKind::Value(self.value()?),
            Tok::Ident(_) if matches!(self.peek(1), Tok::Punct("(")) => {
                let name = self.ident()?;
                FKind::Call {
                    prefix: None,
                    name,
                    args: self.call_args()?,
                }
            }
            Tok::Ident(_) => FKind::Value(self.value()?),
            Tok::Kw(k @ ("spec" | "fold")) => {
                self.bump();
                self.punct(":")?;
                let name = self.ident()?;
                FKind::Call {
                    prefix: Some(if k == "spec" {
                        CallPrefix::Spec
                    } else {
                        CallPrefix::Fold
                    }),
                    name,
                    args: self.call_args()?,
                }
            }
            Tok::Kw("result") => {
                self.bump();
                FKind::Result
            }
            Tok::Int(s) | Tok::Decimal(s) if !matches!(self.peek(1), Tok::Punct(":")) => {
                self.bump();
                FKind::Num(s)
            }
            Tok::Int(_) | Tok::Decimal(_) | Tok::Punct("-" | "[") | Tok::Kw("true" | "false") => {
                FKind::Value(self.value()?)
            }
            Tok::Punct("(") => {
                self.bump();
                let f = self.formula()?;
                self.punct(")")?;
                return Ok(f);
            }
            Tok::Kw("forall") => {
                self.bump();
                self.punct("(")?;
                let mut ranges = Vec::new();
                loop {
                    let var = self.ident()?;
                    self.kw("in")?;
                    let lo = self.binary(5)?;
                    self.punct("..")?;
                    let hi = self.binary(5)?;
                    ranges.push(Range { var, lo, hi });
                    if !self.eat_punct(",") {
                        break;
                    }
                }
                self.punct(")")?;
                self.punct("{")?;
                let body = self.formula()?;
                self.punct("}")?;
                FKind::Forall {
                    ranges,
                    body: Box::new(body),
                }
            }
            _ => return self.err("a formula"),
        };
        self.mk(kind, start)
    }

    fn call_args(&mut self) -> R<Vec<Formula>> {
        self.punct("(")?;
        let mut v = Vec::new();
        if !self.is_punct(")") {
            v.push(self.formula()?);
            while self.eat_punct(",") {
                v.push(self.formula()?);
            }
        }
        self.punct(")")?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(params: &str, post: &str, body: &str) -> String {
        format!(
            "module lex;\nNODE @n {{\n  SIGNATURE: ({params}) -> (i32);\n  EFFECTS: [];\n  CONTRACT {{ POST: {post}; }}\n  BODY {{ {body} }}\n}}\n"
        )
    }

    /// The lexical cases of tools/check_examples.py, with the verdicts Lark gives.
    #[test]
    fn lexical_cases_of_check_examples() {
        let cases = [
            (node("x: i32", "result == x-1", "RETURN(x);"), true),
            (
                node(
                    "x: i8",
                    "result == x",
                    "%0 = ADD.wrap(x, -128:i8); RETURN(%0);",
                ),
                true,
            ),
            (
                node(
                    "x: i32",
                    "result == x",
                    "%0 = IDENTITY([1, -2, 3]:Array<i32, 3>); %1 = INDEX(%0, 0:u64); RETURN(%1);",
                ),
                true,
            ),
            (node("x: i32", "result == x % 2", "RETURN(x);"), true),
            (node("x: i32", "result == x%2", "RETURN(x);"), false),
            (node("x: i32", "(result.0).1 == x", "RETURN(x);"), true),
            (node("x: i32", "result.0.1 == x", "RETURN(x);"), false),
            (
                node("x: i32", "result == x", "%0 = ADD(result, x); RETURN(%0);"),
                false,
            ),
            (node("post: i32", "result == post", "RETURN(post);"), false),
            (
                node("x: i32", "forall(i in0..2) { x > 0 }", "RETURN(x);"),
                false,
            ),
            (
                node(
                    "x: i32",
                    "result == x",
                    "%0: i32 = ADD.wrap(x, 1:i32); RETURN(%0);",
                ),
                true,
            ),
            (
                node("x: i32", "result == fold:step(0:i32, x, 1)", "RETURN(x);"),
                true,
            ),
            (node("fold: i32", "result == fold", "RETURN(fold);"), false),
        ];
        for (src, ok) in cases {
            assert_eq!(parse(&src).is_ok(), ok, "{src}");
        }
    }

    #[test]
    fn negative_typed_literal_binds_its_sign() {
        let m = parse(&node(
            "x: i8",
            "result == -128:i8 && result == -x",
            "RETURN(x);",
        ))
        .unwrap();
        let Item::Node(n) = &m.items[0] else { panic!() };
        let FKind::Binary(BinOp::And, a, b) = &n.clauses[0].formula.kind else {
            panic!()
        };
        let FKind::Binary(BinOp::Eq, _, lit) = &a.kind else {
            panic!()
        };
        assert!(matches!(
            &lit.kind,
            FKind::Value(Value::Lit(Literal::Num(SNum { neg: true, .. }, "i8"), _))
        ));
        let FKind::Binary(BinOp::Eq, _, neg) = &b.kind else {
            panic!()
        };
        assert!(matches!(&neg.kind, FKind::Unary(UnOp::Neg, _)));
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_crash() {
        let deep = format!("{}x{}", "(".repeat(5000), ")".repeat(5000));
        assert_eq!(
            parse(&node("x: i32", &deep, "RETURN(x);"))
                .unwrap_err()
                .code,
            "VG-P0001"
        );
        let chain = vec!["x"; 5000].join(" + ");
        assert_eq!(
            parse(&node("x: i32", &chain, "RETURN(x);"))
                .unwrap_err()
                .code,
            "VG-P0001"
        );
        let negs = format!("{}x", "-".repeat(5000));
        assert_eq!(
            parse(&node("x: i32", &negs, "RETURN(x);"))
                .unwrap_err()
                .code,
            "VG-P0001"
        );
        let ty = format!("{}u8{}", "Slice<".repeat(5000), ">".repeat(5000));
        assert_eq!(
            parse(&node(&format!("x: {ty}"), "true", "RETURN(x);"))
                .unwrap_err()
                .code,
            "VG-P0001"
        );
    }

    #[test]
    fn reserved_word_as_name() {
        let e = parse(&node("post: i32", "true", "RETURN(post);")).unwrap_err();
        assert_eq!(e.code, "VG-P0004");
        assert_eq!(parse("module m; NODE").unwrap_err().code, "VG-P0001");
    }
}
