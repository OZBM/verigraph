//! Prints a syntax tree as VG-Sym, in one fixed layout. Parsing the output gives back the same tree.

use crate::ast::*;
use std::fmt::Write;

pub fn print(m: &Module) -> String {
    let mut out = format!("module {};\n", m.name);
    for item in &m.items {
        out.push('\n');
        match item {
            Item::Query(q) => query(&mut out, q),
            Item::Node(n) => node(&mut out, n),
        }
    }
    out
}

fn query(out: &mut String, q: &MeshQuery) {
    let _ = writeln!(out, "@MESH_QUERY @{} {{", q.label);
    let params: Vec<_> = q.params.iter().map(ty).collect();
    let _ = writeln!(
        out,
        "  signature: ({}) -> ({});",
        params.join(", "),
        types(&q.results)
    );
    let _ = writeln!(out, "  effects: [{}];", names(&q.effects));
    if let Some(pre) = &q.pre {
        let _ = writeln!(out, "  pre: {};", formula(pre));
    }
    let _ = writeln!(out, "  post: {};", formula(&q.post));
    if let Some(i) = &q.intent {
        let _ = writeln!(out, "  intent: {i};");
    }
    out.push_str("}\n");
}

fn node(out: &mut String, n: &NodeDef) {
    out.push_str(if n.spec { "SPEC" } else { "NODE" });
    let _ = write!(out, " @{}", n.label);
    if !n.attrs.is_empty() {
        let attrs: Vec<String> = n
            .attrs
            .iter()
            .map(|a| match a {
                Attr::Intent(s) => format!("intent: {s}"),
                Attr::Export => "export".into(),
                Attr::Inline => "inline".into(),
            })
            .collect();
        let _ = write!(out, " [{}]", attrs.join(", "));
    }
    out.push_str(" {\n");
    let params: Vec<String> = n
        .params
        .iter()
        .map(|p| format!("{}: {}", p.name.text, ty(&p.ty)))
        .collect();
    let _ = writeln!(
        out,
        "  SIGNATURE: ({}) -> ({});",
        params.join(", "),
        types(&n.results)
    );
    let _ = writeln!(out, "  EFFECTS: [{}];", names(&n.effects));
    if n.clauses.is_empty() {
        out.push_str("  CONTRACT { }\n");
    } else {
        out.push_str("  CONTRACT {\n");
        for c in &n.clauses {
            let head = match &c.kind {
                ClauseKind::Pre => "PRE".to_string(),
                ClauseKind::Post => "POST".to_string(),
                ClauseKind::Decreases => "DECREASES".to_string(),
                ClauseKind::Cost => "COST".to_string(),
                ClauseKind::Budget => "BUDGET".to_string(),
                ClauseKind::Invariant { label, k, acc } => {
                    format!("INVARIANT @{}({}, {})", label.text, k.text, acc.text)
                }
            };
            let _ = writeln!(out, "    {head}: {};", formula(&c.formula));
        }
        out.push_str("  }\n");
    }
    if let Some(ls) = &n.learnable {
        out.push_str("  LEARNABLE {\n");
        for l in ls {
            let _ = write!(
                out,
                "    {}: {} in [{}, {}]",
                l.name.text,
                l.ty,
                snum(&l.lo),
                snum(&l.hi)
            );
            if let Some(loss) = &l.loss {
                let _ = write!(out, " loss: @{}", loss.text);
            }
            out.push_str(";\n");
        }
        out.push_str("  }\n");
    }
    out.push_str("  BODY {\n");
    for s in &n.body {
        out.push_str("    ");
        match s {
            Stmt::Assign {
                binds,
                op,
                mode,
                args,
                ..
            } => {
                let binds: Vec<String> = binds
                    .iter()
                    .map(|b| match &b.ty {
                        Some(t) => format!("%{}: {}", b.port.text, ty(t)),
                        None => format!("%{}", b.port.text),
                    })
                    .collect();
                let _ = write!(out, "{} = {}", binds.join(", "), op.text);
                if let Some(m) = mode {
                    let _ = write!(out, ".{}", m.text);
                }
                let args: Vec<String> = args
                    .iter()
                    .map(|a| {
                        let v = match &a.val {
                            ArgVal::Value(v) => value(v),
                            ArgVal::Label(l) => format!("@{}", l.text),
                        };
                        match &a.name {
                            Some(n) => format!("{}={v}", n.text),
                            None => v,
                        }
                    })
                    .collect();
                let _ = write!(out, "({});", args.join(", "));
            }
            Stmt::Return { values, .. } => {
                let vs: Vec<String> = values.iter().map(value).collect();
                let _ = write!(out, "RETURN({});", vs.join(", "));
            }
        }
        out.push('\n');
    }
    out.push_str("  }\n}\n");
}

fn names(ns: &[Name]) -> String {
    ns.iter()
        .map(|n| n.text.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn types(ts: &[TypeExpr]) -> String {
    ts.iter().map(ty).collect::<Vec<_>>().join(", ")
}

pub fn ty(t: &TypeExpr) -> String {
    match t {
        TypeExpr::Scalar(s) => s.to_string(),
        TypeExpr::Bool => "Bool".into(),
        TypeExpr::Eff => "Eff".into(),
        TypeExpr::Handle(h) => h.name().into(),
        TypeExpr::Array(e, s) => format!("Array<{}, {}>", ty(e), shape(s)),
        TypeExpr::Tensor(e, s) => format!("Tensor<{}, {}>", ty(e), shape(s)),
        TypeExpr::Slice(e) => format!("Slice<{}>", ty(e)),
        TypeExpr::Tuple(ms) => format!("Tuple<{}>", types(ms)),
        TypeExpr::Result(a, b) => format!("Result<{}, {}>", ty(a), ty(b)),
        TypeExpr::Arena(n) => format!("Arena<{n}>"),
        TypeExpr::Refine(n, b, f) => format!("{{{}: {} | {}}}", n.text, ty(b), formula(f)),
    }
}

fn shape(s: &Shape) -> String {
    let dims: Vec<&str> = s
        .dims
        .iter()
        .map(|d| match d {
            Dim::Int(s) | Dim::Ident(s) => s.as_str(),
        })
        .collect();
    if s.bracketed {
        format!("[{}]", dims.join(", "))
    } else {
        dims.join(", ")
    }
}

fn snum(n: &SNum) -> String {
    format!("{}{}", if n.neg { "-" } else { "" }, n.text)
}

pub fn value(v: &Value) -> String {
    match v {
        Value::Port(p) => format!("%{}", p.text),
        Value::Ident(n) => n.text.clone(),
        Value::Lit(l, _) => literal(l),
    }
}

fn literal(l: &Literal) -> String {
    match l {
        Literal::Num(n, t) => format!("{}:{t}", snum(n)),
        Literal::Bool(b) => b.to_string(),
        Literal::Array(es, t) => format!("{}:{}", elems(es), ty(t)),
    }
}

fn elems(es: &[Elem]) -> String {
    let v: Vec<String> = es
        .iter()
        .map(|e| match e {
            Elem::Num(n) => snum(n),
            Elem::Bool(b) => b.to_string(),
            Elem::Array(a) => elems(a),
        })
        .collect();
    format!("[{}]", v.join(", "))
}

/// Binding strength of a formula: 1 to 6 as `BinOp::level`, 7 unary, 8 postfix and primaries.
fn strength(f: &Formula) -> u8 {
    match &f.kind {
        FKind::Binary(op, ..) => op.level(),
        FKind::Unary(..) => 7,
        _ => 8,
    }
}

fn wrap(f: &Formula, min: u8) -> String {
    if strength(f) < min {
        format!("({})", formula(f))
    } else {
        formula(f)
    }
}

/// True when the formula prints starting with an unsigned typed number, which a `-` before it
/// would absorb into the literal.
fn starts_with_typed_number(f: &Formula) -> bool {
    match &f.kind {
        FKind::Value(Value::Lit(Literal::Num(n, _), _)) => !n.neg,
        FKind::Index(b, _) | FKind::Proj(b, _) => starts_with_typed_number(b),
        _ => false,
    }
}

pub fn formula(f: &Formula) -> String {
    match &f.kind {
        FKind::Value(v) => value(v),
        FKind::Num(s) => s.clone(),
        FKind::Result => "result".into(),
        FKind::Call { prefix, name, args } => {
            let p = match prefix {
                Some(CallPrefix::Spec) => "spec:",
                Some(CallPrefix::Fold) => "fold:",
                None => "",
            };
            let a: Vec<String> = args.iter().map(formula).collect();
            format!("{p}{}({})", name.text, a.join(", "))
        }
        FKind::Forall { ranges, body } => {
            let rs: Vec<String> = ranges
                .iter()
                .map(|r| format!("{} in {}..{}", r.var.text, wrap(&r.lo, 5), wrap(&r.hi, 5)))
                .collect();
            format!("forall({}) {{ {} }}", rs.join(", "), formula(body))
        }
        FKind::Unary(op, e) => {
            let sym = if *op == UnOp::Not { "!" } else { "-" };
            if *op == UnOp::Neg && starts_with_typed_number(e) {
                format!("{sym}({})", formula(e))
            } else {
                format!("{sym}{}", wrap(e, 7))
            }
        }
        FKind::Binary(op, a, b) => {
            let l = op.level();
            let (left, right) = match op {
                // Right-associative.
                BinOp::Implies => (wrap(a, 2), wrap(b, 1)),
                // A comparison takes no comparison as an operand.
                _ if l == 4 => (wrap(a, 5), wrap(b, 5)),
                _ => (wrap(a, l), wrap(b, l + 1)),
            };
            format!("{left} {} {right}", op.symbol())
        }
        FKind::Index(b, idx) => {
            let i: Vec<String> = idx.iter().map(formula).collect();
            format!("{}[{}]", wrap(b, 8), i.join(", "))
        }
        FKind::Proj(b, j) => {
            // `t.0.1` would lex `0.1` as a number, and `5.0` is one: parenthesize those bases.
            let base = if matches!(b.kind, FKind::Proj(..) | FKind::Num(_)) {
                format!("({})", formula(b))
            } else {
                wrap(b, 8)
            };
            format!("{base}.{j}")
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::parse;

    fn roundtrip(src: &str) {
        let a = parse(src).unwrap();
        let text = super::print(&a);
        let b = parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"));
        assert_eq!(a, b, "\n{text}");
        assert_eq!(super::print(&b), text);
    }

    #[test]
    fn tricky_formulas_survive_printing() {
        roundtrip(
            "module t; NODE @n [intent: \"a \\\"q\\\"\", export] { SIGNATURE: (x: i32, t: Tuple<Tuple<i32, i32>, i32>) -> (i32); EFFECTS: [];
             CONTRACT { PRE: -(5:i32) < x && -5:i32 < x && - -x > 0 && (x - (1 - 2)) * 3 == x / (2 * 4) % 5;
                        POST: (t.0).1 == x ==> (x > 1 ==> x > 0) && ((x > 1 ==> x > 0) ==> true) || !(x == 1);
                        POST: (5).0 == 1 && (a ==> b) == c && forall(i in 0..x - 1, j in -1..i) { [1, -2]:Array<i32, 2>[i] != j };
                        COST: ops <= 2 * len(x) + spec:f(fold:g(0, x, 1)); }
             LEARNABLE { g: f32 in [-0.5, 2e3] loss: @l; }
             BODY { %0: i32 = ADD.wrap(x, -128:i32); %1, %2 = CALL_HASH(@f, %0, x, true, [[1, 2], [3, 4]]:Tensor<u8, [2, 2]>); RETURN(%1); } }",
        );
    }
}
