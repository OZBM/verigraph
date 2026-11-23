//! Tier 1 grammar export (P5.3): single in-crate VG-Sym grammar generating
//! llama.cpp GBNF, Lark, and llguidance formats, plus an Earley recognizer.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use vg_core::diag::{Diagnostic, Span};
use vg_core::lexer::{Tok, lex};

/// Supported grammar export formats (docs/07, P5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Gbnf,
    Lark,
    Llguidance,
}

#[derive(Clone, Debug)]
enum Expr {
    Lit(&'static str),
    Term(&'static str),
    Ref(&'static str),
    Seq(Vec<Expr>),
    Alt(Vec<Expr>),
    Opt(Box<Expr>),
    Star(Box<Expr>),
    Plus(Box<Expr>),
}

#[derive(Clone, Debug)]
struct Rule {
    name: &'static str,
    inline: bool,
    body: Expr,
}

#[derive(Clone, Debug)]
struct Grammar {
    rules: Vec<Rule>,
}

fn lit(s: &'static str) -> Expr {
    Expr::Lit(s)
}

fn term(s: &'static str) -> Expr {
    Expr::Term(s)
}

fn nt(s: &'static str) -> Expr {
    Expr::Ref(s)
}

fn seq(items: Vec<Expr>) -> Expr {
    Expr::Seq(items)
}

fn alt(items: Vec<Expr>) -> Expr {
    Expr::Alt(items)
}

fn opt(e: Expr) -> Expr {
    Expr::Opt(Box::new(e))
}

fn star(e: Expr) -> Expr {
    Expr::Star(Box::new(e))
}

fn plus(e: Expr) -> Expr {
    Expr::Plus(Box::new(e))
}

fn sep_by(item: Expr, sep: &'static str) -> Expr {
    seq(vec![item.clone(), star(seq(vec![lit(sep), item]))])
}

fn sep_by_opt(item: Expr, sep: &'static str) -> Expr {
    opt(sep_by(item, sep))
}

/// The single authoritative in-crate VG-Sym grammar description, matching `tools/vgsym.lark`.
fn vgsym_grammar() -> Grammar {
    let rules = vec![
        Rule {
            name: "start",
            inline: false,
            body: seq(vec![
                lit("module"),
                term("IDENT"),
                lit(";"),
                star(nt("item")),
            ]),
        },
        Rule {
            name: "item",
            inline: true,
            body: alt(vec![nt("mesh_query"), nt("node_def"), nt("spec_def")]),
        },
        Rule {
            name: "mesh_query",
            inline: false,
            body: seq(vec![
                lit("@MESH_QUERY"),
                term("LABEL"),
                lit("{"),
                lit("signature"),
                lit(":"),
                nt("fn_type"),
                lit(";"),
                lit("effects"),
                lit(":"),
                nt("effects"),
                lit(";"),
                opt(seq(vec![lit("pre"), lit(":"), nt("formula"), lit(";")])),
                lit("post"),
                lit(":"),
                nt("formula"),
                lit(";"),
                opt(seq(vec![lit("intent"), lit(":"), term("STRING"), lit(";")])),
                lit("}"),
            ]),
        },
        Rule {
            name: "spec_def",
            inline: false,
            body: seq(vec![lit("SPEC"), term("LABEL"), nt("node_body")]),
        },
        Rule {
            name: "node_def",
            inline: false,
            body: seq(vec![
                lit("NODE"),
                term("LABEL"),
                opt(nt("attrs")),
                nt("node_body"),
            ]),
        },
        Rule {
            name: "node_body",
            inline: false,
            body: seq(vec![
                lit("{"),
                lit("SIGNATURE"),
                lit(":"),
                nt("params"),
                lit("->"),
                nt("results"),
                lit(";"),
                lit("EFFECTS"),
                lit(":"),
                nt("effects"),
                lit(";"),
                lit("CONTRACT"),
                lit("{"),
                star(nt("clause")),
                lit("}"),
                opt(nt("learnable")),
                lit("BODY"),
                lit("{"),
                plus(nt("stmt")),
                lit("}"),
                lit("}"),
            ]),
        },
        Rule {
            name: "attrs",
            inline: false,
            body: seq(vec![lit("["), sep_by(nt("attr"), ","), lit("]")]),
        },
        Rule {
            name: "attr",
            inline: false,
            body: alt(vec![
                seq(vec![lit("intent"), lit(":"), term("STRING")]),
                lit("export"),
                lit("inline"),
            ]),
        },
        Rule {
            name: "params",
            inline: false,
            body: seq(vec![
                lit("("),
                sep_by_opt(seq(vec![term("IDENT"), lit(":"), nt("type")]), ","),
                lit(")"),
            ]),
        },
        Rule {
            name: "results",
            inline: false,
            body: seq(vec![lit("("), sep_by(nt("type"), ","), lit(")")]),
        },
        Rule {
            name: "fn_type",
            inline: false,
            body: seq(vec![
                lit("("),
                sep_by_opt(nt("type"), ","),
                lit(")"),
                lit("->"),
                nt("results"),
            ]),
        },
        Rule {
            name: "clause",
            inline: false,
            body: alt(vec![
                seq(vec![lit("PRE"), lit(":"), nt("formula"), lit(";")]),
                seq(vec![lit("POST"), lit(":"), nt("formula"), lit(";")]),
                seq(vec![
                    lit("INVARIANT"),
                    term("LABEL"),
                    lit("("),
                    term("IDENT"),
                    lit(","),
                    term("IDENT"),
                    lit(")"),
                    lit(":"),
                    nt("formula"),
                    lit(";"),
                ]),
                seq(vec![lit("DECREASES"), lit(":"), nt("formula"), lit(";")]),
                seq(vec![lit("COST"), lit(":"), nt("formula"), lit(";")]),
                seq(vec![lit("BUDGET"), lit(":"), nt("formula"), lit(";")]),
            ]),
        },
        Rule {
            name: "learnable",
            inline: false,
            body: seq(vec![
                lit("LEARNABLE"),
                lit("{"),
                plus(seq(vec![
                    term("IDENT"),
                    lit(":"),
                    nt("float_type"),
                    lit("in"),
                    lit("["),
                    nt("signed"),
                    lit(","),
                    nt("signed"),
                    lit("]"),
                    opt(seq(vec![lit("loss"), lit(":"), term("LABEL")])),
                    lit(";"),
                ])),
                lit("}"),
            ]),
        },
        Rule {
            name: "stmt",
            inline: false,
            body: alt(vec![
                seq(vec![
                    sep_by(nt("binding"), ","),
                    lit("="),
                    nt("op"),
                    lit("("),
                    opt(nt("args")),
                    lit(")"),
                    lit(";"),
                ]),
                seq(vec![
                    lit("RETURN"),
                    lit("("),
                    sep_by(nt("value"), ","),
                    lit(")"),
                    lit(";"),
                ]),
            ]),
        },
        Rule {
            name: "binding",
            inline: false,
            body: seq(vec![nt("port"), opt(seq(vec![lit(":"), nt("type")]))]),
        },
        Rule {
            name: "op",
            inline: false,
            body: seq(vec![
                alt(vec![term("OPNAME"), nt("effect")]),
                opt(seq(vec![lit("."), nt("mode")])),
            ]),
        },
        Rule {
            name: "mode",
            inline: false,
            body: alt(vec![term("IDENT"), nt("scalar_type")]),
        },
        Rule {
            name: "args",
            inline: false,
            body: sep_by(nt("arg"), ","),
        },
        Rule {
            name: "arg",
            inline: false,
            body: seq(vec![
                opt(seq(vec![term("IDENT"), lit("=")])),
                alt(vec![nt("value"), term("LABEL")]),
            ]),
        },
        Rule {
            name: "value",
            inline: true,
            body: alt(vec![nt("port"), term("IDENT"), nt("literal")]),
        },
        Rule {
            name: "port",
            inline: false,
            body: term("PORT"),
        },
        Rule {
            name: "literal",
            inline: false,
            body: alt(vec![
                seq(vec![nt("signed"), lit(":"), nt("scalar_type")]),
                lit("true"),
                lit("false"),
                seq(vec![nt("array"), lit(":"), nt("type")]),
            ]),
        },
        Rule {
            name: "signed",
            inline: false,
            body: seq(vec![opt(lit("-")), nt("number")]),
        },
        Rule {
            name: "number",
            inline: true,
            body: alt(vec![term("INT"), term("DECIMAL")]),
        },
        Rule {
            name: "array",
            inline: false,
            body: seq(vec![lit("["), sep_by(nt("element"), ","), lit("]")]),
        },
        Rule {
            name: "element",
            inline: true,
            body: alt(vec![nt("signed"), lit("true"), lit("false"), nt("array")]),
        },
        Rule {
            name: "effects",
            inline: false,
            body: seq(vec![lit("["), sep_by_opt(nt("effect"), ","), lit("]")]),
        },
        Rule {
            name: "effect",
            inline: false,
            body: alt(vec![
                lit("ARENA"),
                lit("CLOCK"),
                lit("FILE_READ"),
                lit("FILE_WRITE"),
                lit("NET_READ"),
                lit("NET_WRITE"),
                lit("GPU"),
                lit("NONDET"),
                lit("PARAM_UPDATE"),
                lit("TELEMETRY"),
                lit("HOST_CALL"),
                lit("SPAWN"),
            ]),
        },
        Rule {
            name: "type",
            inline: false,
            body: alt(vec![
                nt("scalar_type"),
                lit("Bool"),
                lit("Eff"),
                nt("handle"),
                seq(vec![
                    alt(vec![lit("Array"), lit("Tensor")]),
                    lit("<"),
                    nt("type"),
                    lit(","),
                    nt("shape"),
                    lit(">"),
                ]),
                seq(vec![lit("Slice"), lit("<"), nt("type"), lit(">")]),
                seq(vec![
                    lit("Tuple"),
                    lit("<"),
                    sep_by(nt("type"), ","),
                    lit(">"),
                ]),
                seq(vec![
                    lit("Result"),
                    lit("<"),
                    nt("type"),
                    lit(","),
                    nt("type"),
                    lit(">"),
                ]),
                seq(vec![lit("Arena"), lit("<"), term("INT"), lit(">")]),
                seq(vec![
                    lit("{"),
                    term("IDENT"),
                    lit(":"),
                    nt("type"),
                    lit("|"),
                    nt("formula"),
                    lit("}"),
                ]),
            ]),
        },
        Rule {
            name: "shape",
            inline: false,
            body: alt(vec![
                nt("dim"),
                seq(vec![lit("["), sep_by(nt("dim"), ","), lit("]")]),
            ]),
        },
        Rule {
            name: "dim",
            inline: false,
            body: alt(vec![term("INT"), term("IDENT")]),
        },
        Rule {
            name: "handle",
            inline: false,
            body: alt(vec![
                lit("FileIn"),
                lit("FileOut"),
                lit("Socket"),
                lit("Clock"),
                lit("GpuQueue"),
            ]),
        },
        Rule {
            name: "scalar_type",
            inline: false,
            body: alt(vec![nt("int_type"), nt("float_type")]),
        },
        Rule {
            name: "int_type",
            inline: false,
            body: alt(vec![
                lit("u8"),
                lit("u16"),
                lit("u32"),
                lit("u64"),
                lit("i8"),
                lit("i16"),
                lit("i32"),
                lit("i64"),
            ]),
        },
        Rule {
            name: "float_type",
            inline: false,
            body: alt(vec![lit("f16"), lit("bf16"), lit("f32"), lit("f64")]),
        },
        Rule {
            name: "formula",
            inline: true,
            body: seq(vec![nt("or_"), opt(seq(vec![lit("==>"), nt("formula")]))]),
        },
        Rule {
            name: "or_",
            inline: true,
            body: sep_by(nt("and_"), "||"),
        },
        Rule {
            name: "and_",
            inline: true,
            body: sep_by(nt("cmp"), "&&"),
        },
        Rule {
            name: "cmp",
            inline: true,
            body: seq(vec![nt("sum"), opt(seq(vec![nt("cmp_op"), nt("sum")]))]),
        },
        Rule {
            name: "cmp_op",
            inline: false,
            body: alt(vec![
                lit("=="),
                lit("!="),
                lit("<="),
                lit(">="),
                lit("<"),
                lit(">"),
            ]),
        },
        Rule {
            name: "sum",
            inline: true,
            body: seq(vec![
                nt("prod"),
                star(seq(vec![alt(vec![lit("+"), lit("-")]), nt("prod")])),
            ]),
        },
        Rule {
            name: "prod",
            inline: true,
            body: seq(vec![
                nt("unary"),
                star(seq(vec![
                    alt(vec![lit("*"), lit("/"), lit("%")]),
                    nt("unary"),
                ])),
            ]),
        },
        Rule {
            name: "unary",
            inline: true,
            body: alt(vec![
                seq(vec![alt(vec![lit("!"), lit("-")]), nt("unary")]),
                nt("postfix"),
            ]),
        },
        Rule {
            name: "postfix",
            inline: true,
            body: seq(vec![
                nt("primary"),
                star(alt(vec![
                    seq(vec![lit("["), sep_by(nt("formula"), ","), lit("]")]),
                    seq(vec![lit("."), term("INT")]),
                ])),
            ]),
        },
        Rule {
            name: "primary",
            inline: true,
            body: alt(vec![
                nt("value"),
                nt("number"),
                lit("result"),
                nt("call"),
                seq(vec![lit("("), nt("formula"), lit(")")]),
                seq(vec![
                    lit("forall"),
                    lit("("),
                    sep_by(nt("range_"), ","),
                    lit(")"),
                    lit("{"),
                    nt("formula"),
                    lit("}"),
                ]),
            ]),
        },
        Rule {
            name: "range_",
            inline: false,
            body: seq(vec![
                term("IDENT"),
                lit("in"),
                nt("sum"),
                lit(".."),
                nt("sum"),
            ]),
        },
        Rule {
            name: "call",
            inline: false,
            body: seq(vec![
                opt(seq(vec![alt(vec![lit("spec"), lit("fold")]), lit(":")])),
                term("IDENT"),
                lit("("),
                sep_by_opt(nt("formula"), ","),
                lit(")"),
            ]),
        },
    ];
    Grammar { rules }
}

/// Export the VG-Sym grammar in llama.cpp GBNF, Lark, or llguidance syntax.
pub fn export(format: Format) -> String {
    let g = vgsym_grammar();
    match format {
        Format::Lark => emit_lark(&g),
        Format::Llguidance => emit_llguidance(&g),
        Format::Gbnf => emit_gbnf(&g),
    }
}

fn quote_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn emit_lark_expr(e: &Expr, need_parens: bool, out: &mut String) {
    match e {
        Expr::Lit(s) => out.push_str(&quote_lit(s)),
        Expr::Term(t) => out.push_str(t),
        Expr::Ref(r) => out.push_str(r),
        Expr::Seq(xs) => {
            if need_parens && xs.len() > 1 {
                out.push('(');
            }
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                let child_parens = matches!(x, Expr::Alt(_));
                emit_lark_expr(x, child_parens, out);
            }
            if need_parens && xs.len() > 1 {
                out.push(')');
            }
        }
        Expr::Alt(xs) => {
            if need_parens && xs.len() > 1 {
                out.push('(');
            }
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push_str(" | ");
                }
                emit_lark_expr(x, false, out);
            }
            if need_parens && xs.len() > 1 {
                out.push(')');
            }
        }
        Expr::Opt(x) => {
            emit_lark_expr(x, true, out);
            out.push('?');
        }
        Expr::Star(x) => {
            emit_lark_expr(x, true, out);
            out.push('*');
        }
        Expr::Plus(x) => {
            emit_lark_expr(x, true, out);
            out.push('+');
        }
    }
}

fn emit_lark(g: &Grammar) -> String {
    let mut out = String::new();
    for r in &g.rules {
        if r.inline {
            out.push('?');
        }
        let _ = write!(out, "{}: ", r.name);
        emit_lark_expr(&r.body, false, &mut out);
        out.push('\n');
    }
    out.push_str("LABEL: \"@\" /[a-z_][A-Za-z0-9_]*/\n");
    out.push_str("IDENT: /[a-z_][A-Za-z0-9_]*/\n");
    out.push_str("OPNAME: /[A-Z][A-Z0-9_]*(?![A-Za-z0-9_])/\n");
    out.push_str("PORT: /%[0-9]+/\n");
    out.push_str("INT: /[0-9]+/\n");
    out.push_str("DECIMAL: /[0-9]+(\\.[0-9]+([eE][+-]?[0-9]+)?|[eE][+-]?[0-9]+)/\n");
    out.push_str("STRING: /\"([^\"\\\\]|\\\\.)*\"/\n");
    out.push_str("COMMENT: /\\/\\/[^\\n]*/\n");
    out.push_str("%import common.WS\n");
    out.push_str("%ignore WS\n");
    out.push_str("%ignore COMMENT\n");
    out
}

fn emit_llguidance(g: &Grammar) -> String {
    let mut out = String::from("%llguidance {}\n\n");
    for r in &g.rules {
        let _ = write!(out, "{}: ", r.name);
        emit_lark_expr(&r.body, false, &mut out);
        out.push('\n');
    }
    out.push('\n');
    out.push_str("LABEL: /@[a-z_][A-Za-z0-9_]*/\n");
    out.push_str("IDENT: /[a-z_][A-Za-z0-9_]*/\n");
    out.push_str("OPNAME: /[A-Z][A-Z0-9_]*/\n");
    out.push_str("PORT: /%[0-9]+/\n");
    out.push_str("INT: /[0-9]+/\n");
    out.push_str("DECIMAL: /[0-9]+(\\.[0-9]+([eE][+-]?[0-9]+)?|[eE][+-]?[0-9]+)/\n");
    out.push_str("STRING: /\"([^\"\\\\]|\\\\.)*\"/\n");
    out.push_str("WS: /[ \\t\\f\\r\\n]+/\n");
    out.push_str("COMMENT: /\\/\\/[^\\n]*/\n");
    out.push_str("%ignore WS\n");
    out.push_str("%ignore COMMENT\n");
    out
}

fn gbnf_rule_name(name: &str) -> &'static str {
    match name {
        "start" => "root",
        "mesh_query" => "mesh-query",
        "spec_def" => "spec-def",
        "node_def" => "node-def",
        "node_body" => "node-body",
        "fn_type" => "fn-type",
        "scalar_type" => "scalar-type",
        "int_type" => "int-type",
        "float_type" => "float-type",
        "or_" => "or-expr",
        "and_" => "and-expr",
        "cmp_op" => "cmp-op",
        "range_" => "range-expr",
        other => match other {
            "item" => "item",
            "attrs" => "attrs",
            "attr" => "attr",
            "params" => "params",
            "results" => "results",
            "clause" => "clause",
            "learnable" => "learnable",
            "stmt" => "stmt",
            "binding" => "binding",
            "op" => "op",
            "mode" => "mode",
            "args" => "args",
            "arg" => "arg",
            "value" => "value",
            "port" => "port",
            "literal" => "literal",
            "signed" => "signed",
            "number" => "number",
            "array" => "array",
            "element" => "element",
            "effects" => "effects",
            "effect" => "effect",
            "type" => "type",
            "shape" => "shape",
            "dim" => "dim",
            "handle" => "handle",
            "formula" => "formula",
            "cmp" => "cmp",
            "sum" => "sum",
            "prod" => "prod",
            "unary" => "unary",
            "postfix" => "postfix",
            "primary" => "primary",
            "call" => "call",
            _ => unreachable!("unknown rule {other}"),
        },
    }
}

fn gbnf_term_name(term: &str) -> &'static str {
    match term {
        "LABEL" => "label",
        "IDENT" => "ident",
        "OPNAME" => "opname",
        "PORT" => "port-tok",
        "INT" => "int-tok",
        "DECIMAL" => "decimal-tok",
        "STRING" => "string-tok",
        other => unreachable!("unknown terminal {other}"),
    }
}

fn emit_gbnf_expr(e: &Expr, need_parens: bool, out: &mut String) {
    match e {
        Expr::Lit(s) => {
            out.push_str(&quote_lit(s));
            out.push_str(" ws");
        }
        Expr::Term(t) => {
            out.push_str(gbnf_term_name(t));
            out.push_str(" ws");
        }
        Expr::Ref(r) => out.push_str(gbnf_rule_name(r)),
        Expr::Seq(xs) => {
            if need_parens && xs.len() > 1 {
                out.push('(');
            }
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push(' ');
                }
                let child_parens = matches!(x, Expr::Alt(_));
                emit_gbnf_expr(x, child_parens, out);
            }
            if need_parens && xs.len() > 1 {
                out.push(')');
            }
        }
        Expr::Alt(xs) => {
            if need_parens && xs.len() > 1 {
                out.push('(');
            }
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push_str(" | ");
                }
                emit_gbnf_expr(x, false, out);
            }
            if need_parens && xs.len() > 1 {
                out.push(')');
            }
        }
        Expr::Opt(x) => {
            emit_gbnf_expr(x, true, out);
            out.push('?');
        }
        Expr::Star(x) => {
            emit_gbnf_expr(x, true, out);
            out.push('*');
        }
        Expr::Plus(x) => {
            emit_gbnf_expr(x, true, out);
            out.push('+');
        }
    }
}

fn emit_gbnf(g: &Grammar) -> String {
    let mut out = String::new();
    for r in &g.rules {
        let name = gbnf_rule_name(r.name);
        let _ = write!(out, "{name} ::= ");
        if r.name == "start" {
            out.push_str("ws ");
            emit_gbnf_expr(&r.body, false, &mut out);
            out.push_str(" ws-eof");
        } else {
            emit_gbnf_expr(&r.body, false, &mut out);
        }
        out.push('\n');
    }
    out.push_str("label ::= \"@\" [a-z_] [a-zA-Z0-9_]*\n");
    out.push_str("ident ::= [a-z_] [a-zA-Z0-9_]*\n");
    out.push_str("opname ::= [A-Z] [A-Z0-9_]*\n");
    out.push_str("port-tok ::= \"%\" [0-9]+\n");
    out.push_str("int-tok ::= [0-9]+\n");
    out.push_str(
        "decimal-tok ::= [0-9]+ (\".\" [0-9]+ ([eE] [+-]? [0-9]+)? | [eE] [+-]? [0-9]+)\n",
    );
    out.push_str("string-tok ::= \"\\\"\" ([^\"\\\\] | \"\\\\\" [^\\n])* \"\\\"\"\n");
    out.push_str("ws ::= ([ \\t\\f\\r\\n] | \"//\" [^\\n]* \"\\n\")*\n");
    out.push_str("ws-eof ::= ws (\"//\" [^\\n]*)?\n");
    out
}

// ---------------------------------------------------------------------------
// In-crate Earley recognizer driven directly by `vgsym_grammar()`
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Sym {
    Nt(usize),
    Lit(&'static str),
    Term(&'static str),
}

struct BnfGrammar {
    start_nt: usize,
    prods: Vec<(usize, Vec<Sym>)>,
    by_nt: Vec<Vec<usize>>,
    nullable: Vec<bool>,
}

struct BnfBuilder {
    nt_ids: BTreeMap<String, usize>,
    prods: Vec<(usize, Vec<Sym>)>,
    synth_counter: usize,
}

impl BnfBuilder {
    fn intern(&mut self, name: &str) -> usize {
        if let Some(&id) = self.nt_ids.get(name) {
            return id;
        }
        let id = self.nt_ids.len();
        self.nt_ids.insert(name.to_string(), id);
        id
    }

    fn fresh_nt(&mut self) -> usize {
        let id = self.synth_counter;
        self.synth_counter += 1;
        let name = format!("__g{id}");
        self.intern(&name)
    }

    fn lower_seq(&mut self, e: &Expr, out: &mut Vec<Sym>) {
        match e {
            Expr::Lit(s) => out.push(Sym::Lit(s)),
            Expr::Term(t) => out.push(Sym::Term(t)),
            Expr::Ref(r) => {
                let id = self.intern(r);
                out.push(Sym::Nt(id));
            }
            Expr::Seq(xs) => {
                for x in xs {
                    self.lower_seq(x, out);
                }
            }
            Expr::Alt(xs) => {
                let helper = self.fresh_nt();
                for x in xs {
                    let mut rhs = Vec::new();
                    self.lower_seq(x, &mut rhs);
                    self.prods.push((helper, rhs));
                }
                out.push(Sym::Nt(helper));
            }
            Expr::Opt(x) => {
                let helper = self.fresh_nt();
                self.prods.push((helper, Vec::new()));
                let mut rhs = Vec::new();
                self.lower_seq(x, &mut rhs);
                self.prods.push((helper, rhs));
                out.push(Sym::Nt(helper));
            }
            Expr::Star(x) => {
                let helper = self.fresh_nt();
                self.prods.push((helper, Vec::new()));
                let mut rhs = vec![Sym::Nt(helper)];
                self.lower_seq(x, &mut rhs);
                self.prods.push((helper, rhs));
                out.push(Sym::Nt(helper));
            }
            Expr::Plus(x) => {
                let helper = self.fresh_nt();
                let mut base = Vec::new();
                self.lower_seq(x, &mut base);
                let mut rec = vec![Sym::Nt(helper)];
                rec.extend_from_slice(&base);
                self.prods.push((helper, base));
                self.prods.push((helper, rec));
                out.push(Sym::Nt(helper));
            }
        }
    }

    fn lower_rule(&mut self, nt_id: usize, body: &Expr) {
        match body {
            Expr::Alt(xs) => {
                for x in xs {
                    let mut rhs = Vec::new();
                    self.lower_seq(x, &mut rhs);
                    self.prods.push((nt_id, rhs));
                }
            }
            other => {
                let mut rhs = Vec::new();
                self.lower_seq(other, &mut rhs);
                self.prods.push((nt_id, rhs));
            }
        }
    }

    fn build(g: &Grammar) -> BnfGrammar {
        let mut b = BnfBuilder {
            nt_ids: BTreeMap::new(),
            prods: Vec::new(),
            synth_counter: 0,
        };
        for r in &g.rules {
            b.intern(r.name);
        }
        for r in &g.rules {
            let id = b.intern(r.name);
            b.lower_rule(id, &r.body);
        }
        let num_nts = b.nt_ids.len();
        let mut by_nt = vec![Vec::new(); num_nts];
        for (pi, (lhs, _)) in b.prods.iter().enumerate() {
            by_nt[*lhs].push(pi);
        }
        let mut nullable = vec![false; num_nts];
        loop {
            let mut changed = false;
            for (lhs, rhs) in &b.prods {
                if !nullable[*lhs]
                    && rhs
                        .iter()
                        .all(|s| matches!(s, Sym::Nt(id) if nullable[*id]))
                {
                    nullable[*lhs] = true;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let start_nt = b.nt_ids["start"];
        BnfGrammar {
            start_nt,
            prods: b.prods,
            by_nt,
            nullable,
        }
    }
}

fn sym_matches_tok(sym: Sym, tok: &Tok) -> bool {
    match (sym, tok) {
        (Sym::Lit(s), Tok::Kw(k)) => s == *k,
        (Sym::Lit(s), Tok::Punct(p)) => s == *p,
        (Sym::Term("IDENT"), Tok::Ident(_)) => true,
        (Sym::Term("OPNAME"), Tok::OpName(_)) => true,
        (Sym::Term("LABEL"), Tok::Label(_)) => true,
        (Sym::Term("PORT"), Tok::Port(_)) => true,
        (Sym::Term("INT"), Tok::Int(_)) => true,
        (Sym::Term("DECIMAL"), Tok::Decimal(_)) => true,
        (Sym::Term("STRING"), Tok::Str(_)) => true,
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Item {
    prod: usize,
    dot: usize,
    origin: usize,
}

fn push_item(col: &mut Vec<Item>, seen: &mut BTreeSet<Item>, item: Item) {
    if seen.insert(item) {
        col.push(item);
    }
}

/// Recognize a VG-Sym source string using the shared grammar description (parse-level only).
pub fn recognize(src: &str) -> Result<(), Diagnostic> {
    let tokens = lex(src)?;
    let bnf = BnfBuilder::build(&vgsym_grammar());
    let n = tokens.len().saturating_sub(1); // exclude trailing Tok::Eof
    let mut chart: Vec<Vec<Item>> = vec![Vec::new(); n + 1];
    let mut seen: Vec<BTreeSet<Item>> = vec![BTreeSet::new(); n + 1];

    for &pi in &bnf.by_nt[bnf.start_nt] {
        push_item(
            &mut chart[0],
            &mut seen[0],
            Item {
                prod: pi,
                dot: 0,
                origin: 0,
            },
        );
    }

    let mut furthest = 0usize;
    for i in 0..=n {
        let mut idx = 0;
        while idx < chart[i].len() {
            let item = chart[i][idx];
            idx += 1;
            let (lhs, rhs) = &bnf.prods[item.prod];
            if item.dot < rhs.len() {
                match rhs[item.dot] {
                    Sym::Nt(b) => {
                        for &pi in &bnf.by_nt[b] {
                            push_item(
                                &mut chart[i],
                                &mut seen[i],
                                Item {
                                    prod: pi,
                                    dot: 0,
                                    origin: i,
                                },
                            );
                        }
                        if bnf.nullable[b] {
                            push_item(
                                &mut chart[i],
                                &mut seen[i],
                                Item {
                                    prod: item.prod,
                                    dot: item.dot + 1,
                                    origin: item.origin,
                                },
                            );
                        }
                    }
                    sym => {
                        if i < n && sym_matches_tok(sym, &tokens[i].tok) {
                            furthest = furthest.max(i + 1);
                            push_item(
                                &mut chart[i + 1],
                                &mut seen[i + 1],
                                Item {
                                    prod: item.prod,
                                    dot: item.dot + 1,
                                    origin: item.origin,
                                },
                            );
                        }
                    }
                }
            } else {
                let completed_nt = *lhs;
                let origin = item.origin;
                let parents: Vec<Item> = chart[origin].clone();
                for p in parents {
                    let (_, p_rhs) = &bnf.prods[p.prod];
                    if p.dot < p_rhs.len() && p_rhs[p.dot] == Sym::Nt(completed_nt) {
                        push_item(
                            &mut chart[i],
                            &mut seen[i],
                            Item {
                                prod: p.prod,
                                dot: p.dot + 1,
                                origin: p.origin,
                            },
                        );
                    }
                }
            }
        }
    }

    let accepted = chart[n].iter().any(|it| {
        let (lhs, rhs) = &bnf.prods[it.prod];
        *lhs == bnf.start_nt && it.origin == 0 && it.dot == rhs.len()
    });
    if accepted {
        Ok(())
    } else {
        let span = tokens
            .get(furthest.min(n))
            .map(|t| t.span)
            .unwrap_or_else(Span::default);
        Err(Diagnostic::error(
            "VG-P0001",
            span,
            "unexpected token according to VG-Sym grammar",
        ))
    }
}
