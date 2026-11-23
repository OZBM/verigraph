//! Dynamic backward slicing, counterfactual passing-input discovery, Zeller's delta debugging
//! (`ddmin`), and value-passthrough collapse (P6.1).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value as J;
use vg_core::float;
use vg_core::ir::{Hash, Node, Operand, Term};
use vg_core::opcode;
use vg_core::types::Type;
use vg_core::value::Value;

use crate::diff::SmtObligationContext;
use crate::names::Names;
use crate::report::SliceEntry;
use crate::trace::{
    ObligationKind, Trace, check_violation, parse_obligation_id, pre_and_learnable_hold,
    trace_eval, trace_eval_with_overrides, value_to_report_json,
};

/// Computes the pruned backward slice (as `SliceEntry` items in canonical order) for `obligation`
/// on `inputs`.
pub fn slice(
    node: &Node,
    obligation: &str,
    names: &Names,
    inputs: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Vec<SliceEntry> {
    let trace_fail = trace_eval(node, inputs, callee);
    slice_with_trace(node, obligation, names, inputs, &trace_fail, callee).1
}

/// Computes `(canonical_op_indices, formatted_slice_entries)` using an existing counterexample trace.
pub fn slice_with_trace(
    node: &Node,
    obligation: &str,
    names: &Names,
    inputs: &[Value],
    trace_fail: &Trace,
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> (Vec<u32>, Vec<SliceEntry>) {
    let Some(kind) = parse_obligation_id(obligation) else {
        return (Vec::new(), Vec::new());
    };
    let roots = root_ops(node, &kind);
    if roots.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let cone = dynamic_backward_cone(node, &roots, trace_fail);
    if cone.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let chk_fail = check_violation(node, &kind, inputs, trace_fail, callee);
    let kept_ops = if chk_fail.pre_holds && chk_fail.violated {
        if let Some(inputs_pass) = find_passing_inputs(node, &kind, inputs, callee) {
            let trace_pass = trace_eval(node, &inputs_pass, callee);
            prune_with_ddmin(
                node,
                &kind,
                &roots,
                &cone,
                inputs,
                trace_fail,
                &trace_pass,
                callee,
            )
        } else {
            cone
        }
    } else {
        cone
    };

    let final_ops = if kept_ops.len() > 5 {
        collapse_passthroughs(node, &roots, &kept_ops, trace_fail)
    } else {
        kept_ops
    };

    let entries = format_slice_entries(node, names, &final_ops, trace_fail);
    (final_ops, entries)
}

fn root_ops(node: &Node, kind: &ObligationKind) -> BTreeSet<u32> {
    let mut roots = BTreeSet::new();
    match kind {
        ObligationKind::Post(j) | ObligationKind::PostWd(j, _) => {
            if let Some(term) = node.contract.post.get(*j) {
                collect_term_roots(node, term, &mut roots);
            }
            if roots.is_empty() {
                for o in &node.outputs {
                    if let Operand::Op(i, _) = o {
                        roots.insert(*i);
                    }
                }
            }
        }
        ObligationKind::FoldBase { op_idx }
        | ObligationKind::FoldStep { op_idx }
        | ObligationKind::FoldPre { op_idx, .. } => {
            roots.insert(*op_idx);
            if let Some(op) = node.ops.get(*op_idx as usize)
                && let Some(Operand::Invariant(v)) = op.operands.last()
                && let Some(terms) = node.contract.invariants.get(*v as usize)
            {
                for t in terms {
                    collect_term_ports(t, &mut roots);
                }
            }
        }
        _ => {
            if let Some(op_idx) = kind.op_index() {
                roots.insert(op_idx);
            }
        }
    }
    roots
}

fn collect_term_roots(node: &Node, t: &Term, out: &mut BTreeSet<u32>) {
    match t {
        Term::Result(j) => {
            if let Some(Operand::Op(i, _)) = node.outputs.get(*j as usize) {
                out.insert(*i);
            }
        }
        Term::Port(i, _) => {
            out.insert(*i);
        }
        _ => {}
    }
    for c in vg_core::canon::children(t) {
        collect_term_roots(node, c, out);
    }
}

fn collect_term_ports(t: &Term, out: &mut BTreeSet<u32>) {
    if let Term::Port(i, _) = t {
        out.insert(*i);
    }
    for c in vg_core::canon::children(t) {
        collect_term_ports(c, out);
    }
}

/// Computes the dynamic backward cone over the dataflow graph from `roots`.
/// For `SELECT(c, a, b)`, only follows `c` and the branch actually selected in `trace_fail`.
fn dynamic_backward_cone(node: &Node, roots: &BTreeSet<u32>, trace_fail: &Trace) -> Vec<u32> {
    let mut visited = BTreeSet::new();
    let mut stack: Vec<u32> = roots.iter().copied().collect();

    while let Some(op_idx) = stack.pop() {
        if !visited.insert(op_idx) {
            continue;
        }
        let Some(op) = node.ops.get(op_idx as usize) else {
            continue;
        };
        let info = opcode::by_code(op.opcode);
        if info.is_some_and(|x| x.name == "SELECT") && op.operands.len() == 3 {
            if let Operand::Op(ci, _) = op.operands[0] {
                stack.push(ci);
            }
            let cond_val = match op.operands[0] {
                Operand::Op(ci, cs) => trace_fail
                    .op_values
                    .get(ci as usize)
                    .and_then(|v| v.as_ref())
                    .and_then(|rs| rs.get(cs as usize))
                    .and_then(|v| match v {
                        Value::Bool(b) => Some(*b),
                        _ => None,
                    }),
                Operand::Const(c) => node
                    .consts
                    .get(c as usize)
                    .and_then(|kc| Value::from_bytes(&kc.bytes, &kc.ty))
                    .and_then(|(v, _)| match v {
                        Value::Bool(b) => Some(b),
                        _ => None,
                    }),
                _ => None,
            };
            match cond_val {
                Some(true) => {
                    if let Operand::Op(ti, _) = op.operands[1] {
                        stack.push(ti);
                    }
                }
                Some(false) => {
                    if let Operand::Op(fi, _) = op.operands[2] {
                        stack.push(fi);
                    }
                }
                None => {
                    for o in &op.operands[1..] {
                        if let Operand::Op(dep, _) = o {
                            stack.push(*dep);
                        }
                    }
                }
            }
            continue;
        }

        for o in &op.operands {
            if let Operand::Op(dep, _) = o {
                stack.push(*dep);
            }
        }
    }

    visited.into_iter().collect()
}

/// Finds an input vector that satisfies `PRE` and does not violate `kind`:
/// first asks Z3 (via `SmtObligationContext`), then falls back to deterministic seeded search.
fn find_passing_inputs(
    node: &Node,
    kind: &ObligationKind,
    inputs_fail: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Option<Vec<Value>> {
    let ctx = SmtObligationContext::build(node, kind, callee, false);
    if let Some(vals) = ctx.find_passing_inputs(node, kind, inputs_fail, callee, 60_000) {
        return Some(vals);
    }
    seeded_passing_search(node, kind, inputs_fail, callee)
}

fn seeded_passing_search(
    node: &Node,
    kind: &ObligationKind,
    inputs_fail: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Option<Vec<Value>> {
    let mut cands_per_param: Vec<Vec<Value>> = Vec::with_capacity(node.input_count());
    for (i, cur) in inputs_fail.iter().enumerate().take(node.input_count()) {
        let ty = node.input_type(i as u32)?;
        cands_per_param.push(candidate_values_for_type(ty, cur));
    }

    let passes = |vals: &[Value]| -> bool {
        if !pre_and_learnable_hold(node, vals, callee) {
            return false;
        }
        let tr = trace_eval(node, vals, callee);
        let chk = check_violation(node, kind, vals, &tr, callee);
        chk.pre_holds && !chk.violated
    };

    // Single-input mutations first.
    for i in 0..node.input_count() {
        let mut trial = inputs_fail.to_vec();
        for v in &cands_per_param[i] {
            trial[i] = v.clone();
            if passes(&trial) {
                return Some(trial);
            }
        }
    }

    // All-zero / base combination and simple pairs.
    let base: Vec<Value> = cands_per_param.iter().map(|cs| cs[0].clone()).collect();
    if passes(&base) {
        return Some(base);
    }

    for i in 0..node.input_count().min(4) {
        let mut trial = base.clone();
        for v in cands_per_param[i].iter().take(6) {
            trial[i] = v.clone();
            if passes(&trial) {
                return Some(trial);
            }
        }
    }

    None
}

fn candidate_values_for_type(ty: &Type, cur: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    match ty {
        Type::Bool => {
            if let Value::Bool(b) = cur {
                out.push(Value::Bool(!b));
                out.push(Value::Bool(*b));
            } else {
                out.push(Value::Bool(true));
                out.push(Value::Bool(false));
            }
        }
        Type::UInt(_) => {
            for v in [0i128, 1, 2, 4, 10] {
                if vg_core::value::int_fits(v, ty) {
                    out.push(Value::Int(v));
                }
            }
            if let Value::Int(x) = cur {
                for cand in [x / 2, x.saturating_sub(1), x.saturating_add(1)] {
                    if vg_core::value::int_fits(cand, ty) {
                        out.push(Value::Int(cand));
                    }
                }
            }
        }
        Type::SInt(_) => {
            for v in [0i128, 1, -1, 2, -2, 10] {
                if vg_core::value::int_fits(v, ty) {
                    out.push(Value::Int(v));
                }
            }
            if let Value::Int(x) = cur {
                for cand in [x / 2, -x, x.saturating_sub(1), x.saturating_add(1)] {
                    if vg_core::value::int_fits(cand, ty) {
                        out.push(Value::Int(cand));
                    }
                }
            }
        }
        t if t.is_float() => {
            for s in ["0.0", "1.0", "0.5", "-1.0", "2.0"] {
                let (neg, body) = s.strip_prefix('-').map_or((false, s), |b| (true, b));
                if let Some(bits) = float::parse(body, neg, t) {
                    out.push(Value::Float(bits));
                }
            }
        }
        Type::Array(elem, n) => {
            let elem_cands = candidate_values_for_type(elem, &Value::Int(0));
            for e in elem_cands.into_iter().take(4) {
                out.push(Value::Seq(vec![e; *n as usize]));
            }
        }
        Type::Slice(elem) => {
            let elem_cands = candidate_values_for_type(elem, &Value::Int(0));
            if let Some(e) = elem_cands.first() {
                out.push(Value::Seq(vec![e.clone()]));
                out.push(Value::Seq(vec![e.clone(); 4]));
            }
            out.push(Value::Seq(Vec::new()));
        }
        _ => out.push(cur.clone()),
    }
    out
}

/// Prunes `cone` by counterfactual replacement using `trace_pass`:
/// 1. Keep every op `op` in `diff_ops` whose individual replacement with `trace_pass[op]` makes
///    the violation disappear.
/// 2. Always retain trapping root ops (since they trapped before producing a value).
/// 3. If no single op makes the violation disappear on its own, run Zeller's `ddmin` (2002)
///    to find a 1-minimal subset of `diff_ops` whose simultaneous replacement fixes the violation.
#[allow(clippy::too_many_arguments)]
fn prune_with_ddmin(
    node: &Node,
    kind: &ObligationKind,
    roots: &BTreeSet<u32>,
    cone: &[u32],
    inputs_fail: &[Value],
    trace_fail: &Trace,
    trace_pass: &Trace,
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Vec<u32> {
    let mut diff_ops = Vec::new();
    for &op_idx in cone {
        if let (Some(vf), Some(vp)) = (
            trace_fail
                .op_values
                .get(op_idx as usize)
                .and_then(|v| v.as_ref()),
            trace_pass
                .op_values
                .get(op_idx as usize)
                .and_then(|v| v.as_ref()),
        ) && vf != vp
        {
            diff_ops.push(op_idx);
        }
    }

    if diff_ops.is_empty() {
        return cone.to_vec();
    }

    let fixes_violation = |subset: &[u32]| -> bool {
        let mut overrides = BTreeMap::new();
        for &op_idx in subset {
            if let Some(vp) = trace_pass
                .op_values
                .get(op_idx as usize)
                .and_then(|v| v.as_ref())
            {
                overrides.insert(op_idx, vp.clone());
            }
        }
        let tr = trace_eval_with_overrides(node, inputs_fail, callee, &overrides);
        let chk = check_violation(node, kind, inputs_fail, &tr, callee);
        chk.pre_holds && !chk.violated
    };

    let mut kept = BTreeSet::new();
    for &op_idx in &diff_ops {
        if fixes_violation(&[op_idx]) {
            kept.insert(op_idx);
        }
    }

    // Include any root op that trapped (had no value in trace_fail).
    for &r in roots {
        if trace_fail
            .op_values
            .get(r as usize)
            .and_then(|v| v.as_ref())
            .is_none()
        {
            kept.insert(r);
        }
    }

    // For POST obligations, ensure the output root op is kept if `kept` is non-empty and the
    // root op's value differed (matches docs/15 soft_knee keeping %12 FCLAMP).
    if matches!(kind, ObligationKind::Post(_)) && !kept.is_empty() {
        for &r in roots {
            if diff_ops.contains(&r) {
                kept.insert(r);
            }
        }
    }

    if !kept.is_empty() {
        return kept.into_iter().collect();
    }

    // No single op fixed it alone: apply Zeller's ddmin on `diff_ops`.
    if fixes_violation(&diff_ops) {
        let min_set = ddmin(&diff_ops, &fixes_violation);
        for op_idx in min_set {
            kept.insert(op_idx);
        }
        for &r in roots {
            if trace_fail
                .op_values
                .get(r as usize)
                .and_then(|v| v.as_ref())
                .is_none()
                || diff_ops.contains(&r)
            {
                kept.insert(r);
            }
        }
        return kept.into_iter().collect();
    }

    cone.to_vec()
}

/// Zeller (2002) `ddmin`: finds a 1-minimal subset `S` of `elems` such that `test(S)` is true.
pub fn ddmin(elems: &[u32], test: &dyn Fn(&[u32]) -> bool) -> Vec<u32> {
    if elems.len() <= 1 {
        return elems.to_vec();
    }
    let mut cur = elems.to_vec();
    let mut n = 2usize;

    loop {
        if cur.len() <= 1 {
            break;
        }
        let n_clamped = n.min(cur.len());
        let chunk_size = cur.len().div_ceil(n_clamped);
        let chunks: Vec<&[u32]> = cur.chunks(chunk_size).collect();

        // 1. Reduce to a single subset.
        let mut next_cur: Option<Vec<u32>> = None;
        for c in &chunks {
            if c.len() < cur.len() && test(c) {
                next_cur = Some(c.to_vec());
                break;
            }
        }
        if let Some(nc) = next_cur {
            cur = nc;
            n = 2;
            continue;
        }

        // 2. Reduce to a complement.
        let mut next_comp: Option<Vec<u32>> = None;
        for (idx, _) in chunks.iter().enumerate() {
            let comp: Vec<u32> = chunks
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != idx)
                .flat_map(|(_, c)| c.iter().copied())
                .collect();
            if !comp.is_empty() && comp.len() < cur.len() && test(&comp) {
                next_comp = Some(comp);
                break;
            }
        }
        if let Some(nc) = next_comp {
            cur = nc;
            n = (n_clamped - 1).max(2);
            continue;
        }

        if n_clamped >= cur.len() {
            break;
        }
        n = (n_clamped * 2).min(cur.len());
    }

    cur
}

/// Collapses value-passthrough ops (like FMIN/FMA/SELECT propagating a NaN unchanged) when a slice
/// exceeds 5 ops, keeping roots, value-transforming ops, combinators, and direct anomaly inputs.
fn collapse_passthroughs(
    node: &Node,
    roots: &BTreeSet<u32>,
    slice_ops: &[u32],
    trace_fail: &Trace,
) -> Vec<u32> {
    let consts = crate::trace::decode_consts(node);
    let empty_inputs: Vec<Value> = Vec::new();
    let read_val = |o: Operand| -> Option<Value> {
        match o {
            Operand::Op(i, s) => trace_fail
                .op_values
                .get(i as usize)
                .and_then(|v| v.as_ref())
                .and_then(|rs| rs.get(s as usize))
                .cloned(),
            Operand::Const(c) => consts.get(c as usize).and_then(|v| v.clone()),
            _ => None,
        }
    };
    let _ = &empty_inputs;

    let is_passthrough = |op_idx: u32| -> bool {
        let Some(op) = node.ops.get(op_idx as usize) else {
            return false;
        };
        let Some(res) = trace_fail
            .op_values
            .get(op_idx as usize)
            .and_then(|v| v.as_ref())
            .and_then(|rs| rs.first())
        else {
            return false;
        };
        op.operands
            .iter()
            .any(|o| read_val(*o).as_ref() == Some(res))
    };

    let is_float_nan_or_inf = |v: &Value, ty: &Type| -> (bool, bool) {
        if let Value::Float(bits) = v
            && ty.is_float()
        {
            (float::is_nan(ty, *bits), float::is_inf(ty, *bits))
        } else {
            (false, false)
        }
    };

    // Identify anomaly-creating ops (e.g. inf/inf -> NaN, finite -> inf).
    let mut keep = BTreeSet::new();
    let mut anomaly_creators = Vec::new();
    for &op_idx in slice_ops {
        let Some(op) = node.ops.get(op_idx as usize) else {
            continue;
        };
        let info = opcode::by_code(op.opcode);
        let is_combinator = info.is_some_and(|x| {
            matches!(
                x.name,
                "CALL_HASH" | "SIMD_MAP" | "FOLD_BOUNDED" | "GENERATE" | "ASSERT_GUARD"
            )
        });
        if roots.contains(&op_idx) || is_combinator || !is_passthrough(op_idx) {
            keep.insert(op_idx);
            if let Some(res) = trace_fail
                .op_values
                .get(op_idx as usize)
                .and_then(|v| v.as_ref())
                .and_then(|rs| rs.first())
                && let Some(rty) = op.results.first()
            {
                let (res_nan, res_inf) = is_float_nan_or_inf(res, rty);
                let any_arg_nan = op.operands.iter().any(|o| {
                    read_val(*o)
                        .zip(node.operand_type(*o))
                        .is_some_and(|(v, t)| is_float_nan_or_inf(&v, t).0)
                });
                let any_arg_inf = op.operands.iter().any(|o| {
                    read_val(*o)
                        .zip(node.operand_type(*o))
                        .is_some_and(|(v, t)| is_float_nan_or_inf(&v, t).1)
                });
                if (res_nan && !any_arg_nan) || (res_inf && !any_arg_inf) {
                    anomaly_creators.push(op_idx);
                }
            }
        }
    }

    // Also keep direct sensitive operands of anomaly creators when those operands are themselves
    // non-finite (e.g. %5 = FADD(1.0, %4) = inf feeding %6 = FDIV(%4, %5) = nan).
    let slice_set: BTreeSet<u32> = slice_ops.iter().copied().collect();
    for creator in anomaly_creators {
        if let Some(op) = node.ops.get(creator as usize) {
            for o in &op.operands {
                if let Operand::Op(dep, s) = o
                    && slice_set.contains(dep)
                    && let Some(val) = read_val(*o)
                    && let Some(ty) = node
                        .ops
                        .get(*dep as usize)
                        .and_then(|d| d.results.get(*s as usize))
                {
                    let (nan, inf) = is_float_nan_or_inf(&val, ty);
                    if nan || inf {
                        keep.insert(*dep);
                    }
                }
            }
        }
    }

    if keep.is_empty() {
        slice_ops.to_vec()
    } else {
        keep.into_iter().collect()
    }
}

pub fn format_slice_entries(
    node: &Node,
    names: &Names,
    slice_ops: &[u32],
    trace_fail: &Trace,
) -> Vec<SliceEntry> {
    let mut entries = Vec::with_capacity(slice_ops.len());
    for &op_idx in slice_ops {
        let Some(op) = node.ops.get(op_idx as usize) else {
            continue;
        };
        let port = names.port_name(op_idx, 0);
        let op_str = names.format_op(node, op_idx);
        let val_json = if let Some(res_vals) = trace_fail
            .op_values
            .get(op_idx as usize)
            .and_then(|v| v.as_ref())
        {
            let non_eff: Vec<(&Value, &Type)> = res_vals
                .iter()
                .zip(&op.results)
                .filter(|(_, t)| **t != Type::Eff)
                .collect();
            let empty = BTreeSet::new();
            let reads = trace_fail
                .accessed_indices
                .get(&Operand::Op(op_idx, 0))
                .unwrap_or(&empty);
            match non_eff.as_slice() {
                [] => J::Null,
                [(v, t)] => value_to_report_json(v, t, reads),
                many => J::Array(
                    many.iter()
                        .map(|(v, t)| value_to_report_json(v, t, &BTreeSet::new()))
                        .collect(),
                ),
            }
        } else {
            J::String("trap".to_string())
        };
        entries.push(SliceEntry {
            port,
            op: op_str,
            value: val_json,
        });
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ddmin_finds_minimal_subset() {
        let elems = vec![0, 1, 2, 3, 4, 5, 6, 7];
        // Failure disappears only when both 2 and 5 are replaced.
        let min = ddmin(&elems, &|s| s.contains(&2) && s.contains(&5));
        assert_eq!(min, vec![2, 5]);
    }
}
