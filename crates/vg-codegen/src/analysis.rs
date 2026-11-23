//! Proof erasure, residual guard discovery, in-place unique slice updates, and scratch placement.

use std::collections::{BTreeMap, BTreeSet};

use vg_core::diag::Diagnostic;
use vg_core::ir::{Hash, Node, Operand};
use vg_core::opcode;
use vg_core::types::Type;

use crate::Target;
use crate::layout::{CallGraph, align_up, size_of};

pub const STACK_INTERMEDIATE_THRESHOLD: u64 = 4096;

pub const OP_SIMD_MAP: u16 = 0x0100;
pub const OP_FOLD_BOUNDED: u16 = 0x0101;
pub const OP_ASSERT_GUARD: u16 = 0x0105;
pub const OP_GENERATE: u16 = 0x0107;
pub const OP_CALL_HASH: u16 = 0x0300;
pub const OP_SLICE: u16 = 0x0063;
pub const OP_CONCAT: u16 = 0x0064;
pub const OP_UPDATE: u16 = 0x0061;
pub const OP_AS_BYTES: u16 = 0x0007;
pub const OP_SELECT: u16 = 0x0005;
pub const OP_TUPLE: u16 = 0x0003;
pub const OP_PROJ: u16 = 0x0004;
pub const OP_OK: u16 = 0x0008;
pub const OP_ERR: u16 = 0x0009;
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

#[derive(Clone, Debug)]
pub struct NodeAnalysis {
    /// Number of uses of each `(op_idx, slot)` across `node.ops` and `node.outputs`.
    pub use_counts: Vec<Vec<usize>>,
    /// For each op, true if `op` returns a `Slice` that owns fresh arena memory AND has 1 use.
    pub unique_slice_ops: Vec<bool>,
    /// True if `UPDATE` at op `i` updates a unique slice in place (0 arena allocation).
    pub inplace_update: Vec<bool>,
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

pub fn analyze_graph(
    graph: &CallGraph,
    target: Target,
) -> Result<(BTreeMap<Hash, NodeAnalysis>, Vec<GuardInfo>), Diagnostic> {
    let mut analyses: BTreeMap<Hash, NodeAnalysis> = BTreeMap::new();
    let mut all_guards: Vec<GuardInfo> = Vec::new();
    let mut next_guard_num = 0u32;

    for (h, node) in &graph.order {
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
                    if let Some(Operand::Op(src_i, src_s)) = op.operands.first().copied() {
                        let src_i = src_i as usize;
                        let src_s = src_s as usize;
                        let src_ty = &node.ops[src_i].results[src_s];
                        if matches!(src_ty, Type::Slice(_)) {
                            if escaping_vals.insert((src_i, src_s)) {
                                work.push((src_i, src_s));
                            }
                        } else {
                            // Backing storage of SLICE/AS_BYTES is a non-Slice computed in this frame.
                            scratch_ops[src_i][src_s] = true;
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
                                && !crate::layout::is_scalar_abi(src_ty)
                                && !matches!(src_ty, Type::Eff)
                            {
                                // Conservative: callee returning a slice could view an aggregate argument
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
                            } else if !crate::layout::is_scalar_abi(src_ty)
                                && !matches!(src_ty, Type::Eff)
                            {
                                scratch_ops[src_i][src_s] = true;
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Also place any fixed-size intermediate larger than 4 KiB in the scratch block (`docs/06`).
        let mut scratch_offsets = BTreeMap::new();
        let mut local_scratch_bytes = 0u64;
        for (i, op) in node.ops.iter().enumerate() {
            for (s, ty) in op.results.iter().enumerate() {
                if crate::layout::is_erased(ty) {
                    continue;
                }
                let sz = size_of(ty, target)?;
                if !crate::layout::is_scalar_abi(ty)
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
                            // Constant-true plain guard is erased (`docs/06`).
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
                        // Fixed-size array/tensor or constant-count GENERATE adds statically.
                        let count_opt = if op.opcode == OP_GENERATE {
                            match op.operands.get(1).copied() {
                                Some(Operand::Const(ci)) => {
                                    let b = &node.consts[ci as usize].bytes;
                                    Some(u64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8])))
                                }
                                _ => None,
                            }
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

        analyses.insert(
            *h,
            NodeAnalysis {
                use_counts,
                unique_slice_ops,
                inplace_update,
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
    use std::collections::HashMap;
    use vg_core::asm_local;

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
        let (analyses, guards) = analyze_graph(&graph, Target::Aarch64Darwin).unwrap();
        let root_an = &analyses[&graph.root_hash];

        // %0 = SIMD_MAP -> unique_slice_ops = true; %1 = UPDATE(%0, ...) -> inplace_update = true
        let simd_idx = root
            .ops
            .iter()
            .position(|o| o.opcode == OP_SIMD_MAP)
            .unwrap();
        let upd_idx = root.ops.iter().position(|o| o.opcode == OP_UPDATE).unwrap();
        assert!(root_an.unique_slice_ops[simd_idx]);
        assert!(root_an.inplace_update[upd_idx]);

        // Only the dynamic plain ASSERT_GUARD counts as residual guard (constant-true and .branch do not)
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
        let (analyses, _) = analyze_graph(&graph, Target::Aarch64Darwin).unwrap();
        let an = &analyses[&graph.root_hash];
        let concat_idx = root.ops.iter().position(|o| o.opcode == OP_CONCAT).unwrap();
        assert!(an.scratch_ops[concat_idx][0]);
        assert!(an.local_scratch_bytes >= 16);
    }
}
