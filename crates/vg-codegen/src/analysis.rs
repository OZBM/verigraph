//! Proof erasure, residual guard discovery, loop fusion, in-place unique updates, and scratch placement.

use std::collections::{BTreeMap, BTreeSet};

use vg_core::diag::Diagnostic;
use vg_core::ir::{Hash, Node, Op, Operand};
use vg_core::opcode;
use vg_core::types::Type;

use crate::Options;
use crate::layout::{CallGraph, align_up, is_erased, is_scalar_abi, size_of};

pub const STACK_INTERMEDIATE_THRESHOLD: u64 = 4096;

pub const OP_SIMD_MAP: u16 = 0x0100;
pub const OP_FOLD_BOUNDED: u16 = 0x0101;
pub const OP_ASSERT_GUARD: u16 = 0x0105;
pub const OP_GENERATE: u16 = 0x0107;
pub const OP_CALL_HASH: u16 = 0x0300;
pub const OP_INDEX: u16 = 0x0060;
pub const OP_LEN: u16 = 0x0062;
pub const OP_SLICE: u16 = 0x0063;
pub const OP_CONCAT: u16 = 0x0064;
pub const OP_UPDATE: u16 = 0x0061;
pub const OP_AS_BYTES: u16 = 0x0007;
pub const OP_SELECT: u16 = 0x0005;
pub const OP_TUPLE: u16 = 0x0003;
pub const OP_PROJ: u16 = 0x0004;
pub const OP_CAST: u16 = 0x0006;
pub const OP_OK: u16 = 0x0008;
pub const OP_ERR: u16 = 0x0009;
pub const OP_IS_OK: u16 = 0x000A;
pub const OP_GET_OK: u16 = 0x000B;
pub const OP_GET_ERR: u16 = 0x000C;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardInfo {
    pub guard_id: String,
    pub obligation: String,
    pub node_hash: Hash,
    pub node_prefix: String,
    pub op_index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TupleFieldAction {
    /// Scalar member stored into `&in0.j` at `tup_idx`.
    StoreScalar,
    /// Aggregate member already mutated/kept in `&in0.j` (`PROJ(in0, j)`, `inplace_proj_updates`,
    /// or `inplace_guard_fields`).
    AlreadyInPlace,
    /// Aggregate member computed into a fresh non-aliasing buffer; copied to `&in0.j` at `tup_idx`.
    CopyFromFresh,
}

/// Verified plan allowing a `FOLD_BOUNDED` kernel to mutate `acc_buf` (`%in0`) directly in place
/// (`@vgi_<prefix>_inplace`), avoiding per-step `next_buf` and aggregate copies (`P4.2`, `docs/06`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InplaceFoldPlan {
    pub tup_idx: usize,
    pub field_actions: Vec<TupleFieldAction>,
    /// `op_idx` of `UPDATE` mutating `&in0.<field>` directly in place -> `field_idx`.
    pub inplace_proj_updates: BTreeMap<usize, usize>,
    /// `op_idx` of `ASSERT_GUARD` writing `&in0.<field>` -> `(field_idx, then_is_noop, else_is_noop)`.
    pub inplace_guard_fields: BTreeMap<usize, (usize, bool, bool)>,
}

/// Pure trap-free scalar ops (`excl_ops`) whose results feed only one arm of a set of scalar
/// `SELECT` ops (`select_ops`) sharing one condition, lowered as a single branch diamond.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LazySelectGroup {
    pub common_ops: Vec<usize>,
    pub excl_ops: Vec<usize>,
    pub select_ops: Vec<usize>,
    pub branch_on_true: bool,
}

#[derive(Clone, Debug)]
pub struct NodeAnalysis {
    /// Number of uses of each `(op_idx, slot)` across `node.ops` and `node.outputs`.
    pub use_counts: Vec<Vec<usize>>,
    /// For each op, true if `op` returns a `Slice` that owns fresh arena memory AND has 1 use.
    pub unique_slice_ops: Vec<bool>,
    /// True if `UPDATE` at op `i` updates a unique slice in place (0 arena allocation).
    pub inplace_update: Vec<bool>,
    /// True if `UPDATE` at op `i` updates a unique local `Array` buffer in place (0 `memcpy`).
    pub inplace_array_update: Vec<bool>,
    /// Map from `fold_op_idx` -> `gen_op_idx` when `GENERATE` (`Array`) fuses into `FOLD_BOUNDED`.
    pub fused_generate: BTreeMap<usize, usize>,
    /// Ops skipped in the main emission loop (`gen_op_idx` fused into `FOLD_BOUNDED`, or ops
    /// emitted inside a `LazySelectGroup`).
    pub deferred_ops: BTreeSet<usize>,
    /// Map from `sel_start` -> `LazySelectGroup`.
    pub lazy_select_groups: BTreeMap<usize, LazySelectGroup>,
    /// Map from `(op_idx, slot)` -> index into `out_ptrs` when an op writes directly to `%out{j}`.
    pub direct_return_outs: BTreeMap<(usize, usize), usize>,
    /// Present when this node qualifies for `@vgi_<prefix>_inplace` when used as a `FOLD_BOUNDED` kernel.
    pub inplace_fold: Option<InplaceFoldPlan>,
    /// For each `(op_idx, slot)`, true if its storage should live in the arena scratch block.
    pub scratch_ops: Vec<Vec<bool>>,
    /// Byte offset inside the local scratch block for each `(op_idx, slot)` marked in `scratch_ops`.
    pub scratch_offsets: BTreeMap<(usize, usize), u64>,
    /// Total 16-byte-aligned local scratch bytes for this node.
    pub local_scratch_bytes: u64,
    /// Total worst-case scratch bytes across this node + static calls (dynamic loops add in formula).
    pub static_scratch_bytes: u64,
    /// Map from `op_idx` of a dynamic plain `ASSERT_GUARD` to its `GuardInfo`.
    pub guards: BTreeMap<usize, GuardInfo>,
}

pub fn is_const_bool(node: &Node, operand: Operand, want: bool) -> bool {
    if let Operand::Const(c) = operand
        && let Some(k) = node.consts.get(c as usize)
        && k.ty == Type::Bool
        && k.bytes.as_slice() == [u8::from(want)]
    {
        return true;
    }
    false
}

fn const_u64_val(node: &Node, operand: Operand) -> Option<u64> {
    let Operand::Const(c) = operand else {
        return None;
    };
    let k = node.consts.get(c as usize)?;
    if k.ty == Type::UInt(64) && k.bytes.len() >= 8 {
        Some(u64::from_le_bytes(k.bytes[..8].try_into().ok()?))
    } else {
        None
    }
}

fn const_u32_val(node: &Node, operand: Operand) -> Option<u32> {
    let Operand::Const(c) = operand else {
        return None;
    };
    let k = node.consts.get(c as usize)?;
    if k.ty == Type::UInt(32) && k.bytes.len() >= 4 {
        Some(u32::from_le_bytes(k.bytes[..4].try_into().ok()?))
    } else {
        None
    }
}

fn contains_slice(ty: &Type) -> bool {
    match ty {
        Type::Slice(_) => true,
        Type::Tuple(ms) => ms.iter().any(contains_slice),
        Type::Result(a, b) => contains_slice(a) || contains_slice(b),
        _ => false,
    }
}

fn produces_fresh_slice(node: &Node, op_idx: usize) -> bool {
    let op = &node.ops[op_idx];
    if op.results.len() != 1 || !matches!(op.results[0], Type::Slice(_)) {
        return false;
    }
    matches!(op.opcode, OP_SIMD_MAP | OP_GENERATE | OP_CONCAT | OP_UPDATE)
}

fn cast_is_total(src: &Type, dst: &Type) -> bool {
    match (src, dst) {
        (Type::Bool, Type::UInt(_) | Type::SInt(_)) => true,
        (Type::UInt(w1), Type::UInt(w2)) => w1 <= w2,
        (Type::UInt(w1), Type::SInt(w2)) => w1 < w2,
        (Type::SInt(w1), Type::SInt(w2)) => w1 <= w2,
        (Type::UInt(_) | Type::SInt(_), t) if t.is_float() => true,
        (s, d) if s.is_float() && d.is_float() => true,
        _ => false,
    }
}

/// True when `op` can never trap even when `runtime_checks` is enabled.
fn op_is_always_trap_free(node: &Node, op: &Op, trap_free_nodes: &BTreeMap<Hash, bool>) -> bool {
    match op.opcode {
        0x0002..=0x0005 | 0x0007..=0x000A | OP_LEN => true,
        OP_CAST => {
            if let (Some(src), Some(dst)) = (
                op.operands.first().and_then(|o| node.operand_type(*o)),
                op.results.first(),
            ) {
                cast_is_total(src, dst)
            } else {
                false
            }
        }
        0x0010..=0x0012 | 0x0015 => op.mode != 0,
        0x0013 | 0x0014 => false,
        0x0030..=0x0033 | 0x0036 | 0x0037 => true,
        0x0034 | 0x0035 => op.mode != 0,
        0x0040..=0x0048 | 0x004B => true,
        0x0049 | 0x004A => false,
        0x0050..=0x0058 => true,
        OP_INDEX | OP_UPDATE => {
            if let Some(Type::Array(_, n)) = op.operands.first().and_then(|o| node.operand_type(*o))
                && let Some(idx) = op.operands.get(1).and_then(|o| const_u64_val(node, *o))
            {
                idx < u64::from(*n)
            } else {
                false
            }
        }
        OP_SLICE => {
            if let Some(Type::Array(_, n)) = op.operands.first().and_then(|o| node.operand_type(*o))
                && let (Some(lo), Some(hi)) = (
                    op.operands.get(1).and_then(|o| const_u64_val(node, *o)),
                    op.operands.get(2).and_then(|o| const_u64_val(node, *o)),
                )
            {
                lo <= hi && hi <= u64::from(*n)
            } else {
                false
            }
        }
        OP_CONCAT => matches!(op.results.first(), Some(Type::Array(..))),
        OP_CALL_HASH | OP_SIMD_MAP | OP_FOLD_BOUNDED | OP_GENERATE => op
            .operands
            .first()
            .and_then(|o| match o {
                Operand::Callee(c) => node.calls.get(*c as usize),
                _ => None,
            })
            .and_then(|h| trap_free_nodes.get(h).copied())
            .unwrap_or(false),
        OP_ASSERT_GUARD => {
            let (Some(Operand::Callee(ct)), Some(Operand::Callee(ce))) =
                (op.operands.get(1).copied(), op.operands.last().copied())
            else {
                return false;
            };
            let t_ok = node
                .calls
                .get(ct as usize)
                .and_then(|h| trap_free_nodes.get(h).copied())
                .unwrap_or(false);
            let e_ok = node
                .calls
                .get(ce as usize)
                .and_then(|h| trap_free_nodes.get(h).copied())
                .unwrap_or(false);
            if op.mode == 0 {
                is_const_bool(node, op.operands[0], true) && t_ok
            } else {
                t_ok && e_ok
            }
        }
        _ => false,
    }
}

fn is_pure_passthrough_of_arg(bnode: &Node) -> Option<usize> {
    if !bnode.effects.is_pure()
        || !bnode.ops.is_empty()
        || !bnode.learnable.is_empty()
        || bnode.outputs.len() != 1
    {
        return None;
    }
    let Operand::Input(param_idx) = bnode.outputs[0] else {
        return None;
    };
    let non_eff_pos = bnode
        .params
        .iter()
        .take(param_idx as usize)
        .filter(|t| !is_erased(t))
        .count();
    if is_erased(bnode.params.get(param_idx as usize)?) {
        return None;
    }
    Some(non_eff_pos)
}

fn op_produces_fresh_buffer(op: &Op, inplace_array_update: &[bool], idx: usize) -> bool {
    match op.opcode {
        OP_TUPLE | OP_OK | OP_ERR | OP_SIMD_MAP | OP_GENERATE | OP_CONCAT | OP_CALL_HASH
        | OP_ASSERT_GUARD | OP_FOLD_BOUNDED => true,
        OP_UPDATE => !inplace_array_update[idx],
        _ => false,
    }
}

fn analyze_inplace_fold(
    node: &Node,
    graph: &CallGraph,
    use_counts: &[Vec<usize>],
    local_scratch_bytes: u64,
    inplace_array_update: &[bool],
    deferred_ops: &BTreeSet<usize>,
) -> Option<InplaceFoldPlan> {
    if !node.effects.is_pure() || node.results.len() != 1 || local_scratch_bytes != 0 {
        return None;
    }
    let acc_ty = node.params.first()?;
    if acc_ty != &node.results[0] || is_scalar_abi(acc_ty) || contains_slice(acc_ty) {
        return None;
    }
    let Type::Tuple(fields) = acc_ty else {
        return None;
    };
    if node.ops.is_empty() {
        return None;
    }
    let tup_idx = node.ops.len() - 1;
    let tup_op = &node.ops[tup_idx];
    if tup_op.opcode != OP_TUPLE
        || deferred_ops.contains(&tup_idx)
        || node.outputs.as_slice() != [Operand::Op(tup_idx as u32, 0)]
        || use_counts[tup_idx].as_slice() != [1]
        || tup_op.operands.len() != fields.len()
    {
        return None;
    }

    // Every reference to Input(0) must be `PROJ(Input(0), j)` at some op < tup_idx.
    let mut proj_field_of_op: BTreeMap<usize, usize> = BTreeMap::new();
    let mut projs_by_field: Vec<Vec<usize>> = vec![Vec::new(); fields.len()];
    for (i, op) in node.ops[..tup_idx].iter().enumerate() {
        for (arg_pos, &o) in op.operands.iter().enumerate() {
            if o == Operand::Input(0) {
                if op.opcode != OP_PROJ || arg_pos != 0 {
                    return None;
                }
                let j = const_u32_val(node, *op.operands.get(1)?)? as usize;
                if j >= fields.len() {
                    return None;
                }
                proj_field_of_op.insert(i, j);
                projs_by_field[j].push(i);
            }
        }
    }
    if tup_op.operands.contains(&Operand::Input(0)) {
        return None;
    }

    let mut field_actions = Vec::with_capacity(fields.len());
    let mut inplace_proj_updates = BTreeMap::new();
    let mut inplace_guard_fields = BTreeMap::new();

    for (j, fty) in fields.iter().enumerate() {
        let tup_arg = tup_op.operands[j];
        if is_scalar_abi(fty) {
            field_actions.push(TupleFieldAction::StoreScalar);
            continue;
        }
        let Operand::Op(src_i, src_s) = tup_arg else {
            return None;
        };
        let src_i = src_i as usize;
        let src_s = src_s as usize;
        if src_s != 0 {
            return None;
        }

        // Case 1: Unchanged field (`PROJ(Input(0), j)`).
        if proj_field_of_op.get(&src_i) == Some(&j) {
            field_actions.push(TupleFieldAction::AlreadyInPlace);
            continue;
        }

        let src_op = &node.ops[src_i];

        // Case 2: In-place `UPDATE(PROJ(Input(0), j), idx, scalar_val)` on `Array` field `j`.
        if src_op.opcode == OP_UPDATE
            && matches!(fty, Type::Array(..))
            && let Some(Operand::Op(p_i, 0)) = src_op.operands.first().copied()
            && proj_field_of_op.get(&(p_i as usize)) == Some(&j)
            && src_op
                .operands
                .get(2)
                .and_then(|v| node.operand_type(*v))
                .is_some_and(is_scalar_abi)
        {
            let upd_i = src_i;
            // Every use of any `PROJ(Input(0), j)` other than `upd_i.operands[0]` must be < upd_i
            // and produce only a scalar (already loaded into SSA before `upd_i` mutates `&in0.j`).
            let mut projs_safe = true;
            for &pi in &projs_by_field[j] {
                for (k, kop) in node.ops[..tup_idx].iter().enumerate() {
                    if k == upd_i {
                        // Make sure `pi` isn't also passed as another operand of `upd_i`
                        if kop.operands[1..].contains(&Operand::Op(pi as u32, 0)) {
                            projs_safe = false;
                        }
                        continue;
                    }
                    if kop.operands.contains(&Operand::Op(pi as u32, 0))
                        && !(k < upd_i
                            && kop.results.len() == 1
                            && is_scalar_abi(&kop.results[0])
                            && matches!(kop.opcode, OP_INDEX | OP_LEN))
                    {
                        projs_safe = false;
                    }
                }
            }
            // Any use of `(upd_i, 0)` in `(upd_i + 1)..tup_idx` must only read it (`INDEX`, `LEN`,
            // `CALL_HASH`, `ASSERT_GUARD`), never alias-escape or mutate it again.
            for kop in &node.ops[upd_i + 1..tup_idx] {
                if kop.operands.contains(&Operand::Op(upd_i as u32, 0))
                    && !matches!(
                        kop.opcode,
                        OP_INDEX | OP_LEN | OP_CALL_HASH | OP_ASSERT_GUARD
                    )
                {
                    projs_safe = false;
                }
            }
            if projs_safe {
                inplace_proj_updates.insert(upd_i, j);
                field_actions.push(TupleFieldAction::AlreadyInPlace);
                continue;
            }
        }

        // Case 3: Guarded conditional update `ASSERT_GUARD(cond, @then, args..., else=@else)` on field `j`.
        if src_op.opcode == OP_ASSERT_GUARD
            && src_op.results.len() == 1
            && use_counts[src_i].as_slice() == [1]
        {
            let ag_i = src_i;
            // No op after `ag_i` (before `tup_idx`) may read any `PROJ(Input(0), j)`.
            let no_late_proj_read = projs_by_field[j].iter().all(|&pi| {
                node.ops[ag_i + 1..tup_idx]
                    .iter()
                    .all(|kop| !kop.operands.contains(&Operand::Op(pi as u32, 0)))
            });
            if no_late_proj_read
                && let (Some(Operand::Callee(ct)), Some(Operand::Callee(ce))) = (
                    src_op.operands.get(1).copied(),
                    src_op.operands.last().copied(),
                )
                && let (Some(tnode), Some(enode)) = (
                    graph.by_hash.get(&node.calls[ct as usize]),
                    graph.by_hash.get(&node.calls[ce as usize]),
                )
            {
                let non_eff_args: Vec<Operand> = src_op.operands[2..src_op.operands.len() - 1]
                    .iter()
                    .copied()
                    .filter(|o| node.operand_type(*o).is_some_and(|t| !is_erased(t)))
                    .collect();
                let arg_is_field_j = |pos: usize| -> bool {
                    matches!(
                        non_eff_args.get(pos).copied(),
                        Some(Operand::Op(pi, 0)) if proj_field_of_op.get(&(pi as usize)) == Some(&j)
                    )
                };
                let then_noop = is_pure_passthrough_of_arg(tnode).is_some_and(arg_is_field_j);
                let else_noop = is_pure_passthrough_of_arg(enode).is_some_and(arg_is_field_j);
                inplace_guard_fields.insert(ag_i, (j, then_noop, else_noop));
                field_actions.push(TupleFieldAction::AlreadyInPlace);
                continue;
            }
        }

        // Case 4: Fresh buffer produced before `tup_idx` — safe to copy into `&in0.j` at `tup_idx`.
        if op_produces_fresh_buffer(src_op, inplace_array_update, src_i)
            && !inplace_proj_updates.contains_key(&src_i)
            && !inplace_guard_fields.contains_key(&src_i)
        {
            field_actions.push(TupleFieldAction::CopyFromFresh);
            continue;
        }

        return None;
    }

    Some(InplaceFoldPlan {
        tup_idx,
        field_actions,
        inplace_proj_updates,
        inplace_guard_fields,
    })
}

fn find_lazy_select_groups(
    node: &Node,
    opts: &Options,
    use_counts: &[Vec<usize>],
    guards: &BTreeMap<usize, GuardInfo>,
    trap_free_nodes: &BTreeMap<Hash, bool>,
    already_deferred: &BTreeSet<usize>,
) -> (BTreeMap<usize, LazySelectGroup>, BTreeSet<usize>) {
    let mut groups = BTreeMap::new();
    let mut deferred = BTreeSet::new();
    let n = node.ops.len();
    let mut i = 0;

    let can_sink_op = |idx: usize, deferred: &BTreeSet<usize>| -> bool {
        if already_deferred.contains(&idx) || deferred.contains(&idx) || guards.contains_key(&idx) {
            return false;
        }
        let op = &node.ops[idx];
        if op.results.len() != 1 || !is_scalar_abi(&op.results[0]) {
            return false;
        }
        if !matches!(op.opcode, OP_CAST | 0x0010..=0x0058) {
            return false;
        }
        !opts.runtime_checks || op_is_always_trap_free(node, op, trap_free_nodes)
    };

    while i < n {
        let op = &node.ops[i];
        if op.opcode != OP_SELECT
            || op.results.len() != 1
            || !is_scalar_abi(&op.results[0])
            || already_deferred.contains(&i)
            || deferred.contains(&i)
        {
            i += 1;
            continue;
        }
        let cond_op = op.operands[0];
        let min_start = match cond_op {
            Operand::Op(ci, _) => (ci as usize) + 1,
            _ => 0,
        };

        // Gather all scalar `SELECT` ops from `i` onward sharing `cond_op`, stopping before any
        // deferred/effect/guard op or any op that consumes an earlier `SELECT` in this candidate set.
        let mut select_ops = vec![i];
        let mut select_set = BTreeSet::from([i]);
        for cand in (i + 1)..n {
            if already_deferred.contains(&cand)
                || deferred.contains(&cand)
                || guards.contains_key(&cand)
                || node.ops[cand].opcode >= 0x0400
            {
                break;
            }
            let uses_prev_sel = node.ops[cand]
                .operands
                .iter()
                .any(|o| matches!(*o, Operand::Op(p, _) if select_set.contains(&(p as usize))));
            if uses_prev_sel {
                break;
            }
            let cop = &node.ops[cand];
            if cop.opcode == OP_SELECT
                && cop.results.len() == 1
                && is_scalar_abi(&cop.results[0])
                && cop.operands[0] == cond_op
            {
                select_ops.push(cand);
                select_set.insert(cand);
            }
        }
        let s_last = *select_ops.last().expect("non-empty");
        let span_start = (min_start..=i)
            .rev()
            .take_while(|&k| {
                !already_deferred.contains(&k)
                    && !deferred.contains(&k)
                    && !guards.contains_key(&k)
                    && node.ops[k].opcode < 0x0400
            })
            .last()
            .unwrap_or(i);

        let mut chosen: Option<(Vec<usize>, bool)> = None;
        for (arm_idx, branch_on_true) in [(1usize, true), (2usize, false)] {
            let other_arm = if arm_idx == 1 { 2 } else { 1 };
            let mut excl_set = BTreeSet::new();
            for k in (span_start..=s_last).rev() {
                if select_set.contains(&k) || !can_sink_op(k, &deferred) {
                    continue;
                }
                let total_uses = use_counts[k][0];
                if total_uses == 0 {
                    continue;
                }
                let mut valid_uses = 0usize;
                let mut bad_use = false;
                for u in (k + 1)..=s_last {
                    if excl_set.contains(&u) {
                        for &o in &node.ops[u].operands {
                            if o == Operand::Op(k as u32, 0) {
                                valid_uses += 1;
                            }
                        }
                    } else if select_set.contains(&u) {
                        let sop = &node.ops[u];
                        if sop.operands[0] == Operand::Op(k as u32, 0)
                            || sop.operands[other_arm] == Operand::Op(k as u32, 0)
                        {
                            bad_use = true;
                        }
                        if sop.operands[arm_idx] == Operand::Op(k as u32, 0) {
                            valid_uses += 1;
                        }
                    }
                }
                if !bad_use && valid_uses == total_uses {
                    excl_set.insert(k);
                }
            }
            if excl_set.len() >= 4 {
                chosen = Some((excl_set.into_iter().collect(), branch_on_true));
                break;
            }
        }

        if let Some((excl_ops, branch_on_true)) = chosen {
            let excl_set: BTreeSet<usize> = excl_ops.iter().copied().collect();
            let first_idx = excl_ops[0].min(select_ops[0]);
            let common_ops: Vec<usize> = (first_idx..=s_last)
                .filter(|k| !excl_set.contains(k) && !select_set.contains(k))
                .collect();
            let common_valid = common_ops.iter().all(|&c| {
                !already_deferred.contains(&c)
                    && !deferred.contains(&c)
                    && node.ops[c].operands.iter().all(|o| match *o {
                        Operand::Op(p, _) => {
                            let p = p as usize;
                            !excl_set.contains(&p) && !select_set.contains(&p)
                        }
                        _ => true,
                    })
            });
            if common_valid {
                for k in (first_idx + 1)..=s_last {
                    deferred.insert(k);
                }
                groups.insert(
                    first_idx,
                    LazySelectGroup {
                        common_ops,
                        excl_ops,
                        select_ops,
                        branch_on_true,
                    },
                );
                i = s_last + 1;
                continue;
            }
        }
        i += 1;
    }

    (groups, deferred)
}

pub fn analyze_graph(
    graph: &CallGraph,
    opts: &Options,
) -> Result<(BTreeMap<Hash, NodeAnalysis>, Vec<GuardInfo>), Diagnostic> {
    let target = opts.target;
    let mut analyses: BTreeMap<Hash, NodeAnalysis> = BTreeMap::new();
    let mut trap_free_nodes: BTreeMap<Hash, bool> = BTreeMap::new();
    let mut all_guards: Vec<GuardInfo> = Vec::new();
    let mut next_guard_num = 0u32;

    for (h, node) in &graph.order {
        let node_tf = node
            .ops
            .iter()
            .all(|op| op_is_always_trap_free(node, op, &trap_free_nodes));
        trap_free_nodes.insert(*h, node_tf);

        let prefix = &graph.prefixes[h];
        let mut use_counts: Vec<Vec<usize>> = node
            .ops
            .iter()
            .map(|op| vec![0usize; op.results.len()])
            .collect();

        for op in &node.ops {
            for o in &op.operands {
                if let Operand::Op(i, s) = *o
                    && let Some(row) = use_counts.get_mut(i as usize)
                    && let Some(slot) = row.get_mut(s as usize)
                {
                    *slot += 1;
                }
            }
        }
        for o in &node.outputs {
            if let Operand::Op(i, s) = *o
                && let Some(row) = use_counts.get_mut(i as usize)
                && let Some(slot) = row.get_mut(s as usize)
            {
                *slot += 1;
            }
        }

        let unique_slice_ops: Vec<bool> = (0..node.ops.len())
            .map(|i| {
                produces_fresh_slice(node, i) && use_counts[i].first().copied().unwrap_or(0) == 1
            })
            .collect();

        let mut inplace_update = vec![false; node.ops.len()];
        for (i, op) in node.ops.iter().enumerate() {
            if op.opcode == OP_UPDATE
                && matches!(op.results.first(), Some(Type::Slice(_)))
                && let Some(Operand::Op(src_i, 0)) = op.operands.first().copied()
                && unique_slice_ops[src_i as usize]
            {
                inplace_update[i] = true;
            }
        }

        // Fuse single-use fixed-size `GENERATE` into `FOLD_BOUNDED` when trap order cannot change.
        let mut fused_generate: BTreeMap<usize, usize> = BTreeMap::new();
        let mut deferred_ops: BTreeSet<usize> = BTreeSet::new();
        for (fold_i, op) in node.ops.iter().enumerate() {
            if op.opcode == OP_FOLD_BOUNDED
                && let Some(Operand::Op(gen_i, 0)) = op.operands.get(1).copied()
            {
                let gen_i = gen_i as usize;
                let gen_op = &node.ops[gen_i];
                if gen_op.opcode == OP_GENERATE
                    && matches!(gen_op.results.first(), Some(Type::Array(..)))
                    && use_counts[gen_i].as_slice() == [1]
                    && let Some(Operand::Callee(gen_c)) = gen_op.operands.first().copied()
                {
                    let gen_kh = node.calls[gen_c as usize];
                    let gen_tf = !opts.runtime_checks
                        || trap_free_nodes.get(&gen_kh).copied().unwrap_or(false);
                    if gen_tf {
                        fused_generate.insert(fold_i, gen_i);
                        deferred_ops.insert(gen_i);
                    }
                }
            }
        }

        // Trace any Slice values reaching outputs backwards so backing local fixed-size allocations
        // (via SLICE or AS_BYTES) are placed in the arena scratch block rather than the stack.
        let mut escaping_vals: BTreeSet<(usize, usize)> = BTreeSet::new();
        let mut work: Vec<(usize, usize)> = Vec::new();
        for (res_idx, out) in node.outputs.iter().enumerate() {
            if let Some(ty) = node.results.get(res_idx)
                && contains_slice(ty)
                && let Operand::Op(i, s) = *out
                && escaping_vals.insert((i as usize, s as usize))
            {
                work.push((i as usize, s as usize));
            }
        }
        let mut scratch_ops: Vec<Vec<bool>> = node
            .ops
            .iter()
            .map(|op| vec![false; op.results.len()])
            .collect();

        while let Some((i, _s)) = work.pop() {
            let op = &node.ops[i];
            match op.opcode {
                OP_SLICE | OP_AS_BYTES => {
                    if let Some(src_op) = op.operands.first().copied() {
                        if let Operand::Op(src_i, src_s) = src_op {
                            let src_i = src_i as usize;
                            let src_s = src_s as usize;
                            let src_ty = &node.ops[src_i].results[src_s];
                            if matches!(src_ty, Type::Slice(_)) {
                                if escaping_vals.insert((src_i, src_s)) {
                                    work.push((src_i, src_s));
                                }
                            } else {
                                scratch_ops[src_i][src_s] = true;
                            }
                        } else if op.opcode == OP_AS_BYTES
                            && let Some(src_ty) = node.operand_type(src_op)
                            && is_scalar_abi(src_ty)
                        {
                            scratch_ops[i][0] = true;
                        }
                    }
                }
                OP_SELECT => {
                    for idx in [1usize, 2] {
                        if let Some(Operand::Op(src_i, src_s)) = op.operands.get(idx).copied()
                            && escaping_vals.insert((src_i as usize, src_s as usize))
                        {
                            work.push((src_i as usize, src_s as usize));
                        }
                    }
                }
                OP_TUPLE | OP_PROJ | OP_OK | OP_ERR | OP_GET_OK | OP_GET_ERR | OP_CALL_HASH => {
                    for o in &op.operands {
                        if let Operand::Op(src_i, src_s) = *o {
                            let src_i = src_i as usize;
                            let src_s = src_s as usize;
                            let src_ty = &node.ops[src_i].results[src_s];
                            if contains_slice(src_ty) {
                                if escaping_vals.insert((src_i, src_s)) {
                                    work.push((src_i, src_s));
                                }
                            } else if op.opcode == OP_CALL_HASH
                                && !is_scalar_abi(src_ty)
                                && !matches!(src_ty, Type::Eff)
                            {
                                scratch_ops[src_i][src_s] = true;
                            }
                        }
                    }
                }
                OP_ASSERT_GUARD => {
                    for o in op.operands.iter().skip(1) {
                        if let Operand::Op(src_i, src_s) = *o {
                            let src_i = src_i as usize;
                            let src_s = src_s as usize;
                            let src_ty = &node.ops[src_i].results[src_s];
                            if contains_slice(src_ty) {
                                if escaping_vals.insert((src_i, src_s)) {
                                    work.push((src_i, src_s));
                                }
                            } else if !is_scalar_abi(src_ty) && !matches!(src_ty, Type::Eff) {
                                scratch_ops[src_i][src_s] = true;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Destination-passing `RETURN`: in proven builds (`proof_facts && !runtime_checks`), `%out{j}`
        // carries `noalias` and the node cannot trap, so a single-use aggregate op returned directly
        // as output `j` writes straight into `%out{j}` without stack/scratch intermediate or `memcpy`.
        let mut direct_return_outs: BTreeMap<(usize, usize), usize> = BTreeMap::new();
        if opts.proof_facts && !opts.runtime_checks {
            let mut out_arg_idx = 0usize;
            for (ty, opnd) in node.results.iter().zip(&node.outputs) {
                if is_erased(ty) {
                    continue;
                }
                if !is_scalar_abi(ty)
                    && let Operand::Op(ret_i, ret_s) = *opnd
                {
                    let ret_i = ret_i as usize;
                    let ret_s = ret_s as usize;
                    let ret_op = &node.ops[ret_i];
                    if use_counts[ret_i].get(ret_s).copied() == Some(1)
                        && !deferred_ops.contains(&ret_i)
                        && matches!(
                            ret_op.opcode,
                            OP_TUPLE
                                | OP_OK
                                | OP_ERR
                                | OP_SIMD_MAP
                                | OP_GENERATE
                                | OP_CONCAT
                                | OP_CALL_HASH
                                | OP_ASSERT_GUARD
                                | OP_FOLD_BOUNDED
                        )
                    {
                        direct_return_outs.insert((ret_i, ret_s), out_arg_idx);
                        scratch_ops[ret_i][ret_s] = false;
                    }
                }
                out_arg_idx += 1;
            }
        }

        // Also place any fixed-size intermediate larger than 4 KiB in the scratch block (`docs/06`),
        // except fused `GENERATE` ops and `direct_return_outs` written straight into caller buffers.
        let mut scratch_offsets = BTreeMap::new();
        let mut local_scratch_bytes = 0u64;
        for (i, op) in node.ops.iter().enumerate() {
            if deferred_ops.contains(&i) {
                continue;
            }
            for (s, ty) in op.results.iter().enumerate() {
                if is_erased(ty) || direct_return_outs.contains_key(&(i, s)) {
                    continue;
                }
                let sz = if op.opcode == OP_AS_BYTES && scratch_ops[i][s] {
                    op.operands
                        .first()
                        .and_then(|o| node.operand_type(*o))
                        .map(|t| size_of(t, target))
                        .transpose()?
                        .unwrap_or(8)
                } else {
                    size_of(ty, target)?
                };
                if !is_scalar_abi(ty)
                    && !matches!(ty, Type::Slice(_))
                    && sz > STACK_INTERMEDIATE_THRESHOLD
                {
                    scratch_ops[i][s] = true;
                }
                if scratch_ops[i][s] {
                    let off = align_up(local_scratch_bytes, 16);
                    scratch_offsets.insert((i, s), off);
                    local_scratch_bytes = off + align_up(sz.max(1), 16);
                }
            }
        }

        // In-place updates on unique local stack `Array` buffers (`P4.2`).
        let mut owns_fresh_stack_array = vec![false; node.ops.len()];
        let mut inplace_array_update = vec![false; node.ops.len()];
        for (i, op) in node.ops.iter().enumerate() {
            if deferred_ops.contains(&i)
                || op.results.len() != 1
                || !matches!(op.results[0], Type::Array(..))
            {
                continue;
            }
            if op.opcode == OP_UPDATE
                && let Some(Operand::Op(src_i, 0)) = op.operands.first().copied()
                && owns_fresh_stack_array[src_i as usize]
                && use_counts[src_i as usize].as_slice() == [1]
                && !scratch_ops[i][0]
                && !direct_return_outs.contains_key(&(i, 0))
            {
                inplace_array_update[i] = true;
                owns_fresh_stack_array[i] = true;
                continue;
            }
            if !scratch_ops[i][0]
                && !direct_return_outs.contains_key(&(i, 0))
                && matches!(
                    op.opcode,
                    OP_UPDATE
                        | OP_SIMD_MAP
                        | OP_GENERATE
                        | OP_CONCAT
                        | OP_CALL_HASH
                        | OP_ASSERT_GUARD
                        | OP_FOLD_BOUNDED
                )
            {
                owns_fresh_stack_array[i] = true;
            }
        }

        // Compute static transitive scratch bytes across callees and count residual guards.
        let mut static_scratch_bytes = local_scratch_bytes;
        let mut guards = BTreeMap::new();
        for (i, op) in node.ops.iter().enumerate() {
            match op.opcode {
                OP_CALL_HASH => {
                    if let Some(Operand::Callee(c)) = op.operands.first().copied() {
                        let ch = node.calls[c as usize];
                        if let Some(ca) = analyses.get(&ch) {
                            static_scratch_bytes += ca.static_scratch_bytes;
                        }
                    }
                }
                OP_ASSERT_GUARD => {
                    if let (Some(Operand::Callee(ct)), Some(Operand::Callee(ce))) =
                        (op.operands.get(1).copied(), op.operands.last().copied())
                    {
                        let st = analyses
                            .get(&node.calls[ct as usize])
                            .map_or(0, |a| a.static_scratch_bytes);
                        let se = analyses
                            .get(&node.calls[ce as usize])
                            .map_or(0, |a| a.static_scratch_bytes);
                        if op.mode == 0 && is_const_bool(node, op.operands[0], true) {
                            static_scratch_bytes += st;
                        } else {
                            static_scratch_bytes += st.max(se);
                            if op.mode == 0 {
                                let guard_id = format!("g{next_guard_num}");
                                next_guard_num += 1;
                                let info = GuardInfo {
                                    guard_id,
                                    obligation: format!("@{prefix}/%{i}/ASSERT_GUARD"),
                                    node_hash: *h,
                                    node_prefix: prefix.clone(),
                                    op_index: i as u32,
                                };
                                guards.insert(i, info.clone());
                                all_guards.push(info);
                            }
                        }
                    }
                }
                OP_SIMD_MAP | OP_FOLD_BOUNDED | OP_GENERATE => {
                    if let Some(Operand::Callee(c)) = op.operands.first().copied()
                        && let Some(ca) = analyses.get(&node.calls[c as usize])
                        && ca.static_scratch_bytes > 0
                    {
                        let count_opt = if op.opcode == OP_GENERATE {
                            const_u64_val(node, *op.operands.get(1).unwrap_or(&Operand::Input(0)))
                        } else if let Some(&gen_i) = fused_generate.get(&i) {
                            const_u64_val(
                                node,
                                *node.ops[gen_i]
                                    .operands
                                    .get(1)
                                    .unwrap_or(&Operand::Input(0)),
                            )
                        } else {
                            match node.operand_type(op.operands[1]) {
                                Some(Type::Array(_, n)) => Some(u64::from(*n)),
                                Some(Type::Tensor(_, dims)) => {
                                    Some(dims.iter().map(|&d| u64::from(d)).product())
                                }
                                _ => None,
                            }
                        };
                        if let Some(cnt) = count_opt {
                            static_scratch_bytes += cnt.saturating_mul(ca.static_scratch_bytes);
                        }
                    }
                }
                _ => {
                    let _ = opcode::by_code(op.opcode);
                }
            }
        }

        let (lazy_select_groups, sel_deferred) = find_lazy_select_groups(
            node,
            opts,
            &use_counts,
            &guards,
            &trap_free_nodes,
            &deferred_ops,
        );
        deferred_ops.extend(sel_deferred);

        let inplace_fold = analyze_inplace_fold(
            node,
            graph,
            &use_counts,
            local_scratch_bytes,
            &inplace_array_update,
            &deferred_ops,
        );

        analyses.insert(
            *h,
            NodeAnalysis {
                use_counts,
                unique_slice_ops,
                inplace_update,
                inplace_array_update,
                fused_generate,
                deferred_ops,
                lazy_select_groups,
                direct_return_outs,
                inplace_fold,
                scratch_ops,
                scratch_offsets,
                local_scratch_bytes,
                static_scratch_bytes,
                guards,
            },
        );
    }

    Ok((analyses, all_guards))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Target;
    use std::collections::HashMap;
    use vg_core::asm_local;

    fn default_opts() -> Options {
        Options {
            target: Target::Aarch64Darwin,
            proof_facts: true,
            runtime_checks: false,
        }
    }

    #[test]
    fn unique_slice_update_and_guard_erasure_analysis() {
        let src = r#"
module m;
NODE @k_then {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {}
  BODY { RETURN(x); }
}
NODE @k_else {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {}
  BODY { RETURN(x); }
}
NODE @inc {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = ADD.wrap(x, 1:u32);
    RETURN(%0);
  }
}
NODE @test_guards_and_update {
  SIGNATURE: (s: Slice<u32>, cond: Bool) -> (Slice<u32>, u32, u32, u32);
  EFFECTS: [ARENA];
  CONTRACT {
    PRE: len(s) >= 2;
  }
  BODY {
    %0 = SIMD_MAP(@inc, s);
    %1 = UPDATE(%0, 0:u64, 42:u32);
    %2 = ASSERT_GUARD(true, @k_then, 1:u32, else=@k_else);
    %3 = ASSERT_GUARD(cond, @k_then, 2:u32, else=@k_else);
    %4 = ASSERT_GUARD.branch(cond, @k_then, 3:u32, else=@k_else);
    RETURN(%1, %2, %3, %4);
  }
}
"#;
        let nodes = asm_local(src).unwrap();
        let map: HashMap<Hash, Node> = nodes
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let root = &nodes.last().unwrap().node;
        let graph = crate::layout::collect_graph(root, &|h| map.get(h).cloned()).unwrap();
        let (analyses, guards) = analyze_graph(&graph, &default_opts()).unwrap();
        let root_an = &analyses[&graph.root_hash];

        let simd_idx = root
            .ops
            .iter()
            .position(|o| o.opcode == OP_SIMD_MAP)
            .unwrap();
        let upd_idx = root.ops.iter().position(|o| o.opcode == OP_UPDATE).unwrap();
        assert!(root_an.unique_slice_ops[simd_idx]);
        assert!(root_an.inplace_update[upd_idx]);

        assert_eq!(guards.len(), 1);
        assert_eq!(guards[0].guard_id, "g0");
    }

    #[test]
    fn large_array_and_escaping_slice_source_go_to_scratch() {
        let src = r#"
module m;
NODE @make_slice {
  SIGNATURE: (a: Array<u32, 2>, b: Array<u32, 2>) -> (Slice<u32>);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = CONCAT(a, b);
    %1 = SLICE(%0, 0:u64, 3:u64);
    RETURN(%1);
  }
}
"#;
        let nodes = asm_local(src).unwrap();
        let root = &nodes[0].node;
        let graph = crate::layout::collect_graph(root, &|_| None).unwrap();
        let (analyses, _) = analyze_graph(&graph, &default_opts()).unwrap();
        let an = &analyses[&graph.root_hash];
        let concat_idx = root.ops.iter().position(|o| o.opcode == OP_CONCAT).unwrap();
        assert!(an.scratch_ops[concat_idx][0]);
        assert!(an.local_scratch_bytes >= 16);
    }

    #[test]
    fn generate_fold_fusion_inplace_tuple_and_lazy_select_detected() {
        let src = r#"
module m;
NODE @iota {
  SIGNATURE: (k: u64) -> (u64);
  EFFECTS: [];
  CONTRACT {}
  BODY { RETURN(k); }
}
NODE @keep {
  SIGNATURE: (h: Array<u32, 8>, blk: Array<u8, 64>) -> (Array<u32, 8>);
  EFFECTS: [];
  CONTRACT {}
  BODY { RETURN(h); }
}
NODE @work {
  SIGNATURE: (h: Array<u32, 8>, blk: Array<u8, 64>) -> (Array<u32, 8>);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = UPDATE(h, 0:u64, 1:u32);
    %1 = UPDATE(%0, 1:u64, 2:u32);
    RETURN(%1);
  }
}
NODE @absorb {
  SIGNATURE: (st: Tuple<Array<u32, 8>, Array<u8, 64>, u64>, k: u64) -> (Tuple<Array<u32, 8>, Array<u8, 64>, u64>);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = PROJ(st, 0:u32);
    %1 = PROJ(st, 1:u32);
    %2 = PROJ(st, 2:u32);
    %3 = AND(%2, 63:u64);
    %4 = UPDATE(%1, %3, 7:u8);
    %5 = EQ(%3, 63:u64);
    %6 = ASSERT_GUARD.branch(%5, @work, %0, %4, else=@keep);
    %7 = ADD.wrap(%2, 1:u64);
    %8 = TUPLE(%6, %4, %7);
    RETURN(%8);
  }
}
NODE @swap_bad {
  SIGNATURE: (st: Tuple<Array<u8, 4>, Array<u8, 4>>, k: u64) -> (Tuple<Array<u8, 4>, Array<u8, 4>>);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = PROJ(st, 0:u32);
    %1 = PROJ(st, 1:u32);
    %2 = TUPLE(%1, %0);
    RETURN(%2);
  }
}
NODE @root {
  SIGNATURE: (st: Tuple<Array<u32, 8>, Array<u8, 64>, u64>, s: Tuple<Array<u8, 4>, Array<u8, 4>>) -> (Tuple<Array<u32, 8>, Array<u8, 64>, u64>, Tuple<Array<u8, 4>, Array<u8, 4>>);
  EFFECTS: [];
  CONTRACT {
    INVARIANT @i1(k, a): k <= 128;
    INVARIANT @i2(k, a): k <= 4;
  }
  BODY {
    %0 = GENERATE(@iota, 128:u64);
    %1 = FOLD_BOUNDED(@absorb, %0, init=st, invariant=@i1);
    %2 = GENERATE(@iota, 4:u64);
    %3 = FOLD_BOUNDED(@swap_bad, %2, init=s, invariant=@i2);
    RETURN(%1, %3);
  }
}
"#;
        let nodes = asm_local(src).unwrap();
        let by_label: HashMap<&str, Hash> = nodes
            .iter()
            .map(|a| (a.label.as_str(), a.hashes.node))
            .collect();
        let map: HashMap<Hash, Node> = nodes
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let root = &nodes.last().unwrap().node;
        let graph = crate::layout::collect_graph(root, &|h| map.get(h).cloned()).unwrap();
        let (analyses, _) = analyze_graph(&graph, &default_opts()).unwrap();

        let absorb_an = &analyses[&by_label["absorb"]];
        let plan = absorb_an
            .inplace_fold
            .as_ref()
            .expect("@absorb must get an inplace fold plan");
        assert_eq!(plan.inplace_proj_updates.len(), 1);
        assert_eq!(plan.inplace_guard_fields.len(), 1);
        let (_, then_noop, else_noop) = *plan.inplace_guard_fields.values().next().unwrap();
        assert!(!then_noop && else_noop);

        // Aggregate swap `TUPLE(st.1, st.0)` must NOT get an inplace plan (would clobber st.0).
        let swap_an = &analyses[&by_label["swap_bad"]];
        assert!(swap_an.inplace_fold.is_none());

        // Chained array UPDATE in `@work` updates in place.
        let work_an = &analyses[&by_label["work"]];
        assert!(work_an.inplace_array_update.iter().any(|&b| b));

        // Both GENERATEs in `@root` fuse into their FOLD_BOUNDEDs.
        let root_an = &analyses[&graph.root_hash];
        assert_eq!(root_an.fused_generate.len(), 2);
    }
}
