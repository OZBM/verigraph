//! IR analysis for OmniLift: inlining, constant folding, pointer/alloca analysis,
//! interval/UB checking, dominators, and counted loop extraction.

use std::collections::{BTreeMap, BTreeSet};

use vg_core::diag::{Diagnostic, Span};

use crate::llvm::{
    BasicBlock, BinOpKind, CastKind, Function, GlobalInit, ICmpPred, Inst, InstKind, LlvmType,
    Module, Terminator, ValueRef,
};

fn err(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error("VG-L0001", Span::default(), msg)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PtrBase {
    Param(usize),
    Alloca(usize),
    Global(String),
    Null,
}

#[derive(Clone, Debug)]
pub struct AllocaInfo {
    pub id: usize,
    pub res_name: String,
    pub byte_size: u64,
    pub word_array: bool, // true => Array<u32, byte_size / 4>, false => Array<u8, byte_size>
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamKind {
    Scalar {
        bits: u32,
        signed: bool,
    },
    Slice {
        len_param_idx: Option<usize>,
    },
    FixedArray {
        elem_bits: u32,
        len: u64,
        writable: bool,
    },
    /// Absorbed into a Slice parameter as its runtime length.
    SliceLen {
        slice_param_idx: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interval {
    pub umin: u64,
    pub umax: u64,
    pub smin: i64,
    pub smax: i64,
}

impl Interval {
    pub fn full(bits: u32) -> Self {
        if bits >= 64 {
            Self {
                umin: 0,
                umax: u64::MAX,
                smin: i64::MIN,
                smax: i64::MAX,
            }
        } else {
            let umax = (1u64 << bits) - 1;
            let smax = ((1i64) << (bits - 1)) - 1;
            let smin = -((1i64) << (bits - 1));
            Self {
                umin: 0,
                umax,
                smin,
                smax,
            }
        }
    }

    pub fn constant(v: i128, bits: u32) -> Self {
        let mask = if bits >= 64 {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        };
        let u = (v as u64) & mask;
        let s = if bits >= 64 {
            u as i64
        } else {
            let shift = 64 - bits;
            ((u << shift) as i64) >> shift
        };
        Self {
            umin: u,
            umax: u,
            smin: s,
            smax: s,
        }
    }

    pub fn join(self, other: Self) -> Self {
        Self {
            umin: self.umin.min(other.umin),
            umax: self.umax.max(other.umax),
            smin: self.smin.min(other.smin),
            smax: self.smax.max(other.smax),
        }
    }
}

#[derive(Clone, Debug)]
pub struct UbSite {
    pub kind: String,
    pub block: String,
    pub inst_idx: usize,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct NaturalLoop {
    pub header: String,
    pub body: BTreeSet<String>,
    pub backedges: Vec<String>,
    pub preheader: String,
    pub exit_block: String,
    pub latch: String,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
}

/// Inline internal calls and perform constant/pointer simplification.
pub fn inline_and_simplify(
    module: &Module,
    entry_name: &str,
) -> Result<(Function, bool), Diagnostic> {
    let entry = module
        .functions
        .get(entry_name)
        .ok_or_else(|| err(format!("function @{entry_name} not found")))?
        .clone();

    let mut stack = vec![entry_name.to_string()];
    let mut seq = 0usize;
    let mut func = inline_fn_rec(module, &entry, &mut stack, &mut seq)?;
    let mut had_null_check = false;
    simplify_cfg_and_consts(module, &mut func, &mut had_null_check)?;
    Ok((func, had_null_check))
}

fn inline_fn_rec(
    module: &Module,
    func: &Function,
    stack: &mut Vec<String>,
    seq: &mut usize,
) -> Result<Function, Diagnostic> {
    let mut new_blocks = Vec::new();
    for bb in &func.blocks {
        let mut cur_label = bb.label.clone();
        let mut cur_insts = Vec::new();
        let orig_term = bb.term.clone();

        for inst in &bb.insts {
            if let InstKind::Call { callee, args, .. } = &inst.kind {
                if callee.starts_with("llvm.") {
                    cur_insts.push(inst.clone());
                    continue;
                }
                if stack.contains(callee) {
                    return Err(err(format!("recursion not supported: @{callee}")));
                }
                let Some(callee_fn) = module.functions.get(callee) else {
                    return Err(err(format!(
                        "external function call @{callee} not in lifting subset"
                    )));
                };
                if callee_fn.params.len() != args.len() {
                    return Err(err(format!(
                        "call to @{callee} arity mismatch: expected {}, got {}",
                        callee_fn.params.len(),
                        args.len()
                    )));
                }
                stack.push(callee.clone());
                let inlined_callee = inline_fn_rec(module, callee_fn, stack, seq)?;
                stack.pop();

                *seq += 1;
                let prefix = format!("inl{seq}_");
                let post_label = format!("{prefix}cont");

                let mut val_map: BTreeMap<String, ValueRef> = BTreeMap::new();
                for (p, (_, arg_val)) in inlined_callee.params.iter().zip(args.iter()) {
                    val_map.insert(p.name.clone(), arg_val.clone());
                }
                let mut renamed_blocks = rename_blocks(&inlined_callee.blocks, &prefix, &val_map);
                let callee_entry = renamed_blocks[0].label.clone();

                cur_insts.push(Inst {
                    result: None,
                    kind: InstKind::Call {
                        tail: false,
                        ret_ty: LlvmType::Void,
                        callee: "llvm.assume".to_string(),
                        args: vec![(LlvmType::Int(1), ValueRef::ConstBool(true))],
                    },
                });
                new_blocks.push(BasicBlock {
                    label: cur_label.clone(),
                    insts: std::mem::take(&mut cur_insts),
                    term: Terminator::Br {
                        target: callee_entry,
                    },
                });

                let mut ret_incoming = Vec::new();
                for cbb in &mut renamed_blocks {
                    if let Terminator::Ret(opt_val) = &cbb.term {
                        if let Some((_, rv)) = opt_val {
                            ret_incoming.push((rv.clone(), cbb.label.clone()));
                        }
                        cbb.term = Terminator::Br {
                            target: post_label.clone(),
                        };
                    }
                }
                new_blocks.extend(renamed_blocks);

                cur_label = post_label;
                if let Some(res_name) = &inst.result
                    && !ret_incoming.is_empty()
                {
                    if ret_incoming.len() == 1 {
                        // Emit dummy phi with 1 incoming edge; simplify_cfg collapses it cleanly.
                        cur_insts.push(Inst {
                            result: Some(res_name.clone()),
                            kind: InstKind::Phi {
                                ty: callee_fn.ret_ty.clone(),
                                incoming: ret_incoming,
                            },
                        });
                    } else {
                        cur_insts.push(Inst {
                            result: Some(res_name.clone()),
                            kind: InstKind::Phi {
                                ty: callee_fn.ret_ty.clone(),
                                incoming: ret_incoming,
                            },
                        });
                    }
                }
                continue;
            }
            cur_insts.push(inst.clone());
        }

        // Rewrite phi predecessors in later blocks if cur_label changed due to call splitting.
        new_blocks.push(BasicBlock {
            label: cur_label.clone(),
            insts: cur_insts,
            term: orig_term,
        });
        if cur_label != bb.label {
            // Update incoming phi edges in all blocks that referred to `bb.label`.
            // Since we're iterating `func.blocks`, we'll patch all blocks after the loop.
        }
    }

    // Fix any phi incoming references to blocks that were split by inlining:
    // If original block `L` was split into `L -> ... -> inlX_cont`, any phi referencing `L`
    // in a successor of `L` now receives control from `inlX_cont`!
    let mut exit_label_of: BTreeMap<String, String> = BTreeMap::new();
    for bb in &func.blocks {
        // Find the last block produced from bb before the next original block.
        let mut last = bb.label.clone();
        for nb in &new_blocks {
            if nb.label == bb.label {
                last = nb.label.clone();
            } else if nb.label.starts_with("inl") && nb.label.ends_with("_cont") {
                // Track if reachable via chain; simpler: map based on terminator targets!
                let _ = &last;
            }
        }
        exit_label_of.insert(bb.label.clone(), bb.label.clone());
    }
    // Better: trace chains from each original block label through any `inl*_cont` blocks.
    let label_to_idx: BTreeMap<String, usize> = new_blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.label.clone(), i))
        .collect();
    for bb in &func.blocks {
        // Walk from bb.label's position forward until next original block.
        let Some(&start_idx) = label_to_idx.get(&bb.label) else {
            continue;
        };
        let mut end_lbl = bb.label.clone();
        for nb in &new_blocks[start_idx..] {
            if nb.label != bb.label && !nb.label.starts_with("inl") {
                break;
            }
            if nb.label.starts_with("inl") && nb.label.ends_with("_cont") {
                end_lbl = nb.label.clone();
            }
        }
        exit_label_of.insert(bb.label.clone(), end_lbl);
    }
    for nb in &mut new_blocks {
        if nb.label.starts_with("inl") && !nb.label.ends_with("_cont") {
            continue;
        }
        for inst in &mut nb.insts {
            if let InstKind::Phi { incoming, .. } = &mut inst.kind {
                for (_, pred) in incoming.iter_mut() {
                    if let Some(actual_pred) = exit_label_of.get(pred) {
                        *pred = actual_pred.clone();
                    }
                }
            }
        }
    }

    Ok(Function {
        name: func.name.clone(),
        ret_ty: func.ret_ty.clone(),
        params: func.params.clone(),
        blocks: new_blocks,
    })
}

fn rename_blocks(
    blocks: &[BasicBlock],
    prefix: &str,
    param_map: &BTreeMap<String, ValueRef>,
) -> Vec<BasicBlock> {
    let mut map = param_map.clone();
    for bb in blocks {
        for inst in &bb.insts {
            if let Some(r) = &inst.result {
                map.insert(r.clone(), ValueRef::Local(format!("{prefix}{r}")));
            }
        }
    }
    let map_val = |v: &ValueRef| -> ValueRef {
        match v {
            ValueRef::Local(name) => map.get(name).cloned().unwrap_or_else(|| v.clone()),
            other => other.clone(),
        }
    };
    let map_lbl = |l: &str| format!("{prefix}{l}");

    blocks
        .iter()
        .map(|bb| {
            let insts = bb
                .insts
                .iter()
                .map(|inst| {
                    let result = inst.result.as_ref().map(|r| format!("{prefix}{r}"));
                    let kind = match &inst.kind {
                        InstKind::Phi { ty, incoming } => InstKind::Phi {
                            ty: ty.clone(),
                            incoming: incoming
                                .iter()
                                .map(|(v, l)| (map_val(v), map_lbl(l)))
                                .collect(),
                        },
                        InstKind::BinOp {
                            op,
                            nuw,
                            nsw,
                            exact,
                            disjoint,
                            ty,
                            lhs,
                            rhs,
                        } => InstKind::BinOp {
                            op: *op,
                            nuw: *nuw,
                            nsw: *nsw,
                            exact: *exact,
                            disjoint: *disjoint,
                            ty: ty.clone(),
                            lhs: map_val(lhs),
                            rhs: map_val(rhs),
                        },
                        InstKind::Cast {
                            op,
                            nneg,
                            nuw,
                            nsw,
                            from_ty,
                            val,
                            to_ty,
                        } => InstKind::Cast {
                            op: *op,
                            nneg: *nneg,
                            nuw: *nuw,
                            nsw: *nsw,
                            from_ty: from_ty.clone(),
                            val: map_val(val),
                            to_ty: to_ty.clone(),
                        },
                        InstKind::ICmp {
                            samesign,
                            pred,
                            ty,
                            lhs,
                            rhs,
                        } => InstKind::ICmp {
                            samesign: *samesign,
                            pred: *pred,
                            ty: ty.clone(),
                            lhs: map_val(lhs),
                            rhs: map_val(rhs),
                        },
                        InstKind::Select {
                            cond,
                            ty,
                            then_val,
                            else_val,
                        } => InstKind::Select {
                            cond: map_val(cond),
                            ty: ty.clone(),
                            then_val: map_val(then_val),
                            else_val: map_val(else_val),
                        },
                        InstKind::Alloca { ty, count, align } => InstKind::Alloca {
                            ty: ty.clone(),
                            count: count.as_ref().map(|(b, v)| (*b, map_val(v))),
                            align: *align,
                        },
                        InstKind::Gep {
                            inbounds,
                            nuw,
                            base_ty,
                            ptr,
                            indices,
                        } => InstKind::Gep {
                            inbounds: *inbounds,
                            nuw: *nuw,
                            base_ty: base_ty.clone(),
                            ptr: map_val(ptr),
                            indices: indices
                                .iter()
                                .map(|(t, v)| (t.clone(), map_val(v)))
                                .collect(),
                        },
                        InstKind::Load { ty, ptr, align } => InstKind::Load {
                            ty: ty.clone(),
                            ptr: map_val(ptr),
                            align: *align,
                        },
                        InstKind::Store {
                            ty,
                            val,
                            ptr,
                            align,
                        } => InstKind::Store {
                            ty: ty.clone(),
                            val: map_val(val),
                            ptr: map_val(ptr),
                            align: *align,
                        },
                        InstKind::Call {
                            tail,
                            ret_ty,
                            callee,
                            args,
                        } => InstKind::Call {
                            tail: *tail,
                            ret_ty: ret_ty.clone(),
                            callee: callee.clone(),
                            args: args.iter().map(|(t, v)| (t.clone(), map_val(v))).collect(),
                        },
                    };
                    Inst { result, kind }
                })
                .collect();

            let term = match &bb.term {
                Terminator::Br { target } => Terminator::Br {
                    target: map_lbl(target),
                },
                Terminator::CondBr {
                    cond,
                    then_bb,
                    else_bb,
                } => Terminator::CondBr {
                    cond: map_val(cond),
                    then_bb: map_lbl(then_bb),
                    else_bb: map_lbl(else_bb),
                },
                Terminator::Ret(opt) => {
                    Terminator::Ret(opt.as_ref().map(|(t, v)| (t.clone(), map_val(v))))
                }
                Terminator::Unreachable => Terminator::Unreachable,
            };

            BasicBlock {
                label: map_lbl(&bb.label),
                insts,
                term,
            }
        })
        .collect()
}

/// Iterative constant propagation, non-null pointer simplification, and dead-block pruning.
fn simplify_cfg_and_consts(
    module: &Module,
    func: &mut Function,
    had_null_check: &mut bool,
) -> Result<(), Diagnostic> {
    for _pass in 0..16 {
        let mut changed = false;
        let mut consts: BTreeMap<String, ValueRef> = BTreeMap::new();
        let mut ptr_info: BTreeMap<String, (PtrBase, Option<i64>)> = BTreeMap::new();

        for (i, p) in func.params.iter().enumerate() {
            if p.ty == LlvmType::Ptr {
                ptr_info.insert(p.name.clone(), (PtrBase::Param(i), Some(0)));
            }
        }
        let mut alloca_id = 0usize;
        for bb in &func.blocks {
            for inst in &bb.insts {
                if let InstKind::Alloca { .. } = inst.kind
                    && let Some(r) = &inst.result
                {
                    ptr_info.insert(r.clone(), (PtrBase::Alloca(alloca_id), Some(0)));
                    alloca_id += 1;
                }
            }
        }

        let resolve = |v: &ValueRef, consts: &BTreeMap<String, ValueRef>| -> ValueRef {
            match v {
                ValueRef::Local(n) => consts.get(n).cloned().unwrap_or_else(|| v.clone()),
                _ => v.clone(),
            }
        };

        let resolve_ptr = |v: &ValueRef,
                           ptr_info: &BTreeMap<String, (PtrBase, Option<i64>)>|
         -> Option<(PtrBase, Option<i64>)> {
            match v {
                ValueRef::Local(n) => ptr_info.get(n).cloned(),
                ValueRef::Global(g) => Some((PtrBase::Global(g.clone()), Some(0))),
                ValueRef::ConstNull => Some((PtrBase::Null, Some(0))),
                _ => None,
            }
        };

        let used_outside_assume: BTreeSet<String> = {
            let mut u = BTreeSet::new();
            for bb in &func.blocks {
                for inst in &bb.insts {
                    if let InstKind::Call { callee, .. } = &inst.kind
                        && (callee.starts_with("llvm.lifetime.")
                            || callee == "llvm.assume"
                            || callee == "llvm.experimental.noalias.scope.decl")
                    {
                        continue;
                    }
                    let mut add = |v: &ValueRef| {
                        if let ValueRef::Local(n) = v {
                            u.insert(n.clone());
                        }
                    };
                    match &inst.kind {
                        InstKind::Phi { incoming, .. } => {
                            for (v, _) in incoming {
                                add(v);
                            }
                        }
                        InstKind::BinOp { lhs, rhs, .. } | InstKind::ICmp { lhs, rhs, .. } => {
                            add(lhs);
                            add(rhs);
                        }
                        InstKind::Cast { val, .. } | InstKind::Load { ptr: val, .. } => add(val),
                        InstKind::Select {
                            cond,
                            then_val,
                            else_val,
                            ..
                        } => {
                            add(cond);
                            add(then_val);
                            add(else_val);
                        }
                        InstKind::Gep { ptr, indices, .. } => {
                            add(ptr);
                            for (_, v) in indices {
                                add(v);
                            }
                        }
                        InstKind::Store { val, ptr, .. } => {
                            add(val);
                            add(ptr);
                        }
                        InstKind::Call { args, .. } => {
                            for (_, v) in args {
                                add(v);
                            }
                        }
                        InstKind::Alloca { .. } => {}
                    }
                }
                match &bb.term {
                    Terminator::CondBr { cond, .. } | Terminator::Ret(Some((_, cond))) => {
                        if let ValueRef::Local(n) = cond {
                            u.insert(n.clone());
                        }
                    }
                    _ => {}
                }
            }
            u
        };

        for bb in &mut func.blocks {
            let mut new_insts = Vec::with_capacity(bb.insts.len());
            for mut inst in std::mem::take(&mut bb.insts) {
                // Drop metadata-only intrinsics early.
                if let InstKind::Call { callee, .. } = &inst.kind
                    && (callee.starts_with("llvm.lifetime.")
                        || callee == "llvm.assume"
                        || callee == "llvm.experimental.noalias.scope.decl")
                {
                    changed = true;
                    continue;
                }

                // Substitute known constants into operands.
                substitute_inst(&mut inst, &consts);

                if let Some(res) = &inst.result {
                    match &inst.kind {
                        InstKind::Phi { incoming, .. } => {
                            let first = &incoming[0].0;
                            if incoming.iter().all(|(v, _)| v == first) {
                                consts.insert(res.clone(), first.clone());
                                changed = true;
                                continue;
                            }
                            let resolved: Vec<(PtrBase, Option<i64>)> = incoming
                                .iter()
                                .filter_map(|(v, _)| resolve_ptr(v, &ptr_info))
                                .collect();
                            if !resolved.is_empty() {
                                let b0 = resolved[0].0.clone();
                                if resolved.iter().all(|(b, _)| *b == b0) {
                                    let o0 = resolved[0].1;
                                    let all_same_off = resolved.len() == incoming.len()
                                        && resolved.iter().all(|(_, o)| *o == o0);
                                    ptr_info.insert(
                                        res.clone(),
                                        (b0, if all_same_off { o0 } else { None }),
                                    );
                                }
                            }
                        }
                        InstKind::BinOp {
                            op, ty, lhs, rhs, ..
                        } => {
                            if let (
                                Some(bits),
                                ValueRef::ConstInt { value: a, .. },
                                ValueRef::ConstInt { value: b, .. },
                            ) = (ty.bit_width(), lhs, rhs)
                                && let Some(c) = eval_const_binop(*op, bits, *a, *b)
                            {
                                consts.insert(res.clone(), ValueRef::ConstInt { bits, value: c });
                                changed = true;
                                continue;
                            }
                        }
                        InstKind::Cast {
                            op,
                            from_ty,
                            val,
                            to_ty,
                            ..
                        } => {
                            if let (Some(fbits), Some(tbits), ValueRef::ConstInt { value: v, .. }) =
                                (from_ty.bit_width(), to_ty.bit_width(), val)
                            {
                                let c = eval_const_cast(*op, fbits, *v, tbits);
                                let cval = if tbits == 1 {
                                    ValueRef::ConstBool(c != 0)
                                } else {
                                    ValueRef::ConstInt {
                                        bits: tbits,
                                        value: c,
                                    }
                                };
                                consts.insert(res.clone(), cval);
                                changed = true;
                                continue;
                            }
                        }
                        InstKind::Gep {
                            base_ty,
                            ptr,
                            indices,
                            ..
                        } => {
                            if let Some((base, base_off)) = resolve_ptr(ptr, &ptr_info) {
                                let off = if let Some(bo) = base_off {
                                    eval_const_gep_offset(module, base_ty, indices)?.map(|d| bo + d)
                                } else {
                                    None
                                };
                                ptr_info.insert(res.clone(), (base, off));
                            }
                        }
                        InstKind::Select {
                            cond,
                            ty,
                            then_val,
                            else_val,
                            ..
                        } => {
                            if let ValueRef::ConstBool(b) = cond {
                                let chosen = if *b { then_val } else { else_val };
                                consts.insert(res.clone(), chosen.clone());
                                changed = true;
                                continue;
                            }
                            if *ty == LlvmType::Ptr
                                && let (Some((b1, o1)), Some((b2, o2))) = (
                                    resolve_ptr(then_val, &ptr_info),
                                    resolve_ptr(else_val, &ptr_info),
                                )
                                && b1 == b2
                            {
                                ptr_info
                                    .insert(res.clone(), (b1, if o1 == o2 { o1 } else { None }));
                            }
                        }
                        InstKind::ICmp {
                            pred, ty, lhs, rhs, ..
                        } => {
                            if *ty == LlvmType::Ptr {
                                let p1 = resolve_ptr(lhs, &ptr_info);
                                let p2 = resolve_ptr(rhs, &ptr_info);
                                match (p1, p2) {
                                    (Some((PtrBase::Null, _)), Some((b, _)))
                                    | (Some((b, _)), Some((PtrBase::Null, _)))
                                        if b != PtrBase::Null =>
                                    {
                                        if used_outside_assume.contains(res) {
                                            *had_null_check = true;
                                        }
                                        let ans = match pred {
                                            ICmpPred::Eq => Some(false),
                                            ICmpPred::Ne => Some(true),
                                            _ => None,
                                        };
                                        if let Some(a) = ans {
                                            consts.insert(res.clone(), ValueRef::ConstBool(a));
                                            changed = true;
                                            continue;
                                        }
                                    }
                                    (Some((b1, Some(o1))), Some((b2, Some(o2)))) if b1 == b2 => {
                                        let ans = match pred {
                                            ICmpPred::Eq => Some(o1 == o2),
                                            ICmpPred::Ne => Some(o1 != o2),
                                            ICmpPred::ULt => Some((o1 as u64) < (o2 as u64)),
                                            ICmpPred::ULe => Some((o1 as u64) <= (o2 as u64)),
                                            ICmpPred::UGt => Some((o1 as u64) > (o2 as u64)),
                                            ICmpPred::UGe => Some((o1 as u64) >= (o2 as u64)),
                                            ICmpPred::SLt => Some(o1 < o2),
                                            ICmpPred::SLe => Some(o1 <= o2),
                                            ICmpPred::SGt => Some(o1 > o2),
                                            ICmpPred::SGe => Some(o1 >= o2),
                                        };
                                        if let Some(a) = ans {
                                            consts.insert(res.clone(), ValueRef::ConstBool(a));
                                            changed = true;
                                            continue;
                                        }
                                    }
                                    _ => {}
                                }
                            } else if let (
                                Some(bits),
                                ValueRef::ConstInt { value: a, .. },
                                ValueRef::ConstInt { value: b, .. },
                            ) = (ty.bit_width(), lhs, rhs)
                            {
                                let ans = eval_const_icmp(*pred, bits, *a, *b);
                                consts.insert(res.clone(), ValueRef::ConstBool(ans));
                                changed = true;
                                continue;
                            }
                        }
                        _ => {}
                    }
                }
                new_insts.push(inst);
            }
            bb.insts = new_insts;

            match &mut bb.term {
                Terminator::CondBr {
                    cond,
                    then_bb,
                    else_bb,
                } => {
                    *cond = resolve(cond, &consts);
                    if let ValueRef::ConstBool(b) = *cond {
                        let target = if b { then_bb.clone() } else { else_bb.clone() };
                        bb.term = Terminator::Br { target };
                        changed = true;
                    }
                }
                Terminator::Ret(Some((_, v))) => {
                    let nv = resolve(v, &consts);
                    if nv != *v {
                        *v = nv;
                        changed = true;
                    }
                }
                _ => {}
            }
        }

        // Detect 1-iteration loop latches whose pointer equality check exits immediately on iter 0.
        let mut iter0_exits: Vec<(usize, String, bool, String)> = Vec::new();
        for (bi, bb) in func.blocks.iter().enumerate() {
            let Terminator::CondBr {
                cond: ValueRef::Local(cname),
                then_bb,
                else_bb,
            } = &bb.term
            else {
                continue;
            };
            let Some(cinst) = bb
                .insts
                .iter()
                .find(|i| i.result.as_deref() == Some(cname.as_str()))
            else {
                continue;
            };
            let InstKind::ICmp {
                pred: pred @ (ICmpPred::Eq | ICmpPred::Ne),
                ty: LlvmType::Ptr,
                lhs: ValueRef::Local(phi_name),
                rhs,
                ..
            } = &cinst.kind
            else {
                continue;
            };
            for hdr_lbl in [then_bb, else_bb] {
                let Some(hdr_bb) = func.blocks.iter().find(|b| &b.label == hdr_lbl) else {
                    continue;
                };
                let Some(pinst) = hdr_bb
                    .insts
                    .iter()
                    .find(|i| i.result.as_deref() == Some(phi_name.as_str()))
                else {
                    continue;
                };
                let InstKind::Phi { incoming, .. } = &pinst.kind else {
                    continue;
                };
                if incoming.len() == 2
                    && incoming.iter().any(|(_, l)| l == &bb.label)
                    && let Some((init_val, _)) = incoming.iter().find(|(_, l)| l != &bb.label)
                    && let (Some((b1, Some(o1))), Some((b2, Some(o2)))) = (
                        resolve_ptr(init_val, &ptr_info),
                        resolve_ptr(rhs, &ptr_info),
                    )
                    && b1 == b2
                {
                    let cond0 = if *pred == ICmpPred::Eq {
                        o1 == o2
                    } else {
                        o1 != o2
                    };
                    let target0 = if cond0 { then_bb } else { else_bb };
                    if target0 != hdr_lbl {
                        iter0_exits.push((bi, cname.clone(), cond0, target0.clone()));
                        break;
                    }
                }
            }
        }
        for (bi, cname, cond0, target0) in iter0_exits {
            func.blocks[bi].term = Terminator::Br { target: target0 };
            let mut local_c = BTreeMap::new();
            local_c.insert(cname.clone(), ValueRef::ConstBool(cond0));
            func.blocks[bi]
                .insts
                .retain(|i| i.result.as_deref() != Some(cname.as_str()));
            for bb in &mut func.blocks {
                for inst in &mut bb.insts {
                    substitute_inst(inst, &local_c);
                }
            }
            changed = true;
        }

        // Remove unreachable blocks and prune phi incoming edges from non-predecessor blocks.
        let reachable = reachable_blocks(func);
        if reachable.len() < func.blocks.len() {
            func.blocks.retain(|b| reachable.contains(&b.label));
            changed = true;
        }
        let mut actual_preds: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for bb in &func.blocks {
            for s in term_succs(&bb.term) {
                actual_preds.entry(s).or_default().insert(bb.label.clone());
            }
        }
        for bb in &mut func.blocks {
            let preds = actual_preds.get(&bb.label);
            for inst in &mut bb.insts {
                if let InstKind::Phi { incoming, .. } = &mut inst.kind {
                    let before = incoming.len();
                    incoming.retain(|(_, pred)| preds.is_some_and(|ps| ps.contains(pred)));
                    if incoming.len() < before {
                        changed = true;
                    }
                }
            }
        }

        if !changed {
            break;
        }
    }
    Ok(())
}

fn substitute_inst(inst: &mut Inst, consts: &BTreeMap<String, ValueRef>) {
    let sub = |v: &mut ValueRef| {
        if let ValueRef::Local(n) = v
            && let Some(c) = consts.get(n)
        {
            *v = c.clone();
        }
    };
    match &mut inst.kind {
        InstKind::Phi { incoming, .. } => {
            for (v, _) in incoming {
                sub(v);
            }
        }
        InstKind::BinOp { lhs, rhs, .. } => {
            sub(lhs);
            sub(rhs);
        }
        InstKind::Cast { val, .. } => sub(val),
        InstKind::ICmp { lhs, rhs, .. } => {
            sub(lhs);
            sub(rhs);
        }
        InstKind::Select {
            cond,
            then_val,
            else_val,
            ..
        } => {
            sub(cond);
            sub(then_val);
            sub(else_val);
        }
        InstKind::Alloca { count, .. } => {
            if let Some((_, v)) = count {
                sub(v);
            }
        }
        InstKind::Gep { ptr, indices, .. } => {
            sub(ptr);
            for (_, v) in indices {
                sub(v);
            }
        }
        InstKind::Load { ptr, .. } => sub(ptr),
        InstKind::Store { val, ptr, .. } => {
            sub(val);
            sub(ptr);
        }
        InstKind::Call { args, .. } => {
            for (_, v) in args {
                sub(v);
            }
        }
    }
}

pub fn eval_const_gep_offset(
    module: &Module,
    base_ty: &LlvmType,
    indices: &[(LlvmType, ValueRef)],
) -> Result<Option<i64>, Diagnostic> {
    if indices.is_empty() {
        return Ok(Some(0));
    }
    let ValueRef::ConstInt { value: i0, .. } = indices[0].1 else {
        return Ok(None);
    };
    let stride0 = module.sizeof(base_ty)? as i64;
    let mut total = (i0 as i64) * stride0;
    let mut cur_ty = base_ty.clone();
    for (_, idx_val) in &indices[1..] {
        let ValueRef::ConstInt { value: idx, .. } = idx_val else {
            return Ok(None);
        };
        match cur_ty {
            LlvmType::Array(_, elem) => {
                let esz = module.sizeof(&elem)? as i64;
                total += (*idx as i64) * esz;
                cur_ty = *elem;
            }
            LlvmType::Struct { .. } | LlvmType::Named(_) => {
                let (foff, fty) = module.struct_field(&cur_ty, *idx as usize)?;
                total += foff as i64;
                cur_ty = fty;
            }
            _ => return Ok(None),
        }
    }
    Ok(Some(total))
}

fn mask_u(v: i128, bits: u32) -> u128 {
    if bits >= 128 {
        v as u128
    } else {
        (v as u128) & ((1u128 << bits) - 1)
    }
}

fn sign_ext(v: i128, bits: u32) -> i128 {
    let shift = 128 - bits;
    ((mask_u(v, bits) << shift) as i128) >> shift
}

fn eval_const_binop(op: BinOpKind, bits: u32, a: i128, b: i128) -> Option<i128> {
    let ua = mask_u(a, bits);
    let ub = mask_u(b, bits);
    let sa = sign_ext(a, bits);
    let sb = sign_ext(b, bits);
    let r: i128 = match op {
        BinOpKind::Add => ua.wrapping_add(ub) as i128,
        BinOpKind::Sub => ua.wrapping_sub(ub) as i128,
        BinOpKind::Mul => ua.wrapping_mul(ub) as i128,
        BinOpKind::UDiv => {
            if ub == 0 {
                return None;
            }
            (ua / ub) as i128
        }
        BinOpKind::SDiv => {
            if sb == 0 {
                return None;
            }
            sa / sb
        }
        BinOpKind::URem => {
            if ub == 0 {
                return None;
            }
            (ua % ub) as i128
        }
        BinOpKind::SRem => {
            if sb == 0 {
                return None;
            }
            sa % sb
        }
        BinOpKind::And => (ua & ub) as i128,
        BinOpKind::Or => (ua | ub) as i128,
        BinOpKind::Xor => (ua ^ ub) as i128,
        BinOpKind::Shl => {
            if ub >= u128::from(bits) {
                return None;
            }
            (ua << ub) as i128
        }
        BinOpKind::LShr => {
            if ub >= u128::from(bits) {
                return None;
            }
            (ua >> ub) as i128
        }
        BinOpKind::AShr => {
            if ub >= u128::from(bits) {
                return None;
            }
            sa >> ub
        }
    };
    Some(mask_u(r, bits) as i128)
}

fn eval_const_cast(op: CastKind, fbits: u32, v: i128, tbits: u32) -> i128 {
    match op {
        CastKind::ZExt => mask_u(v, fbits) as i128,
        CastKind::SExt => mask_u(sign_ext(v, fbits), tbits) as i128,
        CastKind::Trunc => mask_u(v, tbits) as i128,
    }
}

fn eval_const_icmp(pred: ICmpPred, bits: u32, a: i128, b: i128) -> bool {
    let ua = mask_u(a, bits);
    let ub = mask_u(b, bits);
    let sa = sign_ext(a, bits);
    let sb = sign_ext(b, bits);
    match pred {
        ICmpPred::Eq => ua == ub,
        ICmpPred::Ne => ua != ub,
        ICmpPred::UGt => ua > ub,
        ICmpPred::UGe => ua >= ub,
        ICmpPred::ULt => ua < ub,
        ICmpPred::ULe => ua <= ub,
        ICmpPred::SGt => sa > sb,
        ICmpPred::SGe => sa >= sb,
        ICmpPred::SLt => sa < sb,
        ICmpPred::SLe => sa <= sb,
    }
}

pub fn reachable_blocks(func: &Function) -> BTreeSet<String> {
    let idx_map: BTreeMap<&str, usize> = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.label.as_str(), i))
        .collect();
    let mut visited = BTreeSet::new();
    let mut stack = vec![func.blocks[0].label.clone()];
    while let Some(lbl) = stack.pop() {
        if !visited.insert(lbl.clone()) {
            continue;
        }
        let bb = &func.blocks[idx_map[lbl.as_str()]];
        for succ in term_succs(&bb.term) {
            if !visited.contains(&succ) {
                stack.push(succ);
            }
        }
    }
    visited
}

pub fn term_succs(term: &Terminator) -> Vec<String> {
    match term {
        Terminator::Br { target } => vec![target.clone()],
        Terminator::CondBr {
            then_bb, else_bb, ..
        } => vec![then_bb.clone(), else_bb.clone()],
        Terminator::Ret(_) | Terminator::Unreachable => Vec::new(),
    }
}

/// Compute Reverse Post-Order of reachable basic blocks.
pub fn compute_rpo(func: &Function) -> Vec<String> {
    let idx_map: BTreeMap<&str, usize> = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.label.as_str(), i))
        .collect();
    let mut visited = BTreeSet::new();
    let mut post = Vec::new();
    fn dfs(
        lbl: &str,
        func: &Function,
        idx_map: &BTreeMap<&str, usize>,
        visited: &mut BTreeSet<String>,
        post: &mut Vec<String>,
    ) {
        if !visited.insert(lbl.to_string()) {
            return;
        }
        let bb = &func.blocks[idx_map[lbl]];
        for s in term_succs(&bb.term) {
            dfs(&s, func, idx_map, visited, post);
        }
        post.push(lbl.to_string());
    }
    dfs(
        &func.blocks[0].label,
        func,
        &idx_map,
        &mut visited,
        &mut post,
    );
    post.reverse();
    post
}

/// Compute immediate dominators (`idom[block] = Some(parent_block)` for all non-entry blocks).
pub fn compute_dominators(func: &Function) -> BTreeMap<String, Option<String>> {
    let rpo = compute_rpo(func);
    let rpo_pos: BTreeMap<&str, usize> = rpo
        .iter()
        .enumerate()
        .map(|(i, l)| (l.as_str(), i))
        .collect();
    let mut preds: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for l in &rpo {
        preds.insert(l.as_str(), Vec::new());
    }
    for bb in &func.blocks {
        if !rpo_pos.contains_key(bb.label.as_str()) {
            continue;
        }
        for s in term_succs(&bb.term) {
            if let Some(ps) = preds.get_mut(s.as_str()) {
                ps.push(bb.label.as_str());
            }
        }
    }

    let n = rpo.len();
    let mut idom: Vec<Option<usize>> = vec![None; n];
    idom[0] = Some(0);

    let intersect = |mut b1: usize, mut b2: usize, idom: &[Option<usize>]| -> usize {
        while b1 != b2 {
            while b1 > b2 {
                b1 = idom[b1].unwrap();
            }
            while b2 > b1 {
                b2 = idom[b2].unwrap();
            }
        }
        b1
    };

    let mut changed = true;
    while changed {
        changed = false;
        for b in 1..n {
            let ps = &preds[rpo[b].as_str()];
            let mut new_idom: Option<usize> = None;
            for &p in ps {
                let p_idx = rpo_pos[p];
                if idom[p_idx].is_some() {
                    new_idom = Some(match new_idom {
                        None => p_idx,
                        Some(cur) => intersect(p_idx, cur, &idom),
                    });
                }
            }
            if new_idom.is_some() && idom[b] != new_idom {
                idom[b] = new_idom;
                changed = true;
            }
        }
    }

    let mut out = BTreeMap::new();
    out.insert(rpo[0].clone(), None);
    for i in 1..n {
        out.insert(rpo[i].clone(), idom[i].map(|p| rpo[p].clone()));
    }
    out
}

pub fn dominates<'a>(idom: &'a BTreeMap<String, Option<String>>, a: &str, mut b: &'a str) -> bool {
    loop {
        if a == b {
            return true;
        }
        match idom.get(b).and_then(|o| o.as_deref()) {
            Some(parent) => b = parent,
            None => return false,
        }
    }
}

/// Find all natural loops, check reducibility, and return loops sorted innermost-first.
pub fn find_natural_loops(func: &Function) -> Result<Vec<NaturalLoop>, Diagnostic> {
    let idom = compute_dominators(func);
    let mut preds: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for bb in &func.blocks {
        preds.entry(bb.label.clone()).or_default();
        for s in term_succs(&bb.term) {
            preds.entry(s).or_default().push(bb.label.clone());
        }
    }

    let mut by_header: BTreeMap<String, (BTreeSet<String>, Vec<String>)> = BTreeMap::new();
    for bb in &func.blocks {
        for s in term_succs(&bb.term) {
            if dominates(&idom, &s, &bb.label) {
                let entry = by_header
                    .entry(s.clone())
                    .or_insert_with(|| (BTreeSet::from([s.clone()]), Vec::new()));
                entry.1.push(bb.label.clone());
                let mut stack = vec![bb.label.clone()];
                while let Some(node) = stack.pop() {
                    if entry.0.insert(node.clone()) {
                        for p in preds.get(&node).into_iter().flatten() {
                            if !entry.0.contains(p) {
                                stack.push(p.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    let bb_map: BTreeMap<&str, &BasicBlock> =
        func.blocks.iter().map(|b| (b.label.as_str(), b)).collect();
    let mut loops = Vec::new();
    for (header, (body, backedges)) in by_header {
        // Check single preheader (loop-simplify invariant).
        let outside_preds: Vec<String> = preds
            .get(&header)
            .into_iter()
            .flatten()
            .filter(|p| !body.contains(*p))
            .cloned()
            .collect();
        if outside_preds.len() != 1 {
            return Err(err(format!(
                "loop at %{header} has {} outside predecessors (expected 1 preheader)",
                outside_preds.len()
            )));
        }
        // Check irreducibility: no body block other than header may have an outside pred.
        for b in &body {
            if *b == header {
                continue;
            }
            for p in preds.get(b).into_iter().flatten() {
                if !body.contains(p) {
                    return Err(err(format!("irreducible loop entry into %{b} from %{p}")));
                }
            }
        }

        if backedges.len() != 1 {
            return Err(err(format!(
                "loop at %{header} has {} backedges (expected 1 latch)",
                backedges.len()
            )));
        }
        let latch = backedges[0].clone();

        // Collect all exit edges from the loop body.
        let mut exit_targets = BTreeSet::new();
        for b in &body {
            let bb = bb_map[b.as_str()];
            for s in term_succs(&bb.term) {
                if !body.contains(&s) {
                    exit_targets.insert(s);
                }
            }
        }
        if exit_targets.len() != 1 {
            return Err(err(format!(
                "loop at %{header} has {} exit blocks (expected 1 exit)",
                exit_targets.len()
            )));
        }
        let exit_block = exit_targets.into_iter().next().unwrap();

        loops.push(NaturalLoop {
            header,
            body,
            backedges,
            preheader: outside_preds[0].clone(),
            exit_block,
            latch,
            children: Vec::new(),
            parent: None,
        });
    }

    // Sort innermost first (smallest body first).
    loops.sort_by_key(|l| (l.body.len(), l.header.clone()));
    for i in 0..loops.len() {
        for j in (i + 1)..loops.len() {
            if loops[i].body.is_subset(&loops[j].body) {
                loops[i].parent = Some(j);
                loops[j].children.push(i);
                break;
            }
        }
    }

    Ok(loops)
}

pub type MemoryAndParams = (
    Vec<AllocaInfo>,
    Vec<ParamKind>,
    BTreeMap<String, (PtrBase, Option<i64>)>,
);

/// Analyze local `alloca`s and classify every pointer/scalar parameter.
pub fn analyze_memory_and_params(
    module: &Module,
    func: &Function,
) -> Result<MemoryAndParams, Diagnostic> {
    let mut allocas = Vec::new();
    let mut ptr_info: BTreeMap<String, (PtrBase, Option<i64>)> = BTreeMap::new();

    for (i, p) in func.params.iter().enumerate() {
        if p.ty == LlvmType::Ptr {
            ptr_info.insert(p.name.clone(), (PtrBase::Param(i), Some(0)));
        }
    }

    for bb in &func.blocks {
        for inst in &bb.insts {
            if let InstKind::Alloca { ty, count, .. } = &inst.kind {
                if let Some((_, cval)) = count
                    && !matches!(cval, ValueRef::ConstInt { value: 1, .. })
                {
                    return Err(err("dynamic alloca count not supported"));
                }
                let res = inst
                    .result
                    .as_ref()
                    .ok_or_else(|| err("alloca without result"))?
                    .clone();
                let sz = module.sizeof(ty)?;
                let id = allocas.len();
                allocas.push(AllocaInfo {
                    id,
                    res_name: res.clone(),
                    byte_size: sz,
                    word_array: sz.is_multiple_of(4),
                });
                ptr_info.insert(res, (PtrBase::Alloca(id), Some(0)));
            }
        }
    }

    // Propagate PtrBase through GEPs, Phis, and Selects to a fixpoint.
    for _ in 0..16 {
        let mut changed = false;
        for bb in &func.blocks {
            for inst in &bb.insts {
                let Some(res) = &inst.result else { continue };
                match &inst.kind {
                    InstKind::Gep {
                        base_ty,
                        ptr,
                        indices,
                        ..
                    } => {
                        let base_resolved = match ptr {
                            ValueRef::Local(n) => ptr_info.get(n).cloned(),
                            ValueRef::Global(g) => Some((PtrBase::Global(g.clone()), Some(0))),
                            ValueRef::ConstNull => Some((PtrBase::Null, Some(0))),
                            _ => None,
                        };
                        if let Some((base, bo)) = base_resolved {
                            let new_off = if let Some(b_off) = bo {
                                eval_const_gep_offset(module, base_ty, indices)?.map(|d| b_off + d)
                            } else {
                                None
                            };
                            let entry = (base, new_off);
                            if ptr_info.get(res) != Some(&entry) {
                                ptr_info.insert(res.clone(), entry);
                                changed = true;
                            }
                        }
                    }
                    InstKind::Phi { incoming, .. } => {
                        let mut base: Option<PtrBase> = None;
                        let mut same_off = true;
                        let mut off0: Option<i64> = None;
                        for (v, _) in incoming {
                            let resolved = match v {
                                ValueRef::Local(n) => ptr_info.get(n).cloned(),
                                ValueRef::Global(g) => Some((PtrBase::Global(g.clone()), Some(0))),
                                ValueRef::ConstNull => Some((PtrBase::Null, Some(0))),
                                _ => None,
                            };
                            if let Some((b, o)) = resolved {
                                match &base {
                                    None => {
                                        base = Some(b);
                                        off0 = o;
                                    }
                                    Some(prev) if *prev == b => {
                                        if o != off0 {
                                            same_off = false;
                                        }
                                    }
                                    Some(_) => {
                                        // Allow Null mixed? Reject mixed bases.
                                        return Err(err(format!(
                                            "pointer phi %{res} merges distinct memory bases"
                                        )));
                                    }
                                }
                            }
                        }
                        if let Some(b) = base {
                            let entry = (b, if same_off { off0 } else { None });
                            if ptr_info.get(res) != Some(&entry) {
                                ptr_info.insert(res.clone(), entry);
                                changed = true;
                            }
                        }
                    }
                    InstKind::Select {
                        ty,
                        then_val,
                        else_val,
                        ..
                    } if *ty == LlvmType::Ptr => {
                        let r1 = match then_val {
                            ValueRef::Local(n) => ptr_info.get(n).cloned(),
                            ValueRef::Global(g) => Some((PtrBase::Global(g.clone()), Some(0))),
                            _ => None,
                        };
                        let r2 = match else_val {
                            ValueRef::Local(n) => ptr_info.get(n).cloned(),
                            ValueRef::Global(g) => Some((PtrBase::Global(g.clone()), Some(0))),
                            _ => None,
                        };
                        if let (Some((b1, o1)), Some((b2, o2))) = (r1, r2) {
                            if b1 != b2 {
                                return Err(err(format!(
                                    "pointer select %{res} merges distinct memory bases"
                                )));
                            }
                            let entry = (b1, if o1 == o2 { o1 } else { None });
                            if ptr_info.get(res) != Some(&entry) {
                                ptr_info.insert(res.clone(), entry);
                                changed = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if !changed {
            break;
        }
    }

    // Validate memory operations (reject pointer-to-pointer, check alloca word alignment).
    let mut param_writes = vec![false; func.params.len()];
    let mut param_max_const_end: Vec<Option<u64>> = vec![Some(0); func.params.len()];
    let mut param_has_dyn_offset = vec![false; func.params.len()];
    let mut param_elem_bits: Vec<Option<u32>> = vec![None; func.params.len()];

    let resolve_val_ptr = |v: &ValueRef| -> Option<(PtrBase, Option<i64>)> {
        match v {
            ValueRef::Local(n) => ptr_info.get(n).cloned(),
            ValueRef::Global(g) => Some((PtrBase::Global(g.clone()), Some(0))),
            ValueRef::ConstNull => Some((PtrBase::Null, Some(0))),
            _ => None,
        }
    };

    for bb in &func.blocks {
        for inst in &bb.insts {
            match &inst.kind {
                InstKind::Load { ty, ptr, align } => {
                    if *ty == LlvmType::Ptr {
                        return Err(err("pointer-to-pointer load not supported"));
                    }
                    let bits = ty
                        .bit_width()
                        .ok_or_else(|| err(format!("unsupported load type {ty:?}")))?;
                    let bytes = u64::from(bits.div_ceil(8));
                    let Some((base, off)) = resolve_val_ptr(ptr) else {
                        return Err(err("load from untracked pointer"));
                    };
                    match base {
                        PtrBase::Alloca(aid) => {
                            if bits != 32
                                || off.is_some_and(|o| o < 0 || !(o as u64).is_multiple_of(4))
                            {
                                allocas[aid].word_array = false;
                            }
                        }
                        PtrBase::Param(pi) => {
                            let elem_bits = if *align == Some(1) { 8 } else { bits };
                            record_param_access(
                                pi,
                                elem_bits,
                                bytes,
                                off,
                                false,
                                &mut param_writes,
                                &mut param_max_const_end,
                                &mut param_has_dyn_offset,
                                &mut param_elem_bits,
                            );
                        }
                        PtrBase::Global(gname) => {
                            let g = module
                                .globals
                                .get(&gname)
                                .ok_or_else(|| err(format!("unknown global @{gname}")))?;
                            if !matches!(g.init, GlobalInit::IntArray { .. } | GlobalInit::ZeroInit)
                            {
                                return Err(err(format!(
                                    "unsupported global @{gname} initializer"
                                )));
                            }
                        }
                        PtrBase::Null => {
                            return Err(err("unconditional null pointer load"));
                        }
                    }
                }
                InstKind::Store { ty, ptr, align, .. } => {
                    if *ty == LlvmType::Ptr {
                        return Err(err("pointer-to-pointer store not supported"));
                    }
                    let bits = ty
                        .bit_width()
                        .ok_or_else(|| err(format!("unsupported store type {ty:?}")))?;
                    let bytes = u64::from(bits.div_ceil(8));
                    let Some((base, off)) = resolve_val_ptr(ptr) else {
                        return Err(err("store to untracked pointer"));
                    };
                    match base {
                        PtrBase::Alloca(aid) => {
                            if bits != 32
                                || off.is_some_and(|o| o < 0 || !(o as u64).is_multiple_of(4))
                            {
                                allocas[aid].word_array = false;
                            }
                        }
                        PtrBase::Param(pi) => {
                            let elem_bits = if *align == Some(1) { 8 } else { bits };
                            record_param_access(
                                pi,
                                elem_bits,
                                bytes,
                                off,
                                true,
                                &mut param_writes,
                                &mut param_max_const_end,
                                &mut param_has_dyn_offset,
                                &mut param_elem_bits,
                            );
                        }
                        PtrBase::Global(gname) => {
                            return Err(err(format!("store to global @{gname} not supported")));
                        }
                        PtrBase::Null => {
                            return Err(err("unconditional null pointer store"));
                        }
                    }
                }
                InstKind::Call { callee, args, .. } if callee.starts_with("llvm.memcpy.") => {
                    let Some((dst_base, dst_off)) = resolve_val_ptr(&args[0].1) else {
                        return Err(err("memcpy with untracked dst pointer"));
                    };
                    let Some((src_base, src_off)) = resolve_val_ptr(&args[1].1) else {
                        return Err(err("memcpy with untracked src pointer"));
                    };
                    let ValueRef::ConstInt { value: nbytes, .. } = args[2].1 else {
                        return Err(err("dynamic-length memcpy not supported"));
                    };
                    let nb = nbytes as u64;
                    if let PtrBase::Alloca(aid) = dst_base {
                        if !nb.is_multiple_of(4)
                            || dst_off.is_some_and(|o| o < 0 || !(o as u64).is_multiple_of(4))
                        {
                            allocas[aid].word_array = false;
                        }
                    } else if let PtrBase::Param(pi) = dst_base {
                        param_writes[pi] = true;
                        if param_elem_bits[pi].is_none()
                            && nb.is_multiple_of(4)
                            && matches!(src_base, PtrBase::Alloca(aid) if allocas[aid].word_array)
                        {
                            param_elem_bits[pi] = Some(32);
                        }
                        if let Some(o) = dst_off
                            && o >= 0
                            && let Some(cur) = param_max_const_end[pi]
                        {
                            param_max_const_end[pi] = Some(cur.max(o as u64 + nb));
                        } else {
                            param_has_dyn_offset[pi] = true;
                        }
                    }
                    if let PtrBase::Alloca(aid) = src_base {
                        if !nb.is_multiple_of(4)
                            || src_off.is_some_and(|o| o < 0 || !(o as u64).is_multiple_of(4))
                        {
                            allocas[aid].word_array = false;
                        }
                    } else if let PtrBase::Param(pi) = src_base {
                        if param_elem_bits[pi].is_none()
                            && nb.is_multiple_of(4)
                            && matches!(dst_base, PtrBase::Alloca(aid) if allocas[aid].word_array)
                        {
                            param_elem_bits[pi] = Some(32);
                        }
                        if let Some(o) = src_off
                            && o >= 0
                            && let Some(cur) = param_max_const_end[pi]
                        {
                            param_max_const_end[pi] = Some(cur.max(o as u64 + nb));
                        } else {
                            param_has_dyn_offset[pi] = true;
                        }
                    }
                }
                InstKind::Call { callee, args, .. } if callee.starts_with("llvm.memset.") => {
                    let Some((dst_base, dst_off)) = resolve_val_ptr(&args[0].1) else {
                        return Err(err("memset with untracked dst pointer"));
                    };
                    let ValueRef::ConstInt { value: nbytes, .. } = args[2].1 else {
                        return Err(err("dynamic-length memset not supported"));
                    };
                    let nb = nbytes as u64;
                    if let PtrBase::Alloca(aid) = dst_base
                        && (!nb.is_multiple_of(4)
                            || dst_off.is_some_and(|o| o < 0 || !(o as u64).is_multiple_of(4)))
                    {
                        allocas[aid].word_array = false;
                    }
                }
                _ => {}
            }
        }
    }

    // Detect if a pointer param with dynamic offset has a paired length integer param.
    let mut param_kinds = vec![
        ParamKind::Scalar {
            bits: 64,
            signed: false
        };
        func.params.len()
    ];
    let mut absorbed_len_for: BTreeMap<usize, usize> = BTreeMap::new();

    for (i, p) in func.params.iter().enumerate() {
        if p.ty == LlvmType::Ptr {
            let len_idx = find_paired_len_param(func, i, &absorbed_len_for);
            if len_idx.is_none()
                && let Some(deref_bytes) = p.dereferenceable
            {
                let eb = param_elem_bits[i].unwrap_or(8);
                let elem_bytes = u64::from(eb.div_ceil(8));
                param_kinds[i] = ParamKind::FixedArray {
                    elem_bits: eb,
                    len: deref_bytes / elem_bytes,
                    writable: param_writes[i],
                };
            } else if !param_has_dyn_offset[i] && param_max_const_end[i].is_some_and(|e| e > 0) {
                let end = param_max_const_end[i].unwrap();
                let eb = param_elem_bits[i].unwrap_or(8);
                let elem_bytes = u64::from(eb.div_ceil(8));
                param_kinds[i] = ParamKind::FixedArray {
                    elem_bits: eb,
                    len: end.div_ceil(elem_bytes),
                    writable: param_writes[i],
                };
            } else if len_idx.is_none()
                && let Some(max_end) = max_bounded_param_byte_end(module, func, i, &ptr_info)
                && max_end > 0
            {
                let eb = param_elem_bits[i].unwrap_or(8);
                let elem_bytes = u64::from(eb.div_ceil(8));
                param_kinds[i] = ParamKind::FixedArray {
                    elem_bits: eb,
                    len: max_end.div_ceil(elem_bytes),
                    writable: param_writes[i],
                };
            } else {
                if param_writes[i] {
                    return Err(err(format!(
                        "dynamic-length writable pointer parameter %{} not supported",
                        p.name
                    )));
                }
                if let Some(li) = len_idx {
                    absorbed_len_for.insert(li, i);
                }
                param_kinds[i] = ParamKind::Slice {
                    len_param_idx: len_idx,
                };
            }
        }
    }

    for (i, p) in func.params.iter().enumerate() {
        if p.ty == LlvmType::Ptr {
            continue;
        }
        if let Some(&slice_idx) = absorbed_len_for.get(&i) {
            param_kinds[i] = ParamKind::SliceLen {
                slice_param_idx: slice_idx,
            };
        } else {
            let bits =
                p.ty.bit_width()
                    .ok_or_else(|| err(format!("unsupported parameter type {:?}", p.ty)))?;
            let signed = is_param_used_signed(func, &p.name);
            param_kinds[i] = ParamKind::Scalar { bits, signed };
        }
    }

    Ok((allocas, param_kinds, ptr_info))
}

#[allow(clippy::too_many_arguments)]
fn record_param_access(
    pi: usize,
    bits: u32,
    bytes: u64,
    off: Option<i64>,
    is_write: bool,
    param_writes: &mut [bool],
    param_max_const_end: &mut [Option<u64>],
    param_has_dyn_offset: &mut [bool],
    param_elem_bits: &mut [Option<u32>],
) {
    if is_write {
        param_writes[pi] = true;
    }
    match param_elem_bits[pi] {
        None => param_elem_bits[pi] = Some(bits),
        Some(prev) if prev != bits => param_elem_bits[pi] = Some(8),
        _ => {}
    }
    if let Some(o) = off
        && o >= 0
    {
        if let Some(cur) = param_max_const_end[pi] {
            param_max_const_end[pi] = Some(cur.max(o as u64 + bytes));
        }
    } else {
        param_has_dyn_offset[pi] = true;
        param_max_const_end[pi] = None;
    }
}

fn max_bounded_param_byte_end(
    module: &Module,
    func: &Function,
    param_idx: usize,
    ptr_info: &BTreeMap<String, (PtrBase, Option<i64>)>,
) -> Option<u64> {
    let idom = compute_dominators(func);

    // Map local value name -> upper bound at `block_label`, if bounded by dominating `icmp ult/ule` or constant arithmetic.
    fn val_upper_bound(
        func: &Function,
        idom: &BTreeMap<String, Option<String>>,
        block_label: &str,
        v: &ValueRef,
        depth: usize,
    ) -> Option<u64> {
        if depth > 12 {
            return None;
        }
        match v {
            ValueRef::ConstInt { value, .. } if *value >= 0 => Some(*value as u64),
            ValueRef::Local(name) => {
                // Check dominating `CondBr` guards (`%x < C` or `%x <= C`).
                for bb in &func.blocks {
                    if !dominates(idom, &bb.label, block_label) || bb.label == block_label {
                        continue;
                    }
                    if let Terminator::CondBr {
                        cond: ValueRef::Local(cname),
                        then_bb,
                        else_bb,
                    } = &bb.term
                    {
                        for inst in &bb.insts {
                            if inst.result.as_deref() == Some(cname.as_str())
                                && let InstKind::ICmp {
                                    pred,
                                    lhs: ValueRef::Local(l),
                                    rhs: ValueRef::ConstInt { value: c, .. },
                                    ..
                                } = &inst.kind
                                && l == name
                                && *c > 0
                            {
                                let c = *c as u64;
                                if dominates(idom, then_bb, block_label) {
                                    match pred {
                                        ICmpPred::ULt | ICmpPred::SLt => return Some(c - 1),
                                        ICmpPred::ULe | ICmpPred::SLe => return Some(c),
                                        _ => {}
                                    }
                                }
                                if dominates(idom, else_bb, block_label) {
                                    match pred {
                                        ICmpPred::UGe | ICmpPred::SGe => return Some(c - 1),
                                        ICmpPred::UGt | ICmpPred::SGt => return Some(c),
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                // Check defining instruction of `name`.
                for bb in &func.blocks {
                    for inst in &bb.insts {
                        if inst.result.as_deref() == Some(name.as_str()) {
                            return match &inst.kind {
                                InstKind::BinOp {
                                    op: BinOpKind::Shl,
                                    lhs,
                                    rhs: ValueRef::ConstInt { value: sh, .. },
                                    ..
                                } if *sh >= 0 && *sh < 32 => {
                                    let l = val_upper_bound(func, idom, &bb.label, lhs, depth + 1)?;
                                    l.checked_shl(*sh as u32)
                                }
                                InstKind::BinOp {
                                    op: BinOpKind::Add,
                                    lhs,
                                    rhs,
                                    ..
                                } => {
                                    let l = val_upper_bound(func, idom, &bb.label, lhs, depth + 1)?;
                                    let r = val_upper_bound(func, idom, &bb.label, rhs, depth + 1)?;
                                    l.checked_add(r)
                                }
                                InstKind::Cast {
                                    op: CastKind::ZExt,
                                    val,
                                    ..
                                } => val_upper_bound(func, idom, &bb.label, val, depth + 1),
                                _ => None,
                            };
                        }
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn ptr_upper_bound(
        module: &Module,
        func: &Function,
        idom: &BTreeMap<String, Option<String>>,
        block_label: &str,
        ptr: &ValueRef,
        param_idx: usize,
        depth: usize,
    ) -> Option<u64> {
        if depth > 12 {
            return None;
        }
        let ValueRef::Local(pname) = ptr else {
            return None;
        };
        if func.params.get(param_idx).is_some_and(|p| &p.name == pname) {
            return Some(0);
        }
        for bb in &func.blocks {
            for inst in &bb.insts {
                if inst.result.as_deref() == Some(pname.as_str())
                    && let InstKind::Gep {
                        base_ty,
                        ptr: base_ptr,
                        indices,
                        ..
                    } = &inst.kind
                {
                    let base_max = ptr_upper_bound(
                        module,
                        func,
                        idom,
                        &bb.label,
                        base_ptr,
                        param_idx,
                        depth + 1,
                    )?;
                    if indices.len() != 1 {
                        return None;
                    }
                    let stride = module.sizeof(base_ty).ok()?;
                    let idx_max =
                        val_upper_bound(func, idom, block_label, &indices[0].1, depth + 1)?;
                    return base_max.checked_add(idx_max.checked_mul(stride)?);
                }
            }
        }
        None
    }

    let mut max_end = 0u64;
    for bb in &func.blocks {
        for inst in &bb.insts {
            let (ty, ptr) = match &inst.kind {
                InstKind::Load { ty, ptr, .. } => (ty, ptr),
                InstKind::Store { ty, ptr, .. } => (ty, ptr),
                _ => continue,
            };
            let ValueRef::Local(pname) = ptr else {
                continue;
            };
            if ptr_info
                .get(pname)
                .is_some_and(|(b, _)| *b == PtrBase::Param(param_idx))
            {
                let bytes = u64::from(ty.bit_width()?.div_ceil(8));
                let off_max = ptr_upper_bound(module, func, &idom, &bb.label, ptr, param_idx, 0)?;
                max_end = max_end.max(off_max.checked_add(bytes)?);
            }
        }
    }
    Some(max_end)
}

fn find_paired_len_param(
    func: &Function,
    ptr_idx: usize,
    already_taken: &BTreeMap<usize, usize>,
) -> Option<usize> {
    // Prefer the parameter right after `ptr_idx` if it's i64/i32 and compared in loops/branches
    // (and not used as a byte index into `ptr_idx` via GEP directly when another param exists).
    let cand = ptr_idx + 1;
    if cand < func.params.len()
        && !already_taken.contains_key(&cand)
        && matches!(func.params[cand].ty, LlvmType::Int(64) | LlvmType::Int(32))
    {
        let pname = &func.params[cand].name;
        // Check if `pname` is used in an `icmp` or `phi` (length/trip bound) rather than solely
        // as a direct GEP index into `ptr_idx`.
        let mut used_in_icmp_or_phi = false;
        let mut used_directly_as_gep_idx = false;
        for bb in &func.blocks {
            for inst in &bb.insts {
                match &inst.kind {
                    InstKind::ICmp { lhs, rhs, .. } => {
                        if *lhs == ValueRef::Local(pname.clone())
                            || *rhs == ValueRef::Local(pname.clone())
                        {
                            used_in_icmp_or_phi = true;
                        }
                    }
                    InstKind::Phi { incoming, .. } => {
                        if incoming
                            .iter()
                            .any(|(v, _)| *v == ValueRef::Local(pname.clone()))
                        {
                            used_in_icmp_or_phi = true;
                        }
                    }
                    InstKind::Gep { indices, .. } => {
                        if indices
                            .iter()
                            .any(|(_, v)| *v == ValueRef::Local(pname.clone()))
                        {
                            used_directly_as_gep_idx = true;
                        }
                    }
                    _ => {}
                }
            }
        }
        if used_in_icmp_or_phi && !used_directly_as_gep_idx {
            return Some(cand);
        }
    }
    None
}

fn is_param_used_signed(func: &Function, name: &str) -> bool {
    let target = ValueRef::Local(name.to_string());
    for bb in &func.blocks {
        for inst in &bb.insts {
            match &inst.kind {
                InstKind::BinOp {
                    op: BinOpKind::SDiv | BinOpKind::SRem | BinOpKind::AShr,
                    lhs,
                    rhs,
                    ..
                } if *lhs == target || *rhs == target => return true,
                InstKind::BinOp {
                    op: BinOpKind::Add | BinOpKind::Sub | BinOpKind::Mul,
                    nsw: true,
                    lhs,
                    rhs,
                    ..
                } if *lhs == target || *rhs == target => {
                    // Only treat as signed if not also masked/unsigned-compared.
                    return !used_unsigned(func, name);
                }
                InstKind::ICmp {
                    pred: ICmpPred::SGt | ICmpPred::SGe | ICmpPred::SLt | ICmpPred::SLe,
                    lhs,
                    rhs,
                    ..
                } if *lhs == target || *rhs == target => return true,
                _ => {}
            }
        }
    }
    false
}

fn used_unsigned(func: &Function, name: &str) -> bool {
    let target = ValueRef::Local(name.to_string());
    for bb in &func.blocks {
        for inst in &bb.insts {
            match &inst.kind {
                InstKind::BinOp {
                    op: BinOpKind::UDiv | BinOpKind::URem | BinOpKind::LShr,
                    lhs,
                    rhs,
                    ..
                } if *lhs == target || *rhs == target => return true,
                InstKind::ICmp {
                    pred: ICmpPred::UGt | ICmpPred::UGe | ICmpPred::ULt | ICmpPred::ULe,
                    lhs,
                    rhs,
                    ..
                } if *lhs == target || *rhs == target => return true,
                _ => {}
            }
        }
    }
    false
}

/// Conservative unsigned/signed interval analysis across the function, used to prove `nsw`,
/// constant/bounded shifts, divisions, and GEP bounds safe without spurious `Result` wrappers.
pub fn compute_intervals(func: &Function, param_kinds: &[ParamKind]) -> BTreeMap<String, Interval> {
    let mut env: BTreeMap<String, Interval> = BTreeMap::new();
    for (p, kind) in func.params.iter().zip(param_kinds) {
        if let Some(bits) = p.ty.bit_width() {
            let mut iv = Interval::full(bits);
            if matches!(kind, ParamKind::SliceLen { .. }) {
                // Realistic slice lengths fit well below i64::MAX / 2.
                iv.umax = (1u64 << 48) - 1;
                iv.smin = 0;
                iv.smax = iv.umax as i64;
            }
            env.insert(p.name.clone(), iv);
        }
    }

    let val_iv = |v: &ValueRef, ty: &LlvmType, env: &BTreeMap<String, Interval>| -> Interval {
        let bits = ty.bit_width().unwrap_or(64);
        match v {
            ValueRef::ConstInt { value, .. } => Interval::constant(*value, bits),
            ValueRef::ConstBool(b) => Interval::constant(i128::from(*b), 1),
            ValueRef::Local(n) => env.get(n).copied().unwrap_or_else(|| Interval::full(bits)),
            _ => Interval::full(bits),
        }
    };

    // Detect loop header IV bounds (`0 <= iv < N` or `1 <= iv <= M`) from loop patterns.
    let mut loop_iv_bounds: BTreeMap<String, Interval> = BTreeMap::new();
    for bb in &func.blocks {
        if let Terminator::CondBr {
            cond: ValueRef::Local(cname),
            then_bb,
            else_bb,
        } = &bb.term
        {
            for inst in &bb.insts {
                if inst.result.as_deref() == Some(cname.as_str())
                    && let InstKind::ICmp {
                        pred,
                        ty,
                        lhs: ValueRef::Local(l),
                        rhs: ValueRef::ConstInt { value: bound, .. },
                        ..
                    } = &inst.kind
                    && let Some(bits) = ty.bit_width()
                {
                    // If `l` is `add %iv, 1` or `add %iv, -1` inside the same block:
                    for pinst in &bb.insts {
                        if let InstKind::Phi { incoming, .. } = &pinst.kind
                            && let Some(phi_name) = &pinst.result
                            && incoming
                                .iter()
                                .any(|(v, _)| *v == ValueRef::Local(l.clone()))
                        {
                            // Up-counter `iv_next = iv + 1`, `icmp eq iv_next, bound` -> `0 <= iv < bound`.
                            if *pred == ICmpPred::Eq
                                && *then_bb != bb.label
                                && *else_bb == bb.label
                                && *bound > 0
                            {
                                loop_iv_bounds.insert(
                                    phi_name.clone(),
                                    Interval {
                                        umin: 0,
                                        umax: (*bound as u64) - 1,
                                        smin: 0,
                                        smax: (*bound as i64) - 1,
                                    },
                                );
                            }
                        }
                    }
                    let _ = bits;
                }
            }
        }
    }

    for _ in 0..8 {
        let mut changed = false;
        for bb in &func.blocks {
            for inst in &bb.insts {
                let Some(res) = &inst.result else { continue };
                let computed = match &inst.kind {
                    InstKind::Phi { ty, incoming } => {
                        let bits = ty.bit_width().unwrap_or(64);
                        if let Some(bound_iv) = loop_iv_bounds.get(res) {
                            *bound_iv
                        } else {
                            let mut acc: Option<Interval> = None;
                            for (v, _) in incoming {
                                let viv = match v {
                                    ValueRef::Local(n) => {
                                        if n == res {
                                            continue;
                                        }
                                        env.get(n).copied()
                                    }
                                    ValueRef::ConstInt { value, .. } => {
                                        Some(Interval::constant(*value, bits))
                                    }
                                    _ => Some(Interval::full(bits)),
                                };
                                if let Some(iv) = viv {
                                    acc = Some(match acc {
                                        None => iv,
                                        Some(prev) => prev.join(iv),
                                    });
                                }
                            }
                            let mut res_iv = acc.unwrap_or_else(|| Interval::full(bits));
                            // Check if any incoming is a down-counter `add iv, -step` (non-negative).
                            if incoming.iter().any(|(v, pred)| {
                                *pred == bb.label
                                    && matches!(v, ValueRef::Local(_))
                                    && res_iv.smin >= 0
                            }) {
                                // Conservative: if initialized >= 0 and only decrements until exit, keep >= 0.
                                res_iv.smin = res_iv.smin.min(0);
                            }
                            res_iv
                        }
                    }
                    InstKind::Cast {
                        op: CastKind::ZExt,
                        from_ty,
                        val,
                        to_ty,
                        ..
                    } => {
                        let viv = val_iv(val, from_ty, &env);
                        let tbits = to_ty.bit_width().unwrap_or(64);
                        if tbits >= 64 {
                            Interval {
                                umin: viv.umin,
                                umax: viv.umax,
                                smin: viv.umin.min(i64::MAX as u64) as i64,
                                smax: viv.umax.min(i64::MAX as u64) as i64,
                            }
                        } else {
                            Interval {
                                umin: viv.umin,
                                umax: viv.umax,
                                smin: viv.umin as i64,
                                smax: viv.umax as i64,
                            }
                        }
                    }
                    InstKind::BinOp {
                        op, ty, lhs, rhs, ..
                    } => {
                        let bits = ty.bit_width().unwrap_or(64);
                        let l = val_iv(lhs, ty, &env);
                        let r = val_iv(rhs, ty, &env);
                        match op {
                            BinOpKind::And => {
                                let umax = l.umax.min(r.umax);
                                let smax = if umax <= i64::MAX as u64 {
                                    umax as i64
                                } else {
                                    i64::MAX
                                };
                                let smin = if umax <= i64::MAX as u64 { 0 } else { i64::MIN };
                                Interval {
                                    umin: 0,
                                    umax,
                                    smin,
                                    smax,
                                }
                            }
                            BinOpKind::LShr => {
                                if r.umin == r.umax && r.umin < u64::from(bits) {
                                    let sh = r.umin as u32;
                                    let umin = l.umin >> sh;
                                    let umax = l.umax >> sh;
                                    let (smin, smax) = if umax <= i64::MAX as u64 {
                                        (umin as i64, umax as i64)
                                    } else {
                                        (i64::MIN, i64::MAX)
                                    };
                                    Interval {
                                        umin,
                                        umax,
                                        smin,
                                        smax,
                                    }
                                } else {
                                    Interval::full(bits)
                                }
                            }
                            BinOpKind::Shl => {
                                if r.umin == r.umax && r.umin < 32 && l.umax < (1u64 << 30) {
                                    let sh = r.umin as u32;
                                    let mask = if bits >= 64 {
                                        u64::MAX
                                    } else {
                                        (1u64 << bits) - 1
                                    };
                                    if (l.umax << sh) <= mask && (l.umax << sh) <= i64::MAX as u64 {
                                        let umin = l.umin << sh;
                                        let umax = l.umax << sh;
                                        Interval {
                                            umin,
                                            umax,
                                            smin: umin as i64,
                                            smax: umax as i64,
                                        }
                                    } else {
                                        Interval::full(bits)
                                    }
                                } else {
                                    Interval::full(bits)
                                }
                            }
                            BinOpKind::URem => {
                                if r.umax > 0 {
                                    let umax = (r.umax - 1).min(l.umax);
                                    Interval {
                                        umin: 0,
                                        umax,
                                        smin: 0,
                                        smax: umax.min(i64::MAX as u64) as i64,
                                    }
                                } else {
                                    Interval::full(bits)
                                }
                            }
                            BinOpKind::Add => {
                                if r.smin == r.smax
                                    && r.smin < 0
                                    && r.smin > -(1i64 << 30)
                                    && l.smin >= 0
                                    && l.umax <= (1u64 << 48)
                                {
                                    let step = (-r.smin) as u64;
                                    if l.umin >= step
                                        || is_guarded_above(func, &bb.label, lhs, step)
                                    {
                                        let umin = l.umin.saturating_sub(step);
                                        let umax = l.umax.saturating_sub(step);
                                        Interval {
                                            umin,
                                            umax,
                                            smin: umin as i64,
                                            smax: umax as i64,
                                        }
                                    } else {
                                        // Down-counter `rem - step` inside its own loop block where
                                        // the loop only continues while `rem_next >= step` or exits at 0:
                                        Interval {
                                            umin: 0,
                                            umax: l.umax,
                                            smin: 0,
                                            smax: l.smax,
                                        }
                                    }
                                } else {
                                    let sum_smin = l.smin.checked_add(r.smin);
                                    let sum_smax = l.smax.checked_add(r.smax);
                                    let sum_umin = l.umin.checked_add(r.umin);
                                    let sum_umax = l.umax.checked_add(r.umax);
                                    let type_smax = if bits >= 64 {
                                        i64::MAX
                                    } else {
                                        ((1i64) << (bits - 1)) - 1
                                    };
                                    let type_smin = if bits >= 64 {
                                        i64::MIN
                                    } else {
                                        -((1i64) << (bits - 1))
                                    };
                                    let type_umax = if bits >= 64 {
                                        u64::MAX
                                    } else {
                                        (1u64 << bits) - 1
                                    };
                                    if let (Some(sn), Some(sx), Some(un), Some(ux)) =
                                        (sum_smin, sum_smax, sum_umin, sum_umax)
                                        && sn >= type_smin
                                        && sx <= type_smax
                                        && ux <= type_umax
                                    {
                                        Interval {
                                            umin: un,
                                            umax: ux,
                                            smin: sn,
                                            smax: sx,
                                        }
                                    } else {
                                        Interval::full(bits)
                                    }
                                }
                            }
                            _ => Interval::full(bits),
                        }
                    }
                    InstKind::Select {
                        ty,
                        then_val,
                        else_val,
                        ..
                    } => val_iv(then_val, ty, &env).join(val_iv(else_val, ty, &env)),
                    _ => continue,
                };
                if env.get(res) != Some(&computed) {
                    env.insert(res.clone(), computed);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    env
}

/// Check if an `nsw` BinOp can provably never signed-overflow under `intervals` and loop guards.
pub fn is_nsw_proven_safe(
    func: &Function,
    block_label: &str,
    op: BinOpKind,
    bits: u32,
    lhs: &ValueRef,
    rhs: &ValueRef,
    intervals: &BTreeMap<String, Interval>,
) -> bool {
    let get_iv = |v: &ValueRef| -> Interval {
        match v {
            ValueRef::ConstInt { value, .. } => Interval::constant(*value, bits),
            ValueRef::Local(n) => intervals
                .get(n)
                .copied()
                .unwrap_or_else(|| Interval::full(bits)),
            _ => Interval::full(bits),
        }
    };
    let l = get_iv(lhs);
    let r = get_iv(rhs);
    let type_smin = if bits >= 64 {
        i128::from(i64::MIN)
    } else {
        -(1i128 << (bits - 1))
    };
    let type_smax = if bits >= 64 {
        i128::from(i64::MAX)
    } else {
        (1i128 << (bits - 1)) - 1
    };

    match op {
        BinOpKind::Add => {
            let lo = i128::from(l.smin) + i128::from(r.smin);
            let hi = i128::from(l.smax) + i128::from(r.smax);
            if lo >= type_smin && hi <= type_smax {
                return true;
            }
            // Down-counting induction variable `iv + (-k)` in a loop block where iv starts >= k
            // and loop continues only while iv_next >= k (or exits when == 0):
            if let (ValueRef::Local(iv_name), ValueRef::ConstInt { value: step, .. }) = (lhs, rhs)
                && *step < 0
                && is_nonneg_down_counter(func, block_label, iv_name, (-*step) as u64, intervals)
            {
                return true;
            }
            // Constant subtraction `x + (-C)` guarded by `x > thresh` in dominating branch
            // (e.g., `adler32_z`: `if (sum2 > 65520) sum2 - 65521`).
            if let ValueRef::ConstInt { value: c, .. } = rhs
                && *c < 0
                && l.smin >= 0
                && i128::from(l.smax) <= type_smax
            {
                return i128::from(l.smin) + *c >= 0
                    || is_guarded_above(func, block_label, lhs, (-*c) as u64);
            }
            false
        }
        BinOpKind::Sub => {
            let lo = i128::from(l.smin) - i128::from(r.smax);
            let hi = i128::from(l.smax) - i128::from(r.smin);
            lo >= type_smin && hi <= type_smax
        }
        BinOpKind::Mul => {
            let prods = [
                i128::from(l.smin) * i128::from(r.smin),
                i128::from(l.smin) * i128::from(r.smax),
                i128::from(l.smax) * i128::from(r.smin),
                i128::from(l.smax) * i128::from(r.smax),
            ];
            let lo = *prods.iter().min().unwrap();
            let hi = *prods.iter().max().unwrap();
            lo >= type_smin && hi <= type_smax
        }
        _ => true,
    }
}

fn is_nonneg_down_counter(
    func: &Function,
    block_label: &str,
    iv_name: &str,
    step: u64,
    intervals: &BTreeMap<String, Interval>,
) -> bool {
    let Some(bb) = func.blocks.iter().find(|b| b.label == block_label) else {
        return false;
    };
    for inst in &bb.insts {
        if inst.result.as_deref() == Some(iv_name)
            && let InstKind::Phi { incoming, .. } = &inst.kind
        {
            // Check every outside incoming value is >= step (or bounded small non-negative).
            for (v, pred) in incoming {
                if *pred == block_label {
                    continue;
                }
                match v {
                    ValueRef::ConstInt { value, .. } if *value >= i128::from(step) => {}
                    ValueRef::Local(n) => {
                        if let Some(iv) = intervals.get(n) {
                            if iv.smin < 0 {
                                return false;
                            }
                            // If `n` is known <= 2^48 (e.g. slice length) and step == 1 or guarded >= step:
                            if step == 1 && iv.umin >= 1 {
                                continue;
                            }
                            if iv.umin >= step {
                                continue;
                            }
                            // Also check if an icmp in predecessor ensures entry >= step (or != 0 for step == 1).
                            if is_guarded_above(func, block_label, v, step) {
                                continue;
                            }
                        }
                        return false;
                    }
                    _ => return false,
                }
            }
            return true;
        }
    }
    false
}

fn is_guarded_above(func: &Function, block_label: &str, val: &ValueRef, min_val: u64) -> bool {
    let idom = compute_dominators(func);
    for bb in &func.blocks {
        if !dominates(&idom, &bb.label, block_label) || bb.label == block_label {
            continue;
        }
        if let Terminator::CondBr {
            cond: ValueRef::Local(cname),
            then_bb,
            else_bb,
        } = &bb.term
        {
            for inst in &bb.insts {
                if inst.result.as_deref() == Some(cname.as_str())
                    && let InstKind::ICmp {
                        pred,
                        lhs,
                        rhs: ValueRef::ConstInt { value: c, .. },
                        ..
                    } = &inst.kind
                    && lhs == val
                {
                    let c = *c as u64;
                    let then_dom = dominates(&idom, then_bb, block_label);
                    let else_dom = dominates(&idom, else_bb, block_label);
                    if then_dom {
                        match pred {
                            ICmpPred::UGt if c + 1 >= min_val => return true,
                            ICmpPred::UGe if c >= min_val => return true,
                            ICmpPred::Ne if c == 0 && min_val == 1 => return true,
                            _ => {}
                        }
                    }
                    if else_dom {
                        match pred {
                            ICmpPred::Eq if c == 0 && min_val == 1 => return true,
                            ICmpPred::ULt if c >= min_val => return true,
                            ICmpPred::ULe if c + 1 >= min_val => return true,
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    false
}
