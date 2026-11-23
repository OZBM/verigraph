//! Contract formula type checker over IR `Term`s (docs/03, docs/14, docs/15).

use std::collections::{BTreeMap, BTreeSet};

use vg_core::diag::{Diagnostic, Span};
use vg_core::ir::{Bin, Builtin, Hash, Node, Operand, Resource, Term};
use vg_core::lower::MAX_TERM_DEPTH;
use vg_core::opcode;
use vg_core::types::Type;

use crate::check::CalleeInfo;
use crate::cost::is_fixed_size;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClauseKind {
    Pre,
    Post,
    Cost,
    Budget,
    Decreases,
    Invariant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum FTy {
    Bool,
    Int,
    Float(Type),
    Val(Type),
}

fn err(code: &'static str, msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error(code, Span::default(), msg)
}

fn fty(t: &Type) -> Result<FTy, Diagnostic> {
    match t {
        Type::Bool => Ok(FTy::Bool),
        Type::UInt(_) | Type::SInt(_) | Type::Int => Ok(FTy::Int),
        Type::Float(_) | Type::BFloat16 => Ok(FTy::Float(t.clone())),
        Type::Eff | Type::Handle(_) => Err(err(
            "VG-T0119",
            format!("{t} cannot appear in a contract formula"),
        )),
        Type::Arena(_) => Err(err("VG-S0104", "Arena<N> is [ALPHA], not in the MVP")),
        Type::Array(..)
        | Type::Slice(..)
        | Type::Tensor(..)
        | Type::Tuple(..)
        | Type::Result(..) => {
            if contains_eff_or_handle(t) {
                return Err(err(
                    "VG-T0119",
                    format!("{t} contains Eff or a handle, which cannot appear in formulas"),
                ));
            }
            Ok(FTy::Val(t.clone()))
        }
    }
}

fn contains_eff_or_handle(t: &Type) -> bool {
    match t {
        Type::Eff | Type::Handle(_) => true,
        Type::Array(e, _) | Type::Slice(e) | Type::Tensor(e, _) => contains_eff_or_handle(e),
        Type::Tuple(ms) => ms.iter().any(contains_eff_or_handle),
        Type::Result(a, b) => contains_eff_or_handle(a) || contains_eff_or_handle(b),
        _ => false,
    }
}

fn has_byte_form(t: &Type) -> bool {
    match t {
        Type::Slice(e) => e.byte_size().is_some(),
        _ => t.byte_size().is_some(),
    }
}

pub fn check_contract(
    node: &Node,
    callees: &BTreeMap<Hash, CalleeInfo>,
    diags: &mut Vec<Diagnostic>,
) {
    let c = &node.contract;
    for (kind, terms) in [
        (ClauseKind::Pre, &c.pre),
        (ClauseKind::Post, &c.post),
        (ClauseKind::Cost, &c.cost),
        (ClauseKind::Budget, &c.budget),
        (ClauseKind::Decreases, &c.decreases),
    ] {
        for t in terms {
            check_clause_term(node, callees, kind, t, &[], diags);
        }
    }

    for (inv_idx, conj) in c.invariants.iter().enumerate() {
        let mut fold_ops: Vec<usize> = Vec::new();
        let mut acc_ty: Option<Type> = None;
        let mut acc_mismatch = false;
        for (op_idx, op) in node.ops.iter().enumerate() {
            if op.opcode == opcode::FOLD_BOUNDED
                && op.operands.last() == Some(&Operand::Invariant(inv_idx as u32))
            {
                fold_ops.push(op_idx);
                if let Some(res_ty) = op.results.first() {
                    match &acc_ty {
                        Some(prev) if prev != res_ty => {
                            acc_mismatch = true;
                        }
                        _ => acc_ty = Some(res_ty.clone()),
                    }
                }
            }
        }
        if acc_mismatch {
            diags.push(err(
                "VG-C0101",
                "one INVARIANT is used by folds with different accumulator types",
            ));
            continue;
        }
        // Check that no Term::Port in this invariant depends on any FOLD_BOUNDED using it (VG-S0007).
        for t in conj {
            let mut ports = Vec::new();
            collect_ports(t, &mut ports);
            for p in ports {
                if fold_ops
                    .iter()
                    .any(|&f_idx| op_depends_on(node, p as usize, f_idx))
                {
                    diags.push(err(
                        "VG-S0007",
                        "an INVARIANT names a port that depends on the fold that uses it",
                    ));
                }
            }
        }

        // Unused invariants (`fold_ops.is_empty()`) still have their formulas structurally checked
        // when bound_acc is unavailable, or if `acc_ty` is known we type with `[Int, acc]`.
        let mut bound = Vec::new();
        if let Some(acc) = &acc_ty {
            match fty(acc) {
                Ok(ft) => {
                    // Bound(1) is step count k, Bound(0) is accumulator acc.
                    bound.push(FTy::Int);
                    bound.push(ft);
                }
                Err(d) => {
                    diags.push(d);
                    continue;
                }
            }
        } else {
            // Even if no fold uses this invariant, verify non-bound terms and report if Bound(0)
            // cannot be typed without a fold (unused invariants in canonical nodes are empty anyway).
            for t in conj {
                if !check_depth(t, 1) {
                    diags.push(err("VG-S0203", "formula nested deeper than 256 levels"));
                }
            }
            continue;
        }
        for t in conj {
            check_clause_term(node, callees, ClauseKind::Invariant, t, &bound, diags);
        }
    }
}

fn check_clause_term(
    node: &Node,
    callees: &BTreeMap<Hash, CalleeInfo>,
    kind: ClauseKind,
    term: &Term,
    initial_bound: &[FTy],
    diags: &mut Vec<Diagnostic>,
) {
    if !check_depth(term, 1) {
        diags.push(err("VG-S0203", "formula nested deeper than 256 levels"));
        return;
    }
    let mut bound = initial_bound.to_vec();
    match infer_term(node, callees, kind, term, &mut bound) {
        Ok(got) => {
            let want = if kind == ClauseKind::Decreases {
                FTy::Int
            } else {
                FTy::Bool
            };
            if got != want {
                diags.push(err(
                    "VG-C0101",
                    format!("clause conjunct must have type {want:?}, found {got:?}"),
                ));
            }
        }
        Err(d) => diags.push(d),
    }
}

fn check_depth(t: &Term, d: usize) -> bool {
    if d > MAX_TERM_DEPTH {
        return false;
    }
    vg_core::canon::children(t)
        .into_iter()
        .all(|c| check_depth(c, d + 1))
}

fn collect_ports(t: &Term, out: &mut Vec<u32>) {
    if let Term::Port(i, _) = t {
        out.push(*i);
    }
    for c in vg_core::canon::children(t) {
        collect_ports(c, out);
    }
}

fn op_depends_on(node: &Node, start_op: usize, target_op: usize) -> bool {
    if start_op >= node.ops.len() || target_op >= node.ops.len() {
        return false;
    }
    let mut visited = BTreeSet::new();
    let mut stack = vec![start_op];
    while let Some(cur) = stack.pop() {
        if cur == target_op {
            return true;
        }
        if !visited.insert(cur) {
            continue;
        }
        if let Some(op) = node.ops.get(cur) {
            for o in &op.operands {
                if let Operand::Op(j, _) = o {
                    stack.push(*j as usize);
                }
            }
        }
    }
    false
}

fn infer_term(
    node: &Node,
    callees: &BTreeMap<Hash, CalleeInfo>,
    kind: ClauseKind,
    term: &Term,
    bound: &mut Vec<FTy>,
) -> Result<FTy, Diagnostic> {
    match term {
        Term::Input(i) => {
            let t = node
                .input_type(*i)
                .ok_or_else(|| err("VG-S0203", format!("formula input {i} out of range")))?;
            fty(t)
        }
        Term::Result(j) => {
            if kind != ClauseKind::Post {
                return Err(err("VG-C0104", "`result` is allowed only in POST"));
            }
            let t = node
                .results
                .get(*j as usize)
                .ok_or_else(|| err("VG-C0104", format!("result.{j} is out of range")))?;
            fty(t)
        }
        Term::Port(i, s) => {
            if kind != ClauseKind::Invariant {
                return Err(err(
                    "VG-C0105",
                    format!("port %{i} referenced outside an INVARIANT"),
                ));
            }
            if *i >= (1 << 24) {
                return Err(err("VG-S0203", "port op index exceeds 24-bit limit"));
            }
            let op = node
                .ops
                .get(*i as usize)
                .ok_or_else(|| err("VG-S0203", format!("port op {i} out of range")))?;
            let t = op
                .results
                .get(*s as usize)
                .ok_or_else(|| err("VG-S0203", format!("port op {i} slot {s} out of range")))?;
            fty(t)
        }
        Term::Bound(db) => {
            let idx = (*db as usize) + 1;
            if idx > bound.len() {
                return Err(err(
                    "VG-S0203",
                    format!("bound variable de Bruijn index {db} out of range"),
                ));
            }
            Ok(bound[bound.len() - idx].clone())
        }
        Term::Const(c) => {
            let ct = node
                .consts
                .get(*c as usize)
                .ok_or_else(|| err("VG-S0203", format!("formula constant {c} out of range")))?;
            fty(&ct.ty)
        }
        Term::Resource(r) => {
            match kind {
                ClauseKind::Cost => {
                    if *r == Resource::LatencyP99Us {
                        return Err(err(
                            "VG-C0106",
                            "latency_p99_us is a measured resource for BUDGET, not COST",
                        ));
                    }
                }
                ClauseKind::Budget => {}
                _ => {
                    return Err(err(
                        "VG-C0106",
                        format!("resource `{}` is allowed only in COST or BUDGET", r.name()),
                    ));
                }
            }
            Ok(FTy::Int)
        }
        Term::Not(a) => {
            let ta = infer_term(node, callees, kind, a, bound)?;
            if ta != FTy::Bool {
                return Err(err("VG-C0101", "`!` requires a Bool operand"));
            }
            Ok(FTy::Bool)
        }
        Term::Neg(a) => {
            let ta = infer_term(node, callees, kind, a, bound)?;
            match ta {
                FTy::Int | FTy::Float(_) => Ok(ta),
                _ => Err(err("VG-C0101", "unary `-` requires an integer or float")),
            }
        }
        Term::Bin(op, a, b) => {
            let ta = infer_term(node, callees, kind, a, bound)?;
            let tb = infer_term(node, callees, kind, b, bound)?;
            match op {
                Bin::And | Bin::Or | Bin::Implies => {
                    if ta != FTy::Bool || tb != FTy::Bool {
                        return Err(err("VG-C0101", "logical operator requires Bool operands"));
                    }
                    Ok(FTy::Bool)
                }
                Bin::Eq | Bin::Ne => {
                    let ok = match (&ta, &tb) {
                        (FTy::Bool, FTy::Bool) | (FTy::Int, FTy::Int) => true,
                        (FTy::Float(x), FTy::Float(y)) => x == y,
                        (FTy::Val(x), FTy::Val(y)) => x == y,
                        _ => false,
                    };
                    if !ok {
                        return Err(err(
                            "VG-C0101",
                            format!("equality operands have incompatible types: {ta:?} vs {tb:?}"),
                        ));
                    }
                    Ok(FTy::Bool)
                }
                Bin::Lt | Bin::Le | Bin::Gt | Bin::Ge => {
                    let ok = match (&ta, &tb) {
                        (FTy::Int, FTy::Int) => true,
                        (FTy::Float(x), FTy::Float(y)) => x == y,
                        _ => false,
                    };
                    if !ok {
                        return Err(err(
                            "VG-C0101",
                            format!(
                                "comparison operands have incompatible types: {ta:?} vs {tb:?}"
                            ),
                        ));
                    }
                    Ok(FTy::Bool)
                }
                Bin::Add | Bin::Sub | Bin::Mul | Bin::Div => match (&ta, &tb) {
                    (FTy::Int, FTy::Int) => Ok(FTy::Int),
                    (FTy::Float(x), FTy::Float(y)) if x == y => Ok(FTy::Float(x.clone())),
                    _ => Err(err(
                        "VG-C0101",
                        format!("arithmetic operands have incompatible types: {ta:?} vs {tb:?}"),
                    )),
                },
                Bin::Mod => {
                    if ta == FTy::Int && tb == FTy::Int {
                        Ok(FTy::Int)
                    } else {
                        Err(err("VG-C0101", "`%` requires integer operands"))
                    }
                }
            }
        }
        Term::Index(base, idx) => {
            let tb = infer_term(node, callees, kind, base, bound)?;
            let elem = match tb {
                FTy::Val(Type::Array(e, _) | Type::Slice(e)) if idx.len() == 1 => *e,
                FTy::Val(Type::Tensor(e, dims)) if idx.len() == dims.len() => *e,
                other => {
                    return Err(err(
                        "VG-C0101",
                        format!("cannot index {other:?} with {} indices", idx.len()),
                    ));
                }
            };
            for i in idx {
                let ti = infer_term(node, callees, kind, i, bound)?;
                if ti != FTy::Int {
                    return Err(err("VG-C0101", "collection index in formula must be Int"));
                }
            }
            fty(&elem)
        }
        Term::Proj(base, j) => {
            let tb = infer_term(node, callees, kind, base, bound)?;
            match tb {
                FTy::Val(Type::Tuple(ms)) => {
                    let mem = ms.get(*j as usize).ok_or_else(|| {
                        err("VG-C0101", format!("tuple projection .{j} out of range"))
                    })?;
                    fty(mem)
                }
                other => Err(err(
                    "VG-C0101",
                    format!("projection .{j} requires Tuple, found {other:?}"),
                )),
            }
        }
        Term::Forall(ranges, body) => {
            let depth0 = bound.len();
            for (lo, hi) in ranges {
                let tlo = infer_term(node, callees, kind, lo, bound);
                let thi = infer_term(node, callees, kind, hi, bound);
                match (tlo, thi) {
                    (Ok(FTy::Int), Ok(FTy::Int)) => {
                        bound.push(FTy::Int);
                    }
                    (Err(d), _) | (_, Err(d)) => {
                        bound.truncate(depth0);
                        return Err(d);
                    }
                    _ => {
                        bound.truncate(depth0);
                        return Err(err("VG-C0101", "forall range bounds must be integers"));
                    }
                }
            }
            let tbody = infer_term(node, callees, kind, body, bound);
            bound.truncate(depth0);
            if tbody? != FTy::Bool {
                return Err(err("VG-C0101", "forall body must be Bool"));
            }
            Ok(FTy::Bool)
        }
        Term::Builtin(b, args) => {
            if args.len() != b.arity() {
                return Err(err(
                    "VG-C0103",
                    format!("builtin {} takes {} argument(s)", b.name(), b.arity()),
                ));
            }
            let ts: Vec<FTy> = args
                .iter()
                .map(|a| infer_term(node, callees, kind, a, bound))
                .collect::<Result<_, _>>()?;
            match (b, ts.as_slice()) {
                (Builtin::Len, [FTy::Val(t)]) if t.elem().is_some() => Ok(FTy::Int),
                (Builtin::IsFinite | Builtin::IsNan, [FTy::Float(_)]) => Ok(FTy::Bool),
                (Builtin::Abs, [t @ (FTy::Int | FTy::Float(_))]) => Ok(t.clone()),
                (Builtin::Min | Builtin::Max, [FTy::Int, FTy::Int]) => Ok(FTy::Int),
                (Builtin::Min | Builtin::Max, [FTy::Float(a), FTy::Float(b)]) if a == b => {
                    Ok(FTy::Float(a.clone()))
                }
                (Builtin::AsBytes, [t]) => {
                    let ok = match t {
                        FTy::Bool | FTy::Int | FTy::Float(_) => true,
                        FTy::Val(v) => has_byte_form(v),
                    };
                    if ok {
                        Ok(FTy::Val(Type::Slice(Box::new(Type::UInt(8)))))
                    } else {
                        Err(err(
                            "VG-C0101",
                            "as_bytes requires a scalar, Bool, or collection with a byte form",
                        ))
                    }
                }
                (Builtin::Same, [x, y]) => {
                    let ok = match (x, y) {
                        (FTy::Bool, FTy::Bool) | (FTy::Int, FTy::Int) => true,
                        (FTy::Float(a), FTy::Float(b)) => a == b,
                        (FTy::Val(a), FTy::Val(b)) => a == b,
                        _ => false,
                    };
                    if ok {
                        Ok(FTy::Bool)
                    } else {
                        Err(err("VG-C0101", "same() arguments must share one type"))
                    }
                }
                (Builtin::IsOk, [FTy::Val(Type::Result(..))]) => Ok(FTy::Bool),
                (Builtin::Ok, [FTy::Val(Type::Result(ok, _))]) => fty(ok),
                (Builtin::Err, [FTy::Val(Type::Result(_, e))]) => fty(e),
                _ => Err(err(
                    "VG-C0101",
                    format!("{} does not accept argument types {ts:?}", b.name()),
                )),
            }
        }
        Term::Spec(ci, args) => {
            let h = node
                .calls
                .get(*ci as usize)
                .ok_or_else(|| err("VG-S0203", format!("spec CALLS index {ci} out of range")))?;
            let callee = callees
                .get(h)
                .ok_or_else(|| err("VG-S0009", "unresolvable spec callee hash"))?;
            if !callee.node.spec {
                return Err(err("VG-C0107", "spec: names a NODE, not a SPEC"));
            }
            if args.len() != callee.node.params.len() {
                return Err(err(
                    "VG-C0103",
                    format!(
                        "spec call takes {} arguments, not {}",
                        callee.node.params.len(),
                        args.len()
                    ),
                ));
            }
            for (arg, param) in args.iter().zip(&callee.node.params) {
                let targ = infer_term(node, callees, kind, arg, bound)?;
                let tparam = fty(param)?;
                check_formula_arg(&targ, &tparam)?;
            }
            callee_formula_result(&callee.node)
        }
        Term::Fold(ci, args) => {
            let h = node
                .calls
                .get(*ci as usize)
                .ok_or_else(|| err("VG-S0203", format!("fold CALLS index {ci} out of range")))?;
            let callee = callees
                .get(h)
                .ok_or_else(|| err("VG-S0009", "unresolvable fold callee hash"))?;
            let f = &callee.node;
            let shape_ok = !f.spec
                && f.effects.0 == 0
                && f.results.len() == 1
                && f.params.len() >= 2
                && f.params[0] == f.results[0]
                && f.params.iter().all(is_fixed_size)
                && is_fixed_size(&f.results[0])
                && args.len() == f.params.len() + 1;
            if !shape_ok {
                return Err(err(
                    "VG-C0108",
                    "fold: requires a pure fixed-size kernel (A, T, B...) -> (A) and (init, xs, k, b...)",
                ));
            }
            let t_init = infer_term(node, callees, kind, &args[0], bound)?;
            let want_init = fty(&f.params[0])?;
            check_formula_arg(&t_init, &want_init).map_err(|_| {
                err(
                    "VG-C0108",
                    "fold: initial accumulator does not match kernel",
                )
            })?;

            let t_xs = infer_term(node, callees, kind, &args[1], bound)?;
            let xs_ok = matches!(
                &t_xs,
                FTy::Val(Type::Array(e, _) | Type::Slice(e)) if **e == f.params[1]
            );
            if !xs_ok {
                return Err(err(
                    "VG-C0108",
                    "fold: collection must be Array<T, N> or Slice<T> matching kernel element type",
                ));
            }

            let t_k = infer_term(node, callees, kind, &args[2], bound)?;
            if t_k != FTy::Int {
                return Err(err("VG-C0108", "fold: step count k must be an integer"));
            }

            for (arg, param) in args[3..].iter().zip(&f.params[2..]) {
                let targ = infer_term(node, callees, kind, arg, bound)?;
                let tparam = fty(param)?;
                check_formula_arg(&targ, &tparam)
                    .map_err(|_| err("VG-C0108", "fold: broadcast argument does not fit kernel"))?;
            }
            callee_formula_result(f)
        }
        Term::Apply(ci, args) => {
            let h = node
                .calls
                .get(*ci as usize)
                .ok_or_else(|| err("VG-S0203", format!("apply CALLS index {ci} out of range")))?;
            let callee = callees
                .get(h)
                .ok_or_else(|| err("VG-S0009", "unresolvable kernel callee hash"))?;
            let f = &callee.node;
            let kernel_ok = !f.spec
                && f.effects.0 == 0
                && f.results.len() == 1
                && f.params.iter().all(is_fixed_size)
                && is_fixed_size(&f.results[0]);
            if !kernel_ok {
                return Err(err(
                    "VG-C0108",
                    "formula kernel application requires a pure fixed-size single-result NODE",
                ));
            }
            if args.len() != f.params.len() {
                return Err(err(
                    "VG-C0103",
                    format!(
                        "kernel application takes {} arguments, not {}",
                        f.params.len(),
                        args.len()
                    ),
                ));
            }
            for (arg, param) in args.iter().zip(&f.params) {
                let targ = infer_term(node, callees, kind, arg, bound)?;
                let tparam = fty(param)?;
                check_formula_arg(&targ, &tparam)?;
            }
            callee_formula_result(f)
        }
    }
}

fn check_formula_arg(arg: &FTy, param: &FTy) -> Result<(), Diagnostic> {
    let ok = match (arg, param) {
        (FTy::Val(Type::Array(e, _) | Type::Slice(e)), FTy::Val(Type::Slice(pe))) => e == pe,
        (a, b) => a == b,
    };
    if ok {
        Ok(())
    } else {
        Err(err(
            "VG-C0101",
            format!("formula call argument {arg:?} does not match parameter {param:?}"),
        ))
    }
}

fn callee_formula_result(f: &Node) -> Result<FTy, Diagnostic> {
    match f.results.as_slice() {
        [] => Err(err("VG-C0101", "formula callee has no results")),
        [t] => fty(t),
        ts => {
            let tup = Type::Tuple(ts.to_vec());
            if contains_eff_or_handle(&tup) {
                return Err(err(
                    "VG-T0119",
                    "Eff or a handle cannot appear in a formula call result",
                ));
            }
            Ok(FTy::Val(tup))
        }
    }
}
