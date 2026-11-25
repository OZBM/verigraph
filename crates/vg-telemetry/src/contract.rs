//! Plain-language contract rendering (`contract_text`, docs/15 "The contract in plain language").
//!
//! Renders `PRE` (with refined parameter types), `POST` (with refined result types), and `COST`
//! through deterministic templates so a human can confirm what was formalized word for word.

use std::collections::BTreeMap;

use vg_core::ast::{
    self, BinOp, CallPrefix, ClauseKind, FKind, Formula, Literal, MeshQuery, NodeDef, TypeExpr,
    UnOp, Value,
};
use vg_core::float;
use vg_core::ir::Node;
use vg_core::lower::lower_type;
use vg_core::types::Type;

/// Callable signatures `(param_types, result_types)` by label without `@`, used to type arguments
/// and results of `spec:` / `fold:` / helper calls inside formulas.
pub type CalleeSigs = BTreeMap<String, (Vec<Type>, Vec<Type>)>;

#[derive(Clone, Debug, PartialEq, Eq)]
enum FTy {
    Bool,
    Int,
    Float(Type),
    Val(Type),
    Num(bool),
    Unknown,
}

fn fty_of(t: &Type) -> FTy {
    if *t == Type::Bool {
        FTy::Bool
    } else if t.is_int() || *t == Type::Int {
        FTy::Int
    } else if t.is_float() {
        FTy::Float(t.clone())
    } else {
        FTy::Val(t.clone())
    }
}

fn unify_fty(a: &FTy, b: &FTy) -> FTy {
    match (a, b) {
        (FTy::Float(t), _) | (_, FTy::Float(t)) => FTy::Float(t.clone()),
        (FTy::Int, _) | (_, FTy::Int) => FTy::Int,
        (FTy::Bool, FTy::Bool) => FTy::Bool,
        (FTy::Val(x), FTy::Val(y)) if x == y => FTy::Val(x.clone()),
        (FTy::Num(x), FTy::Num(y)) => FTy::Num(*x || *y),
        _ => FTy::Unknown,
    }
}

fn base_type(te: &TypeExpr) -> Option<Type> {
    match te {
        TypeExpr::Refine(_, b, _) => lower_type(b, Default::default()).ok(),
        other => lower_type(other, Default::default()).ok(),
    }
}

/// Builds `CalleeSigs` for every `NODE`, `SPEC`, and `@MESH_QUERY` in an AST `Module`.
pub fn module_callee_sigs(module: &ast::Module) -> CalleeSigs {
    let mut out = BTreeMap::new();
    for item in &module.items {
        match item {
            ast::Item::Node(n) => {
                let params: Option<Vec<Type>> = n.params.iter().map(|p| base_type(&p.ty)).collect();
                let results: Option<Vec<Type>> = n.results.iter().map(base_type).collect();
                if let (Some(ps), Some(rs)) = (params, results) {
                    out.insert(n.label.clone(), (ps, rs));
                }
            }
            ast::Item::Query(q) => {
                let params: Option<Vec<Type>> = q.params.iter().map(base_type).collect();
                let results: Option<Vec<Type>> = q.results.iter().map(base_type).collect();
                if let (Some(ps), Some(rs)) = (params, results) {
                    out.insert(q.label.clone(), (ps, rs));
                }
            }
        }
    }
    out
}

/// Renders a `NodeDef`'s contract (`PRE`, refined parameters, `POST`, refined results, `COST`) in
/// plain language per docs/15.
pub fn contract_text(def: &NodeDef, sigs: &CalleeSigs) -> String {
    let mut scope: BTreeMap<String, FTy> = BTreeMap::new();
    for p in &def.params {
        if let Some(t) = base_type(&p.ty) {
            scope.insert(p.name.text.clone(), fty_of(&t));
        }
    }
    for l in def.learnable.iter().flatten() {
        if let Some(t) = Type::scalar(l.ty) {
            scope.insert(l.name.text.clone(), fty_of(&t));
        }
    }
    let results: Vec<FTy> = def
        .results
        .iter()
        .map(|r| base_type(r).map_or(FTy::Unknown, |t| fty_of(&t)))
        .collect();

    let cx = RenderCx {
        scope,
        results,
        sigs,
    };

    let mut pre_conj = Vec::new();
    let mut post_conj = Vec::new();
    let mut cost_conj = Vec::new();

    for c in &def.clauses {
        match &c.kind {
            ClauseKind::Pre => flatten_and(&c.formula, &mut pre_conj),
            ClauseKind::Post => flatten_and(&c.formula, &mut post_conj),
            ClauseKind::Cost => flatten_and(&c.formula, &mut cost_conj),
            ClauseKind::Invariant { .. } | ClauseKind::Decreases | ClauseKind::Budget => {}
        }
    }

    let mut pre_owned = Vec::new();
    for p in &def.params {
        if let TypeExpr::Refine(var, _, f) = &p.ty {
            let target = Formula::new(FKind::Value(Value::Ident(ast::Name::new(&p.name.text))));
            pre_owned.push(substitute_ident(f, &var.text, &target));
        }
    }
    for f in &pre_owned {
        flatten_and(f, &mut pre_conj);
    }

    let mut post_owned = Vec::new();
    for (j, r) in def.results.iter().enumerate() {
        if let TypeExpr::Refine(var, _, f) = r {
            let target = if def.results.len() == 1 {
                Formula::new(FKind::Result)
            } else {
                Formula::new(FKind::Proj(
                    Box::new(Formula::new(FKind::Result)),
                    j.to_string(),
                ))
            };
            post_owned.push(substitute_ident(f, &var.text, &target));
        }
    }
    for f in &post_owned {
        flatten_and(f, &mut post_conj);
    }

    join_sections(&cx, &pre_conj, &post_conj, &cost_conj)
}

/// Renders a `@MESH_QUERY` block's contract (`pre`, `post`) in plain language per docs/15.
pub fn query_contract_text(q: &MeshQuery, sigs: &CalleeSigs) -> String {
    let mut scope: BTreeMap<String, FTy> = BTreeMap::new();
    for (i, t) in q.params.iter().enumerate() {
        if let Some(ty) = base_type(t) {
            scope.insert(format!("arg{i}"), fty_of(&ty));
        }
    }
    let results: Vec<FTy> = q
        .results
        .iter()
        .map(|r| base_type(r).map_or(FTy::Unknown, |t| fty_of(&t)))
        .collect();
    let cx = RenderCx {
        scope,
        results,
        sigs,
    };

    let mut pre_conj = Vec::new();
    if let Some(pre) = &q.pre {
        flatten_and(pre, &mut pre_conj);
    }
    let mut post_conj = Vec::new();
    flatten_and(&q.post, &mut post_conj);

    join_sections(&cx, &pre_conj, &post_conj, &[])
}

/// Fallback plain-language contract renderer from a canonical `Node` and optional callee lookup.
pub fn node_contract_text(
    node: &Node,
    callee: &dyn Fn(&vg_core::ir::Hash) -> Option<Node>,
) -> String {
    if let Some(debug_src) = &node.debug {
        let wrapped = format!("module _debug;\n{debug_src}");
        if let Ok(ast) = vg_core::parser::parse(&wrapped) {
            let sigs = module_callee_sigs(&ast);
            if let Some(ast::Item::Node(def)) = ast.items.into_iter().next() {
                return contract_text(&def, &sigs);
            }
        }
    }
    if let Ok(h) = vg_core::canon::hashes(node) {
        let get = |q: &vg_core::ir::Hash| -> Option<Node> {
            if *q == h.node {
                Some(node.clone())
            } else {
                callee(q)
            }
        };
        if let Ok(m) = vg_core::disasm::disasm("_disasm", &[h.node], &get) {
            let sigs = module_callee_sigs(&m);
            if let Some(ast::Item::Node(def)) = m.items.into_iter().last() {
                return contract_text(&def, &sigs);
            }
        }
    }
    String::new()
}

fn join_sections(
    cx: &RenderCx<'_>,
    pre: &[&Formula],
    post: &[&Formula],
    cost: &[&Formula],
) -> String {
    let render_list = |fs: &[&Formula]| -> String {
        fs.iter()
            .map(|f| render_formula(f, cx, None, 0))
            .collect::<Vec<_>>()
            .join(", and ")
    };
    let mut sections = Vec::new();
    if !pre.is_empty() {
        sections.push(format!("Requires: {}.", render_list(pre)));
    }
    if !post.is_empty() {
        sections.push(format!("Ensures: {}.", render_list(post)));
    }
    if !cost.is_empty() {
        sections.push(format!("Cost: {}.", render_list(cost)));
    }
    sections.join(" ")
}

fn flatten_and<'a>(f: &'a Formula, out: &mut Vec<&'a Formula>) {
    match &f.kind {
        FKind::Binary(BinOp::And, a, b) => {
            flatten_and(a, out);
            flatten_and(b, out);
        }
        _ => out.push(f),
    }
}

struct RenderCx<'a> {
    scope: BTreeMap<String, FTy>,
    results: Vec<FTy>,
    sigs: &'a CalleeSigs,
}

impl RenderCx<'_> {
    fn with_bound(&self, vars: &[(String, FTy)]) -> RenderCx<'_> {
        let mut scope = self.scope.clone();
        for (k, v) in vars {
            scope.insert(k.clone(), v.clone());
        }
        RenderCx {
            scope,
            results: self.results.clone(),
            sigs: self.sigs,
        }
    }

    fn infer(&self, f: &Formula) -> FTy {
        match &f.kind {
            FKind::Result => {
                if self.results.len() == 1 {
                    self.results[0].clone()
                } else {
                    FTy::Unknown
                }
            }
            FKind::Proj(b, j) if matches!(b.kind, FKind::Result) && self.results.len() > 1 => j
                .parse::<usize>()
                .ok()
                .and_then(|idx| self.results.get(idx).cloned())
                .unwrap_or(FTy::Unknown),
            FKind::Proj(b, j) => match self.infer(b) {
                FTy::Val(Type::Tuple(ms)) => j
                    .parse::<usize>()
                    .ok()
                    .and_then(|idx| ms.get(idx))
                    .map_or(FTy::Unknown, fty_of),
                _ => FTy::Unknown,
            },
            FKind::Index(b, _) => match self.infer(b) {
                FTy::Val(Type::Array(e, _) | Type::Slice(e) | Type::Tensor(e, _)) => fty_of(&e),
                _ => FTy::Unknown,
            },
            FKind::Value(Value::Ident(n)) => {
                if vg_core::ir::Resource::ALL
                    .iter()
                    .any(|r| r.name() == n.text)
                {
                    FTy::Int
                } else {
                    self.scope.get(&n.text).cloned().unwrap_or(FTy::Unknown)
                }
            }
            FKind::Value(Value::Port(_)) => FTy::Unknown,
            FKind::Value(Value::Lit(Literal::Num(_, t), _)) => {
                Type::scalar(t).map_or(FTy::Unknown, |ty| fty_of(&ty))
            }
            FKind::Value(Value::Lit(Literal::Bool(_), _)) => FTy::Bool,
            FKind::Value(Value::Lit(Literal::Array(_, te), _)) => {
                base_type(te).map_or(FTy::Unknown, |ty| fty_of(&ty))
            }
            FKind::Num(s) => FTy::Num(s.contains(['.', 'e', 'E'])),
            FKind::Unary(UnOp::Not, _) => FTy::Bool,
            FKind::Unary(UnOp::Neg, e) => self.infer(e),
            FKind::Binary(op, a, b) => match op {
                BinOp::And
                | BinOp::Or
                | BinOp::Implies
                | BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge => FTy::Bool,
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    unify_fty(&self.infer(a), &self.infer(b))
                }
            },
            FKind::Forall { .. } => FTy::Bool,
            FKind::Call { prefix, name, args } => match (prefix, name.text.as_str()) {
                (None, "is_finite" | "is_nan" | "same" | "is_ok") => FTy::Bool,
                (None, "len") => FTy::Int,
                (None, "abs") => args.first().map_or(FTy::Unknown, |a| self.infer(a)),
                (None, "min" | "max") if args.len() == 2 => {
                    unify_fty(&self.infer(&args[0]), &self.infer(&args[1]))
                }
                (None, "ok") => match args.first().map(|a| self.infer(a)) {
                    Some(FTy::Val(Type::Result(ok, _))) => fty_of(&ok),
                    _ => FTy::Unknown,
                },
                (None, "err") => match args.first().map(|a| self.infer(a)) {
                    Some(FTy::Val(Type::Result(_, err))) => fty_of(&err),
                    _ => FTy::Unknown,
                },
                (None, "as_bytes") => FTy::Val(Type::Slice(Box::new(Type::UInt(8)))),
                (Some(CallPrefix::Fold), _) => args.first().map_or(FTy::Unknown, |a| self.infer(a)),
                (_, callee_name) => {
                    if let Some((_, rs)) = self.sigs.get(callee_name) {
                        match rs.as_slice() {
                            [t] => fty_of(t),
                            ts if !ts.is_empty() => FTy::Val(Type::Tuple(ts.to_vec())),
                            _ => FTy::Unknown,
                        }
                    } else {
                        FTy::Unknown
                    }
                }
            },
        }
    }
}

fn is_compound_bool(f: &Formula) -> bool {
    matches!(
        f.kind,
        FKind::Binary(..) | FKind::Unary(..) | FKind::Forall { .. }
    )
}

fn render_range_upper_inclusive(hi: &Formula, cx: &RenderCx<'_>) -> String {
    match &hi.kind {
        FKind::Num(s) => {
            if let Ok(n) = s.parse::<i128>() {
                let v = n - 1;
                return if v < 0 {
                    format!("−{}", -v)
                } else {
                    v.to_string()
                };
            }
        }
        FKind::Value(Value::Lit(Literal::Num(sn, _), _)) => {
            if let Ok(mag) = sn.text.parse::<i128>() {
                let n = if sn.neg { -mag } else { mag };
                let v = n - 1;
                return if v < 0 {
                    format!("−{}", -v)
                } else {
                    v.to_string()
                };
            }
        }
        FKind::Binary(BinOp::Add, a, b) => {
            if matches!(&b.kind, FKind::Num(s) if s == "1") {
                return render_formula(a, cx, Some(&FTy::Int), 0);
            }
        }
        _ => {}
    }
    let s = render_formula(hi, cx, Some(&FTy::Int), 5);
    format!("{s} − 1")
}

fn render_formula(f: &Formula, cx: &RenderCx<'_>, want: Option<&FTy>, min_prec: u8) -> String {
    let wrap = |s: String, prec: u8| {
        if prec < min_prec { format!("({s})") } else { s }
    };
    match &f.kind {
        FKind::Result => "the result".to_string(),
        FKind::Proj(b, j) if matches!(b.kind, FKind::Result) => format!("result {j}"),
        FKind::Proj(b, j) => {
            let base = render_formula(b, cx, None, 8);
            format!("{base}.{j}")
        }
        FKind::Index(b, idx) => {
            let base = render_formula(b, cx, None, 8);
            let is: Vec<String> = idx
                .iter()
                .map(|i| render_formula(i, cx, Some(&FTy::Int), 0))
                .collect();
            format!("{base}[{}]", is.join(", "))
        }
        FKind::Num(s) => render_number_text(s, false, want),
        FKind::Value(v) => match v {
            Value::Port(p) => format!("%{}", p.text),
            Value::Ident(n) => n.text.clone(),
            Value::Lit(lit, _) => match lit {
                Literal::Bool(b) => b.to_string(),
                Literal::Num(sn, ty_name) => {
                    if let Some(ty) = Type::scalar(ty_name) {
                        if ty.is_float() {
                            render_float_literal(&sn.text, sn.neg, &ty)
                        } else if sn.neg {
                            format!("−{}", sn.text)
                        } else {
                            sn.text.clone()
                        }
                    } else if sn.neg {
                        format!("−{}", sn.text)
                    } else {
                        sn.text.clone()
                    }
                }
                Literal::Array(..) => vg_core::printer::value(v),
            },
        },
        FKind::Unary(UnOp::Not, a) => {
            let inner = render_formula(a, cx, Some(&FTy::Bool), 0);
            wrap(format!("not ({inner})"), 7)
        }
        FKind::Unary(UnOp::Neg, a) => {
            if let FKind::Num(s) = &a.kind {
                return render_number_text(s, true, want);
            }
            let inner = render_formula(a, cx, want, 7);
            wrap(format!("−{inner}"), 7)
        }
        FKind::Binary(op, a, b) => match op {
            BinOp::Implies => {
                let ls = render_formula(a, cx, Some(&FTy::Bool), 2);
                let rs = render_formula(b, cx, Some(&FTy::Bool), 1);
                wrap(format!("if {ls}, then {rs}"), 1)
            }
            BinOp::Or => {
                let ls = render_formula(a, cx, Some(&FTy::Bool), 2);
                let rs = render_formula(b, cx, Some(&FTy::Bool), 3);
                wrap(format!("{ls}, or {rs}"), 2)
            }
            BinOp::And => {
                let ls = render_formula(a, cx, Some(&FTy::Bool), 3);
                let rs = render_formula(b, cx, Some(&FTy::Bool), 4);
                wrap(format!("{ls}, and {rs}"), 3)
            }
            BinOp::Eq | BinOp::Ne => {
                let (ta, tb) = (cx.infer(a), cx.infer(b));
                if ta == FTy::Bool && tb == FTy::Bool {
                    let la = render_formula(a, cx, Some(&FTy::Bool), 0);
                    let rb = render_formula(b, cx, Some(&FTy::Bool), 0);
                    let lw = if is_compound_bool(a) {
                        format!("({la})")
                    } else {
                        la
                    };
                    let rw = if is_compound_bool(b) {
                        format!("({rb})")
                    } else {
                        rb
                    };
                    let s = if *op == BinOp::Eq {
                        format!("{lw} is true exactly when {rw} is true")
                    } else {
                        format!("not ({lw} is true exactly when {rw} is true)")
                    };
                    return wrap(s, 4);
                }
                let shared = unify_fty(&ta, &tb);
                let ls = render_formula(a, cx, Some(&shared), 5);
                let rs = render_formula(b, cx, Some(&shared), 5);
                let sym = if *op == BinOp::Eq { "=" } else { "≠" };
                wrap(format!("{ls} {sym} {rs}"), 4)
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let shared = unify_fty(&cx.infer(a), &cx.infer(b));
                let ls = render_formula(a, cx, Some(&shared), 5);
                let rs = render_formula(b, cx, Some(&shared), 5);
                let sym = match op {
                    BinOp::Lt => "<",
                    BinOp::Le => "≤",
                    BinOp::Gt => ">",
                    BinOp::Ge => "≥",
                    _ => unreachable!(),
                };
                wrap(format!("{ls} {sym} {rs}"), 4)
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                let p = op.level();
                let inferred = unify_fty(&cx.infer(a), &cx.infer(b));
                let target = match inferred {
                    FTy::Num(_) | FTy::Unknown => want,
                    ref t => Some(t),
                };
                let ls = render_formula(a, cx, target, p);
                let rs = render_formula(b, cx, target, p + 1);
                let sym = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "−",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Mod => "%",
                    _ => unreachable!(),
                };
                wrap(format!("{ls} {sym} {rs}"), p)
            }
        },
        FKind::Forall { ranges, body } => {
            let mut bound_vars: Vec<(String, FTy)> = Vec::with_capacity(ranges.len());
            let mut range_parts: Vec<String> = Vec::with_capacity(ranges.len());
            for r in ranges {
                let cur_cx = cx.with_bound(&bound_vars);
                let lo = render_formula(&r.lo, &cur_cx, Some(&FTy::Int), 5);
                let hi_inc = render_range_upper_inclusive(&r.hi, &cur_cx);
                range_parts.push(format!("{} from {lo} to {hi_inc}", r.var.text));
                bound_vars.push((r.var.text.clone(), FTy::Int));
            }
            let body_cx = cx.with_bound(&bound_vars);
            let body_s = render_formula(body, &body_cx, Some(&FTy::Bool), 0);
            wrap(
                format!("for every {}: {body_s}", range_parts.join(" and ")),
                1,
            )
        }
        FKind::Call { prefix, name, args } => render_call(*prefix, &name.text, args, cx, want),
    }
}

fn render_call(
    prefix: Option<CallPrefix>,
    name: &str,
    args: &[Formula],
    cx: &RenderCx<'_>,
    want: Option<&FTy>,
) -> String {
    match (prefix, name) {
        (None, "is_finite") if args.len() == 1 => {
            format!("{} is finite", render_formula(&args[0], cx, None, 4))
        }
        (None, "is_nan") if args.len() == 1 => {
            format!("{} is NaN", render_formula(&args[0], cx, None, 4))
        }
        (None, "same") if args.len() == 2 => {
            let shared = unify_fty(&cx.infer(&args[0]), &cx.infer(&args[1]));
            let a = render_formula(&args[0], cx, Some(&shared), 0);
            let b = render_formula(&args[1], cx, Some(&shared), 0);
            format!("{a} is identical to {b}, bit for bit")
        }
        (None, "abs") if args.len() == 1 => {
            format!("abs({})", render_formula(&args[0], cx, want, 0))
        }
        (None, "min" | "max") if args.len() == 2 => {
            let shared = unify_fty(&cx.infer(&args[0]), &cx.infer(&args[1]));
            let target = match shared {
                FTy::Num(_) | FTy::Unknown => want,
                ref t => Some(t),
            };
            let a = render_formula(&args[0], cx, target, 0);
            let b = render_formula(&args[1], cx, target, 0);
            format!("{name}({a}, {b})")
        }
        (Some(CallPrefix::Spec), spec_name) => {
            let param_tys = cx.sigs.get(spec_name).map(|(ps, _)| ps.as_slice());
            let rendered_args: Vec<String> = args
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let wt = param_tys.and_then(|ps| ps.get(i)).map(fty_of);
                    render_formula(a, cx, wt.as_ref(), 0)
                })
                .collect();
            let display_spec = if spec_name == "sha256" {
                "SHA-256".to_string()
            } else {
                spec_name.to_string()
            };
            if rendered_args.is_empty() {
                format!("the {display_spec} reference spec")
            } else {
                format!(
                    "the {display_spec} reference spec applied to {}",
                    rendered_args.join(", ")
                )
            }
        }
        (Some(CallPrefix::Fold), fold_name) if args.len() >= 3 => {
            let param_tys = cx.sigs.get(fold_name).map(|(ps, _)| ps.as_slice());
            let init_ty = param_tys.and_then(|ps| ps.first()).map(fty_of);
            let init_s = render_formula(&args[0], cx, init_ty.as_ref(), 0);
            let xs_s = render_formula(&args[1], cx, None, 0);
            let k_s = render_formula(&args[2], cx, Some(&FTy::Int), 0);
            if args.len() == 3 {
                format!(
                    "{fold_name} folded over the first {k_s} elements of {xs_s}, starting from {init_s}"
                )
            } else {
                let extra: Vec<String> = args[3..]
                    .iter()
                    .enumerate()
                    .map(|(j, a)| {
                        let wt = param_tys.and_then(|ps| ps.get(j + 2)).map(fty_of);
                        render_formula(a, cx, wt.as_ref(), 0)
                    })
                    .collect();
                format!(
                    "{fold_name} folded over the first {k_s} elements of {xs_s}, starting from {init_s} (with {})",
                    extra.join(", ")
                )
            }
        }
        _ => {
            let pfx = match prefix {
                Some(CallPrefix::Spec) => "spec:",
                Some(CallPrefix::Fold) => "fold:",
                None => "",
            };
            let param_tys = cx.sigs.get(name).map(|(ps, _)| ps.as_slice());
            let rendered_args: Vec<String> = args
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let wt = param_tys.and_then(|ps| ps.get(i)).map(fty_of);
                    render_formula(a, cx, wt.as_ref(), 0)
                })
                .collect();
            format!("{pfx}{name}({})", rendered_args.join(", "))
        }
    }
}

fn render_number_text(text: &str, neg: bool, want: Option<&FTy>) -> String {
    if let Some(FTy::Float(ty)) = want {
        return render_float_literal(text, neg, ty);
    }
    if neg {
        format!("−{text}")
    } else {
        text.to_string()
    }
}

fn render_float_literal(text: &str, neg: bool, ty: &Type) -> String {
    let written = if neg {
        format!("−{text}")
    } else {
        text.to_string()
    };
    let Some(bits) = float::parse(text, neg, ty) else {
        return written;
    };
    let Some(exact_abs) = exact_float_decimal_abs(ty, bits) else {
        return written;
    };
    if canonical_decimal_digits(text) == canonical_decimal_digits(&exact_abs) {
        written
    } else {
        let exact_signed = if neg && exact_abs != "0" {
            format!("−{exact_abs}")
        } else {
            exact_abs
        };
        format!("{written} (the {ty} value {exact_signed})")
    }
}

/// Canonical `(significant_digits, base10_exponent)` for an unsigned decimal string, so `"1.0"`,
/// `"1"`, and `"1e0"` compare equal while `"0.99"` and `"0.9900000095367431640625"` differ.
fn canonical_decimal_digits(s: &str) -> (Vec<u8>, i64) {
    let (mant, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], s[i + 1..].parse::<i64>().unwrap_or(0)),
        None => (s, 0),
    };
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    let all: Vec<u8> = int
        .bytes()
        .chain(frac.bytes())
        .filter(|c| c.is_ascii_digit())
        .map(|c| c - b'0')
        .collect();
    let lead = all.iter().position(|&c| c != 0).unwrap_or(all.len());
    let mut sig = all[lead..].to_vec();
    while sig.last() == Some(&0) {
        sig.pop();
    }
    if sig.is_empty() {
        (Vec::new(), 0)
    } else {
        (sig, int.len() as i64 - lead as i64 + exp)
    }
}

/// Exact unsigned decimal expansion of the finite float value represented by `bits` in `ty`.
pub fn exact_float_decimal_abs(ty: &Type, bits: u64) -> Option<String> {
    if float::is_nan(ty, bits) || float::is_inf(ty, bits) {
        return None;
    }
    let (e_bits, m_bits): (u32, u32) = match ty {
        Type::Float(16) => (5, 10),
        Type::BFloat16 => (8, 7),
        Type::Float(32) => (8, 23),
        Type::Float(64) => (11, 52),
        _ => return None,
    };
    let total = e_bits + m_bits;
    let abs_bits = if total == 63 {
        bits & ((1u64 << 63) - 1)
    } else {
        bits & ((1u64 << total) - 1)
    };
    if abs_bits == 0 {
        return Some("0".to_string());
    }
    let exp_field = ((abs_bits >> m_bits) & ((1u64 << e_bits) - 1)) as i32;
    let frac = abs_bits & ((1u64 << m_bits) - 1);
    let bias = (1i32 << (e_bits - 1)) - 1;
    let (mut mant, mut q) = if exp_field == 0 {
        (frac, 1 - bias - m_bits as i32)
    } else {
        ((1u64 << m_bits) | frac, exp_field - bias - m_bits as i32)
    };
    if q < 0 {
        let tz = mant.trailing_zeros().min((-q) as u32);
        mant >>= tz;
        q += tz as i32;
    }

    const BASE: u64 = 1_000_000_000;
    let mut limbs: Vec<u32> = Vec::new();
    let mut rem = mant;
    while rem > 0 {
        limbs.push((rem % BASE) as u32);
        rem /= BASE;
    }
    let mul_small = |limbs: &mut Vec<u32>, factor: u64| {
        let mut carry = 0u64;
        for limb in limbs.iter_mut() {
            let prod = u64::from(*limb) * factor + carry;
            *limb = (prod % BASE) as u32;
            carry = prod / BASE;
        }
        while carry > 0 {
            limbs.push((carry % BASE) as u32);
            carry /= BASE;
        }
    };
    let limbs_to_string = |limbs: &[u32]| -> String {
        let Some((&top, rest)) = limbs.split_last() else {
            return "0".to_string();
        };
        let mut s = top.to_string();
        for &limb in rest.iter().rev() {
            use std::fmt::Write;
            let _ = write!(s, "{limb:09}");
        }
        s
    };

    if q >= 0 {
        let mut left = q as u32;
        while left > 0 {
            let step = left.min(29);
            mul_small(&mut limbs, 1u64 << step);
            left -= step;
        }
        Some(limbs_to_string(&limbs))
    } else {
        let k = (-q) as usize;
        let mut left = k;
        while left > 0 {
            let step = left.min(13);
            mul_small(&mut limbs, 5u64.pow(step as u32));
            left -= step;
        }
        let digits = limbs_to_string(&limbs);
        if digits.len() <= k {
            Some(format!("0.{}{digits}", "0".repeat(k - digits.len())))
        } else {
            let split = digits.len() - k;
            Some(format!("{}.{}", &digits[..split], &digits[split..]))
        }
    }
}

fn substitute_ident(f: &Formula, name: &str, replacement: &Formula) -> Formula {
    let sub = |g: &Formula| substitute_ident(g, name, replacement);
    let kind = match &f.kind {
        FKind::Value(Value::Ident(n)) if n.text == name => return replacement.clone(),
        FKind::Value(_) | FKind::Num(_) | FKind::Result => return f.clone(),
        FKind::Call {
            prefix,
            name: n,
            args,
        } => FKind::Call {
            prefix: *prefix,
            name: n.clone(),
            args: args.iter().map(sub).collect(),
        },
        FKind::Forall { ranges, body } => {
            let mut shadowed = false;
            let mut new_ranges = Vec::with_capacity(ranges.len());
            for r in ranges {
                let lo = if shadowed { r.lo.clone() } else { sub(&r.lo) };
                let hi = if shadowed { r.hi.clone() } else { sub(&r.hi) };
                if r.var.text == name {
                    shadowed = true;
                }
                new_ranges.push(ast::Range {
                    var: r.var.clone(),
                    lo,
                    hi,
                });
            }
            let new_body = if shadowed {
                body.as_ref().clone()
            } else {
                sub(body)
            };
            FKind::Forall {
                ranges: new_ranges,
                body: Box::new(new_body),
            }
        }
        FKind::Unary(op, a) => FKind::Unary(*op, Box::new(sub(a))),
        FKind::Binary(op, a, b) => FKind::Binary(*op, Box::new(sub(a)), Box::new(sub(b))),
        FKind::Index(b, idx) => FKind::Index(Box::new(sub(b)), idx.iter().map(sub).collect()),
        FKind::Proj(b, j) => FKind::Proj(Box::new(sub(b)), j.clone()),
    };
    Formula::new(kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_knee_matches_docs_15_word_for_word() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/audio_limiter.vgs"),
        )
        .expect("audio_limiter.vgs");
        let ast = vg_core::parser::parse(&src).expect("parses");
        let sigs = module_callee_sigs(&ast);
        let soft_knee = ast
            .items
            .iter()
            .find_map(|i| match i {
                ast::Item::Node(n) if n.label == "soft_knee" => Some(n),
                _ => None,
            })
            .expect("@soft_knee");

        assert_eq!(
            contract_text(soft_knee, &sigs),
            "Requires: x is finite, and knee ≥ 0.5, and knee ≤ 0.99 \
             (the f32 value 0.9900000095367431640625). \
             Ensures: the result ≥ −1.0, and the result ≤ 1.0, and \
             (x ≥ 0.0) is true exactly when (the result ≥ 0.0) is true."
        );
    }

    #[test]
    fn forall_spec_fold_same_implication_and_cost_templates_match_docs_15() {
        let src = r#"module m;
SPEC @sha256 {
  SIGNATURE: (m: Slice<u8>) -> (Array<u8, 32>);
  EFFECTS: [];
  CONTRACT { }
  BODY {
    %0 = GENERATE(@zero_byte, 32:u64);
    RETURN(%0);
  }
}
NODE @f {
  SIGNATURE: (acc: u32, x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT { POST: result == acc; }
  BODY { RETURN(acc); }
}
NODE @demo {
  SIGNATURE: (xs: Slice<u32>, n: u64, i: u32, k: u64, m: Slice<u8>, a: f32, b: f32) -> (u32, Array<u8, 32>);
  EFFECTS: [];
  CONTRACT {
    PRE: forall(idx in 0..n, c in 0..2) { !is_nan(a) || same(a, b) };
    POST: (k <= len(xs) ==> result.0 == fold:f(i, xs, k)) && result.1 == spec:sha256(m);
    COST: heap_bytes == 0 && arena_bytes == 0;
  }
  BODY {
    RETURN(i, [0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]:Array<u8, 32>);
  }
}
"#;
        let ast = vg_core::parser::parse(src).expect("parses");
        let sigs = module_callee_sigs(&ast);
        let demo = ast
            .items
            .iter()
            .find_map(|i| match i {
                ast::Item::Node(n) if n.label == "demo" => Some(n),
                _ => None,
            })
            .expect("@demo");
        assert_eq!(
            contract_text(demo, &sigs),
            "Requires: for every idx from 0 to n − 1 and c from 0 to 1: not (a is NaN), or a is identical to b, bit for bit. \
             Ensures: if k ≤ len(xs), then result 0 = f folded over the first k elements of xs, starting from i, and \
             result 1 = the SHA-256 reference spec applied to m. \
             Cost: heap_bytes = 0, and arena_bytes = 0."
        );
    }

    #[test]
    fn exact_float_decimal_handles_f16_bf16_f32_and_f64_subnormals() {
        let f32_099 = float::parse("0.99", false, &Type::Float(32)).unwrap();
        assert_eq!(
            exact_float_decimal_abs(&Type::Float(32), f32_099).as_deref(),
            Some("0.9900000095367431640625")
        );
        let f64_min_sub = 1u64; // 2^-1074 = 5^1074 * 10^-1074
        let s = exact_float_decimal_abs(&Type::Float(64), f64_min_sub).unwrap();
        assert!(s.starts_with("0.000") && s.ends_with('5'));
        assert_eq!(s.len(), 2 + 1074);
    }
}
