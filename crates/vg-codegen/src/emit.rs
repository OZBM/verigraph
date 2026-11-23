//! Textual LLVM IR emission for all MVP opcodes, checked entries, and the stdin/stdout runner.

use std::collections::BTreeMap;

use vg_core::diag::{Diagnostic, Span};
use vg_core::ir::{Builtin, Hash, Node, Op, Operand, Term};
use vg_core::types::{Handle, Type};

use crate::Options;
use crate::analysis::{
    NodeAnalysis, OP_ASSERT_GUARD, OP_CALL_HASH, OP_FOLD_BOUNDED, OP_GENERATE, OP_SIMD_MAP,
    analyze_graph, is_const_bool,
};
use crate::formula::{FormulaCx, emit_arena_bytes_fn, eval_bool_term};
use crate::ir::{
    FnBuilder, IrVal, ModBuilder, emit_branch_trap_if_false, emit_canon_float, emit_const_val,
    emit_gep_bytes, emit_load_scalar, emit_memcpy, emit_memcpy_dyn, emit_slice_parts,
    emit_store_scalar_canon, emit_store_slice, emit_store_value_canon, f32_const_expr,
    f64_const_expr, needs_input_validation,
};
use crate::layout::{
    CallGraph, align_of, collect_graph, docs13_fixed_byte_size, is_erased, is_scalar_abi,
    llvm_type, size_of, tensor_elem_count,
};

pub struct EmittedModule {
    pub ir: String,
    pub residual_guards: u32,
}

pub fn emit_module(
    root: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    opts: &Options,
) -> Result<EmittedModule, Diagnostic> {
    let graph = collect_graph(root, callee)?;
    let (analyses, guards) = analyze_graph(&graph, opts.target)?;
    let mut m = ModBuilder::new(opts.target, opts.proof_facts, opts.runtime_checks);
    m.require_vg_rt_core();

    for (h, node) in &graph.order {
        let prefix = &graph.prefixes[h];
        let an = &analyses[h];
        emit_internal_node(&mut m, &graph, h, prefix, node, an)?;
    }

    emit_arena_bytes_fn(&mut m, &graph, &analyses)?;
    emit_checked_entry(&mut m, &graph, &analyses)?;
    emit_main_runner(&mut m, &graph)?;

    Ok(EmittedModule {
        ir: m.render(),
        residual_guards: guards.len() as u32,
    })
}

fn operand_val(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    node: &Node,
    o: Operand,
    inputs: &[IrVal],
    op_vals: &[Vec<IrVal>],
) -> Result<IrVal, Diagnostic> {
    match o {
        Operand::Input(i) => Ok(inputs[i as usize].clone()),
        Operand::Op(i, s) => Ok(op_vals[i as usize][s as usize].clone()),
        Operand::Const(c) => {
            let k = &node.consts[c as usize];
            emit_const_val(f, m, &k.ty, &k.bytes)
        }
        Operand::Callee(_) | Operand::Invariant(_) => Err(Diagnostic::error(
            "VG-S0203",
            Span::default(),
            "expected value operand, found callee/invariant",
        )),
    }
}

fn pre_term_is_simple_assume(t: &Term) -> bool {
    match t {
        Term::Input(_) | Term::Const(_) => true,
        Term::Not(a) | Term::Neg(a) => pre_term_is_simple_assume(a),
        Term::Bin(_, a, b) => pre_term_is_simple_assume(a) && pre_term_is_simple_assume(b),
        Term::Builtin(
            Builtin::IsFinite | Builtin::IsNan | Builtin::Abs | Builtin::Min | Builtin::Max,
            args,
        ) => args.iter().all(pre_term_is_simple_assume),
        _ => false,
    }
}

fn emit_internal_node(
    m: &mut ModBuilder,
    graph: &CallGraph,
    node_hash: &Hash,
    prefix: &str,
    node: &Node,
    an: &NodeAnalysis,
) -> Result<(), Diagnostic> {
    let mut sig_params = vec!["ptr %ctx".to_string()];
    let mut inputs: Vec<IrVal> = vec![
        IrVal {
            ty: Type::Eff,
            ssa: String::new(),
        };
        node.input_count()
    ];

    for (i, ty) in node
        .params
        .iter()
        .chain(node.learnable.iter().map(|l| &l.ty))
        .enumerate()
    {
        if is_erased(ty) {
            continue;
        }
        let name = format!("%in{i}");
        if is_scalar_abi(ty) {
            sig_params.push(format!("{} {name}", llvm_type(ty, m.target)?));
        } else {
            sig_params.push(format!("ptr readonly {name}"));
        }
        inputs[i] = IrVal {
            ty: ty.clone(),
            ssa: name,
        };
    }

    let mut out_ptrs = Vec::new();
    let mut out_tys = Vec::new();
    for (j, ty) in node.results.iter().enumerate() {
        if is_erased(ty) {
            continue;
        }
        let name = format!("%out{j}");
        let noalias = if m.proof_facts { " noalias" } else { "" };
        sig_params.push(format!("ptr{noalias} {name}"));
        out_ptrs.push(name);
        out_tys.push(ty.clone());
    }

    let mut f = FnBuilder::new();
    let ctx_ptr = "%ctx";

    // Allocate scratch block at start of arena when needed (`docs/06` line 9).
    let scratch_base = if an.local_scratch_bytes > 0 {
        let s = f.reg();
        f.emit(format!(
            "{s} = call ptr @vg_arena_alloc(ptr {ctx_ptr}, i64 {})",
            an.local_scratch_bytes
        ));
        Some(s)
    } else {
        None
    };

    // Attach llvm.assume for simple loop-free scalar PRE conjuncts when proof_facts enabled and
    // runtime_checks disabled (proven node).
    if m.proof_facts && !m.runtime_checks && !node.contract.pre.is_empty() {
        let mut fcx = FormulaCx {
            node,
            graph,
            ctx_ptr,
            inputs: &inputs,
            bound: Vec::new(),
        };
        for t in &node.contract.pre {
            if pre_term_is_simple_assume(t) {
                let cond = eval_bool_term(&mut f, m, &mut fcx, t)?;
                m.require_decl("declare void @llvm.assume(i1)");
                f.emit(format!("call void @llvm.assume(i1 {cond})"));
            }
        }
    }

    let mut op_vals: Vec<Vec<IrVal>> = Vec::with_capacity(node.ops.len());
    for (i, op) in node.ops.iter().enumerate() {
        let res = emit_op(
            &mut f,
            m,
            graph,
            prefix,
            node_hash,
            node,
            an,
            i,
            op,
            ctx_ptr,
            scratch_base.as_deref(),
            &inputs,
            &op_vals,
        )?;
        op_vals.push(res);
    }

    // Store outputs (canonicalizing NaN on every float store/output, `docs/06`).
    let non_eff_outputs: Vec<(&Type, &Operand)> = node
        .results
        .iter()
        .zip(&node.outputs)
        .filter(|(t, _)| !is_erased(t))
        .collect();
    for ((ty, opnd), out_ptr) in non_eff_outputs.into_iter().zip(&out_ptrs) {
        let val = operand_val(&mut f, m, node, *opnd, &inputs, &op_vals)?;
        emit_store_value_canon(&mut f, m, ty, &val, out_ptr)?;
    }

    f.ret_void();
    let header = format!("define void @vgi_{prefix}({})", sig_params.join(", "));
    m.fns.push(f.finish(&header));
    Ok(())
}

fn alloc_result_buf(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    an: &NodeAnalysis,
    op_idx: usize,
    slot: usize,
    ty: &Type,
    scratch_base: Option<&str>,
) -> Result<String, Diagnostic> {
    if let Some(off) = an.scratch_offsets.get(&(op_idx, slot))
        && let Some(base) = scratch_base
    {
        if *off == 0 {
            return Ok(base.to_string());
        }
        let r = f.reg();
        f.emit(format!("{r} = getelementptr i8, ptr {base}, i64 {off}"));
        return Ok(r);
    }
    let lty = llvm_type(ty, m.target)?;
    let align = align_of(ty, m.target)?;
    Ok(f.alloca(&lty, align))
}

#[allow(clippy::too_many_arguments)]
fn emit_op(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    graph: &CallGraph,
    prefix: &str,
    _node_hash: &Hash,
    node: &Node,
    an: &NodeAnalysis,
    op_idx: usize,
    op: &Op,
    ctx_ptr: &str,
    scratch_base: Option<&str>,
    inputs: &[IrVal],
    op_vals: &[Vec<IrVal>],
) -> Result<Vec<IrVal>, Diagnostic> {
    let ob = |name: &str| format!("@{prefix}/%{op_idx}/{name}");
    let one = |ty: Type, ssa: String| Ok(vec![IrVal { ty, ssa }]);

    match op.opcode {
        0x0003 => {
            // TUPLE
            let ty = op.results[0].clone();
            let lty = llvm_type(&ty, m.target)?;
            let buf = alloc_result_buf(f, m, an, op_idx, 0, &ty, scratch_base)?;
            for (idx, &o) in op.operands.iter().enumerate() {
                let v = operand_val(f, m, node, o, inputs, op_vals)?;
                let fp = f.reg();
                f.emit(format!(
                    "{fp} = getelementptr inbounds {lty}, ptr {buf}, i32 0, i32 {idx}"
                ));
                emit_store_value_canon(f, m, &v.ty.clone(), &v, &fp)?;
            }
            one(ty, buf)
        }
        0x0004 => {
            // PROJ(t, j)
            let tv = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
            let Operand::Const(ci) = op.operands[1] else {
                return Err(Diagnostic::error(
                    "VG-S0108",
                    Span::default(),
                    "PROJ index must be const",
                ));
            };
            let b = &node.consts[ci as usize].bytes;
            let j = u32::from_le_bytes(b[..4].try_into().unwrap_or([0; 4])) as usize;
            let lty = llvm_type(&tv.ty, m.target)?;
            let fp = f.reg();
            f.emit(format!(
                "{fp} = getelementptr inbounds {lty}, ptr {}, i32 0, i32 {j}",
                tv.ssa
            ));
            let res_ty = op.results[0].clone();
            if is_scalar_abi(&res_ty) {
                let s = emit_load_scalar(f, &res_ty, &fp, m.target)?;
                one(res_ty, s)
            } else {
                one(res_ty, fp)
            }
        }
        0x0005 => {
            // SELECT(c, a, b)
            let c = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
            let a = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            let b = operand_val(f, m, node, op.operands[2], inputs, op_vals)?;
            let cond = f.reg();
            f.emit(format!("{cond} = icmp ne i8 {}, 0", c.ssa));
            let res_ty = op.results[0].clone();
            let sel_ty = if is_scalar_abi(&res_ty) {
                llvm_type(&res_ty, m.target)?
            } else {
                "ptr".to_string()
            };
            let r = f.reg();
            f.emit(format!(
                "{r} = select i1 {cond}, {sel_ty} {}, {sel_ty} {}",
                a.ssa, b.ssa
            ));
            one(res_ty, r)
        }
        0x0006 => {
            // CAST
            let v = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
            let dst_ty = op.results[0].clone();
            let ssa = emit_cast(
                f,
                m,
                &v.ty,
                &dst_ty,
                &v.ssa,
                ctx_ptr,
                &ob("CAST.in_range"),
                prefix,
            )?;
            one(dst_ty, ssa)
        }
        0x0007 => {
            // AS_BYTES
            let v = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
            let slice_ty = op.results[0].clone();
            if is_scalar_abi(&v.ty) {
                let spill_buf = alloc_result_buf(f, m, an, op_idx, 0, &v.ty, scratch_base)?;
                let hdr_buf = f.alloca("{ ptr, i64 }", 8);
                emit_store_scalar_canon(f, &v.ty, &v.ssa, &spill_buf, m.target)?;
                let sz = docs13_fixed_byte_size(&v.ty)?.ok_or_else(|| {
                    Diagnostic::error("VG-T0114", Span::default(), "AS_BYTES invalid scalar")
                })?;
                emit_store_slice(f, &hdr_buf, &spill_buf, &format!("{sz}"));
                return one(slice_ty, hdr_buf);
            }
            let slice_buf = f.alloca("{ ptr, i64 }", 8);
            match &v.ty {
                Type::Slice(elem) => {
                    let (dp, len) = emit_slice_parts(f, &v.ssa);
                    let esz = docs13_fixed_byte_size(elem)?.ok_or_else(|| {
                        Diagnostic::error(
                            "VG-T0114",
                            Span::default(),
                            "AS_BYTES invalid slice elem",
                        )
                    })?;
                    let bytes = f.reg();
                    f.emit(format!("{bytes} = mul i64 {len}, {esz}"));
                    emit_store_slice(f, &slice_buf, &dp, &bytes);
                }
                _ => {
                    let sz = docs13_fixed_byte_size(&v.ty)?.ok_or_else(|| {
                        Diagnostic::error("VG-T0114", Span::default(), "AS_BYTES invalid type")
                    })?;
                    emit_store_slice(f, &slice_buf, &v.ssa, &format!("{sz}"));
                }
            }
            one(slice_ty, slice_buf)
        }
        0x0008 | 0x0009 => {
            // OK / ERR
            let v = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
            let res_ty = op.results[0].clone();
            let lty = llvm_type(&res_ty, m.target)?;
            let buf = alloc_result_buf(f, m, an, op_idx, 0, &res_ty, scratch_base)?;
            let tp = f.reg();
            f.emit(format!(
                "{tp} = getelementptr inbounds {lty}, ptr {buf}, i32 0, i32 0"
            ));
            let tag = if op.opcode == 0x0008 { 1 } else { 0 };
            f.emit(format!("store i8 {tag}, ptr {tp}, align 1"));
            let pp = f.reg();
            f.emit(format!(
                "{pp} = getelementptr inbounds {lty}, ptr {buf}, i32 0, i32 1"
            ));
            emit_store_value_canon(f, m, &v.ty.clone(), &v, &pp)?;
            one(res_ty, buf)
        }
        0x000A => {
            // IS_OK
            let r = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
            let lty = llvm_type(&r.ty, m.target)?;
            let tp = f.reg();
            f.emit(format!(
                "{tp} = getelementptr inbounds {lty}, ptr {}, i32 0, i32 0",
                r.ssa
            ));
            let tag = f.reg();
            f.emit(format!("{tag} = load i8, ptr {tp}, align 1"));
            let b = f.reg();
            f.emit(format!("{b} = icmp ne i8 {tag}, 0"));
            let out = f.reg();
            f.emit(format!("{out} = zext i1 {b} to i8"));
            one(Type::Bool, out)
        }
        0x000B | 0x000C => {
            // GET_OK / GET_ERR
            let r = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
            let lty = llvm_type(&r.ty, m.target)?;
            if m.runtime_checks {
                let tp = f.reg();
                f.emit(format!(
                    "{tp} = getelementptr inbounds {lty}, ptr {}, i32 0, i32 0",
                    r.ssa
                ));
                let tag = f.reg();
                f.emit(format!("{tag} = load i8, ptr {tp}, align 1"));
                let cond = f.reg();
                if op.opcode == 0x000B {
                    f.emit(format!("{cond} = icmp ne i8 {tag}, 0"));
                    emit_branch_trap_if_false(
                        f,
                        m,
                        &cond,
                        ctx_ptr,
                        "rt",
                        &ob("GET_OK.is_ok"),
                        prefix,
                    );
                } else {
                    f.emit(format!("{cond} = icmp eq i8 {tag}, 0"));
                    emit_branch_trap_if_false(
                        f,
                        m,
                        &cond,
                        ctx_ptr,
                        "rt",
                        &ob("GET_ERR.is_err"),
                        prefix,
                    );
                }
            }
            let pp = f.reg();
            f.emit(format!(
                "{pp} = getelementptr inbounds {lty}, ptr {}, i32 0, i32 1",
                r.ssa
            ));
            let out_ty = op.results[0].clone();
            if is_scalar_abi(&out_ty) {
                let s = emit_load_scalar(f, &out_ty, &pp, m.target)?;
                one(out_ty, s)
            } else {
                one(out_ty, pp)
            }
        }
        0x0010..=0x0015 => {
            let ssa = emit_int_arith(f, m, node, op_idx, op, ctx_ptr, prefix, inputs, op_vals)?;
            one(op.results[0].clone(), ssa)
        }
        0x0030..=0x0037 => {
            let ssa =
                emit_bitwise_and_shift(f, m, node, op_idx, op, ctx_ptr, prefix, inputs, op_vals)?;
            one(op.results[0].clone(), ssa)
        }
        0x0040..=0x004B => {
            let ssa = emit_float_op(f, m, node, op_idx, op, ctx_ptr, prefix, inputs, op_vals)?;
            one(op.results[0].clone(), ssa)
        }
        0x0050..=0x0058 => {
            let ssa = emit_cmp_and_logic(f, m, node, op, inputs, op_vals)?;
            one(Type::Bool, ssa)
        }
        0x0060 => {
            // INDEX(xs, i)
            let xs = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
            let idx = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            let (dp, len) = collection_ptr_len(f, &xs)?;
            if m.runtime_checks {
                let ok = f.reg();
                f.emit(format!("{ok} = icmp ult i64 {}, {len}", idx.ssa));
                emit_branch_trap_if_false(f, m, &ok, ctx_ptr, "rt", &ob("INDEX.in_bounds"), prefix);
            }
            let elem_ty = op.results[0].clone();
            let elem_lty = llvm_type(&elem_ty, m.target)?;
            let ep = emit_gep_bytes(f, m.target, m.proof_facts, &elem_lty, &dp, &idx.ssa);
            if is_scalar_abi(&elem_ty) {
                let s = emit_load_scalar(f, &elem_ty, &ep, m.target)?;
                one(elem_ty, s)
            } else {
                one(elem_ty, ep)
            }
        }
        0x0061 => {
            // UPDATE(xs, i, v)
            let xs = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
            let idx = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            let val = operand_val(f, m, node, op.operands[2], inputs, op_vals)?;
            let (src_dp, len) = collection_ptr_len(f, &xs)?;
            if m.runtime_checks {
                let ok = f.reg();
                f.emit(format!("{ok} = icmp ult i64 {}, {len}", idx.ssa));
                emit_branch_trap_if_false(
                    f,
                    m,
                    &ok,
                    ctx_ptr,
                    "rt",
                    &ob("UPDATE.in_bounds"),
                    prefix,
                );
            }
            let out_ty = op.results[0].clone();
            let elem_ty = out_ty.elem().expect("collection elem").clone();
            let elem_lty = llvm_type(&elem_ty, m.target)?;
            let elem_sz = size_of(&elem_ty, m.target)?;
            let out_buf = alloc_result_buf(f, m, an, op_idx, 0, &out_ty, scratch_base)?;

            match &out_ty {
                Type::Array(_, n) => {
                    emit_memcpy(f, m, &out_buf, &src_dp, u64::from(*n) * elem_sz);
                    let ep =
                        emit_gep_bytes(f, m.target, m.proof_facts, &elem_lty, &out_buf, &idx.ssa);
                    emit_store_value_canon(f, m, &elem_ty, &val, &ep)?;
                    one(out_ty, out_buf)
                }
                Type::Slice(_) => {
                    let dst_dp = if an.inplace_update[op_idx] {
                        src_dp
                    } else {
                        let bytes = f.reg();
                        f.emit(format!("{bytes} = mul i64 {len}, {elem_sz}"));
                        let new_dp = f.reg();
                        f.emit(format!(
                            "{new_dp} = call ptr @vg_arena_alloc(ptr {ctx_ptr}, i64 {bytes})"
                        ));
                        emit_memcpy_dyn(f, m, &new_dp, &src_dp, &bytes);
                        new_dp
                    };
                    let ep =
                        emit_gep_bytes(f, m.target, m.proof_facts, &elem_lty, &dst_dp, &idx.ssa);
                    emit_store_value_canon(f, m, &elem_ty, &val, &ep)?;
                    emit_store_slice(f, &out_buf, &dst_dp, &len);
                    one(out_ty, out_buf)
                }
                _ => unreachable!(),
            }
        }
        0x0062 => {
            // LEN
            let xs = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
            let (_, len) = collection_ptr_len(f, &xs)?;
            one(Type::UInt(64), len)
        }
        0x0063 => {
            // SLICE(xs, lo, hi)
            let xs = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
            let lo = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            let hi = operand_val(f, m, node, op.operands[2], inputs, op_vals)?;
            let (dp, len) = collection_ptr_len(f, &xs)?;
            if m.runtime_checks {
                let c1 = f.reg();
                f.emit(format!("{c1} = icmp ule i64 {}, {}", lo.ssa, hi.ssa));
                let c2 = f.reg();
                f.emit(format!("{c2} = icmp ule i64 {}, {len}", hi.ssa));
                let ok = f.reg();
                f.emit(format!("{ok} = and i1 {c1}, {c2}"));
                emit_branch_trap_if_false(f, m, &ok, ctx_ptr, "rt", &ob("SLICE.in_bounds"), prefix);
            }
            let out_ty = op.results[0].clone();
            let elem_ty = out_ty.elem().expect("slice elem").clone();
            let elem_lty = llvm_type(&elem_ty, m.target)?;
            let new_dp = emit_gep_bytes(f, m.target, m.proof_facts, &elem_lty, &dp, &lo.ssa);
            let new_len = f.reg();
            f.emit(format!("{new_len} = sub i64 {}, {}", hi.ssa, lo.ssa));
            let out_buf = alloc_result_buf(f, m, an, op_idx, 0, &out_ty, scratch_base)?;
            emit_store_slice(f, &out_buf, &new_dp, &new_len);
            one(out_ty, out_buf)
        }
        0x0064 => {
            // CONCAT(a, b)
            let a = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
            let b = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            let (pa, la) = collection_ptr_len(f, &a)?;
            let (pb, lb) = collection_ptr_len(f, &b)?;
            let out_ty = op.results[0].clone();
            let elem_ty = out_ty.elem().expect("concat elem").clone();
            let elem_lty = llvm_type(&elem_ty, m.target)?;
            let elem_sz = size_of(&elem_ty, m.target)?;
            let out_buf = alloc_result_buf(f, m, an, op_idx, 0, &out_ty, scratch_base)?;

            match &out_ty {
                Type::Array(_, n) => {
                    let Type::Array(_, na) = a.ty else {
                        unreachable!()
                    };
                    let Type::Array(_, nb) = b.ty else {
                        unreachable!()
                    };
                    let _ = n;
                    emit_memcpy(f, m, &out_buf, &pa, u64::from(na) * elem_sz);
                    let mid =
                        emit_gep_bytes(f, m.target, true, &elem_lty, &out_buf, &format!("{na}"));
                    emit_memcpy(f, m, &mid, &pb, u64::from(nb) * elem_sz);
                    one(out_ty, out_buf)
                }
                Type::Slice(_) => {
                    m.require_decl("declare { i64, i1 } @llvm.uadd.with.overflow.i64(i64, i64)");
                    m.require_decl("declare { i64, i1 } @llvm.umul.with.overflow.i64(i64, i64)");
                    let total = if m.runtime_checks {
                        let ov = f.reg();
                        f.emit(format!(
                            "{ov} = call {{ i64, i1 }} @llvm.uadd.with.overflow.i64(i64 {la}, i64 {lb})"
                        ));
                        let sum = f.reg();
                        f.emit(format!("{sum} = extractvalue {{ i64, i1 }} {ov}, 0"));
                        let flag = f.reg();
                        f.emit(format!("{flag} = extractvalue {{ i64, i1 }} {ov}, 1"));
                        let ok = f.reg();
                        f.emit(format!("{ok} = xor i1 {flag}, true"));
                        emit_branch_trap_if_false(
                            f,
                            m,
                            &ok,
                            ctx_ptr,
                            "rt",
                            &ob("CONCAT.length"),
                            prefix,
                        );
                        sum
                    } else {
                        let sum = f.reg();
                        let nuw = if m.proof_facts { " nuw" } else { "" };
                        f.emit(format!("{sum} = add{nuw} i64 {la}, {lb}"));
                        sum
                    };
                    let bytes_a = f.reg();
                    f.emit(format!("{bytes_a} = mul i64 {la}, {elem_sz}"));
                    let bytes_b = f.reg();
                    f.emit(format!("{bytes_b} = mul i64 {lb}, {elem_sz}"));
                    let total_bytes = f.reg();
                    f.emit(format!("{total_bytes} = mul i64 {total}, {elem_sz}"));
                    let dst_dp = f.reg();
                    f.emit(format!(
                        "{dst_dp} = call ptr @vg_arena_alloc(ptr {ctx_ptr}, i64 {total_bytes})"
                    ));
                    emit_memcpy_dyn(f, m, &dst_dp, &pa, &bytes_a);
                    let mid = emit_gep_bytes(f, m.target, true, &elem_lty, &dst_dp, &la);
                    emit_memcpy_dyn(f, m, &mid, &pb, &bytes_b);
                    emit_store_slice(f, &out_buf, &dst_dp, &total);
                    one(out_ty, out_buf)
                }
                _ => unreachable!(),
            }
        }
        OP_SIMD_MAP => emit_simd_map(
            f,
            m,
            graph,
            node,
            an,
            op_idx,
            op,
            ctx_ptr,
            scratch_base,
            inputs,
            op_vals,
        ),
        OP_FOLD_BOUNDED => emit_fold_bounded(
            f,
            m,
            graph,
            node,
            an,
            op_idx,
            op,
            ctx_ptr,
            scratch_base,
            inputs,
            op_vals,
        ),
        OP_GENERATE => emit_generate(
            f,
            m,
            graph,
            node,
            an,
            op_idx,
            op,
            ctx_ptr,
            scratch_base,
            inputs,
            op_vals,
        ),
        OP_ASSERT_GUARD => emit_assert_guard(
            f,
            m,
            graph,
            prefix,
            node,
            an,
            op_idx,
            op,
            ctx_ptr,
            scratch_base,
            inputs,
            op_vals,
        ),
        OP_CALL_HASH => emit_call_hash(
            f,
            m,
            graph,
            node,
            an,
            op_idx,
            op,
            ctx_ptr,
            scratch_base,
            inputs,
            op_vals,
        ),
        0x0400 => {
            // FILE_READ(e, h, max) -> (Eff, Result<Slice<u8>, u32>)
            m.require_decl("declare void @vg_rt_file_read(ptr, i32, i64, ptr)");
            let h = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            let max_b = operand_val(f, m, node, op.operands[2], inputs, op_vals)?;
            let res_ty = op.results[1].clone();
            let buf = alloc_result_buf(f, m, an, op_idx, 1, &res_ty, scratch_base)?;
            f.emit(format!(
                "call void @vg_rt_file_read(ptr {ctx_ptr}, i32 {}, i64 {}, ptr {buf})",
                h.ssa, max_b.ssa
            ));
            Ok(vec![
                IrVal {
                    ty: Type::Eff,
                    ssa: String::new(),
                },
                IrVal {
                    ty: res_ty,
                    ssa: buf,
                },
            ])
        }
        0x0401 => {
            // FILE_WRITE(e, h, data) -> (Eff, Result<u64, u32>)
            m.require_decl("declare void @vg_rt_file_write(ptr, i32, ptr, ptr)");
            let h = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            let data = operand_val(f, m, node, op.operands[2], inputs, op_vals)?;
            let res_ty = op.results[1].clone();
            let buf = alloc_result_buf(f, m, an, op_idx, 1, &res_ty, scratch_base)?;
            f.emit(format!(
                "call void @vg_rt_file_write(ptr {ctx_ptr}, i32 {}, ptr {}, ptr {buf})",
                h.ssa, data.ssa
            ));
            Ok(vec![
                IrVal {
                    ty: Type::Eff,
                    ssa: String::new(),
                },
                IrVal {
                    ty: res_ty,
                    ssa: buf,
                },
            ])
        }
        0x0402 => {
            // CLOCK_READ(e, c) -> (Eff, u64)
            m.require_decl("declare i64 @vg_rt_clock_read(ptr, i32)");
            let c = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            let r = f.reg();
            f.emit(format!(
                "{r} = call i64 @vg_rt_clock_read(ptr {ctx_ptr}, i32 {})",
                c.ssa
            ));
            Ok(vec![
                IrVal {
                    ty: Type::Eff,
                    ssa: String::new(),
                },
                IrVal {
                    ty: Type::UInt(64),
                    ssa: r,
                },
            ])
        }
        other => Err(Diagnostic::error(
            "VG-S0104",
            Span::default(),
            format!("opcode {other:#06x} not supported in codegen"),
        )),
    }
}

fn collection_ptr_len(f: &mut FnBuilder, v: &IrVal) -> Result<(String, String), Diagnostic> {
    match &v.ty {
        Type::Array(_, n) => Ok((v.ssa.clone(), format!("{n}"))),
        Type::Tensor(_, dims) => Ok((v.ssa.clone(), format!("{}", tensor_elem_count(dims)?))),
        Type::Slice(_) => Ok(emit_slice_parts(f, &v.ssa)),
        t => Err(Diagnostic::error(
            "VG-T0101",
            Span::default(),
            format!("expected collection, found {t}"),
        )),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_cast(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    src_ty: &Type,
    dst_ty: &Type,
    src: &str,
    ctx_ptr: &str,
    ob: &str,
    prefix: &str,
) -> Result<String, Diagnostic> {
    if src_ty == dst_ty {
        return Ok(src.to_string());
    }
    match (src_ty, dst_ty) {
        (Type::Bool, Type::UInt(w) | Type::SInt(w)) => {
            let b = f.reg();
            f.emit(format!("{b} = icmp ne i8 {src}, 0"));
            let r = f.reg();
            f.emit(format!("{r} = zext i1 {b} to i{w}"));
            Ok(r)
        }
        (Type::UInt(w1), Type::UInt(w2)) => {
            if w1 < w2 {
                let r = f.reg();
                f.emit(format!("{r} = zext i{w1} {src} to i{w2}"));
                Ok(r)
            } else {
                if m.runtime_checks {
                    let max2 = (1u128 << w2) - 1;
                    let ok = f.reg();
                    f.emit(format!("{ok} = icmp ule i{w1} {src}, {max2}"));
                    emit_branch_trap_if_false(f, m, &ok, ctx_ptr, "rt", ob, prefix);
                }
                let r = f.reg();
                f.emit(format!("{r} = trunc i{w1} {src} to i{w2}"));
                Ok(r)
            }
        }
        (Type::UInt(w1), Type::SInt(w2)) => {
            if w1 < w2 {
                let r = f.reg();
                f.emit(format!("{r} = zext i{w1} {src} to i{w2}"));
                Ok(r)
            } else {
                if m.runtime_checks {
                    let smax2 = (1u128 << (w2 - 1)) - 1;
                    let ok = f.reg();
                    f.emit(format!("{ok} = icmp ule i{w1} {src}, {smax2}"));
                    emit_branch_trap_if_false(f, m, &ok, ctx_ptr, "rt", ob, prefix);
                }
                if w1 == w2 {
                    Ok(src.to_string())
                } else {
                    let r = f.reg();
                    f.emit(format!("{r} = trunc i{w1} {src} to i{w2}"));
                    Ok(r)
                }
            }
        }
        (Type::SInt(w1), Type::SInt(w2)) => {
            if w1 < w2 {
                let r = f.reg();
                f.emit(format!("{r} = sext i{w1} {src} to i{w2}"));
                Ok(r)
            } else {
                if m.runtime_checks {
                    let smin2 = -(1i128 << (w2 - 1));
                    let smax2 = (1i128 << (w2 - 1)) - 1;
                    let c1 = f.reg();
                    f.emit(format!("{c1} = icmp sge i{w1} {src}, {smin2}"));
                    let c2 = f.reg();
                    f.emit(format!("{c2} = icmp sle i{w1} {src}, {smax2}"));
                    let ok = f.reg();
                    f.emit(format!("{ok} = and i1 {c1}, {c2}"));
                    emit_branch_trap_if_false(f, m, &ok, ctx_ptr, "rt", ob, prefix);
                }
                let r = f.reg();
                f.emit(format!("{r} = trunc i{w1} {src} to i{w2}"));
                Ok(r)
            }
        }
        (Type::SInt(w1), Type::UInt(w2)) => {
            if m.runtime_checks {
                let c1 = f.reg();
                f.emit(format!("{c1} = icmp sge i{w1} {src}, 0"));
                let ok = if w1 > w2 {
                    let umax2 = (1i128 << w2) - 1;
                    let c2 = f.reg();
                    f.emit(format!("{c2} = icmp sle i{w1} {src}, {umax2}"));
                    let and = f.reg();
                    f.emit(format!("{and} = and i1 {c1}, {c2}"));
                    and
                } else {
                    c1
                };
                emit_branch_trap_if_false(f, m, &ok, ctx_ptr, "rt", ob, prefix);
            }
            if w1 < w2 {
                let r = f.reg();
                f.emit(format!("{r} = zext i{w1} {src} to i{w2}"));
                Ok(r)
            } else if w1 == w2 {
                Ok(src.to_string())
            } else {
                let r = f.reg();
                f.emit(format!("{r} = trunc i{w1} {src} to i{w2}"));
                Ok(r)
            }
        }
        (Type::UInt(w), Type::Float(32) | Type::Float(64)) => {
            let dlty = llvm_type(dst_ty, m.target)?;
            let r = f.reg();
            f.emit(format!("{r} = uitofp i{w} {src} to {dlty}"));
            Ok(r)
        }
        (Type::SInt(w), Type::Float(32) | Type::Float(64)) => {
            let dlty = llvm_type(dst_ty, m.target)?;
            let r = f.reg();
            f.emit(format!("{r} = sitofp i{w} {src} to {dlty}"));
            Ok(r)
        }
        (Type::UInt(w), Type::Float(16) | Type::BFloat16) => {
            let widened = if *w < 64 {
                let r = f.reg();
                f.emit(format!("{r} = zext i{w} {src} to i64"));
                r
            } else {
                src.to_string()
            };
            let func = if *dst_ty == Type::Float(16) {
                m.require_decl("declare i16 @vg_u64_to_f16(i64)");
                "@vg_u64_to_f16"
            } else {
                m.require_decl("declare i16 @vg_u64_to_bf16(i64)");
                "@vg_u64_to_bf16"
            };
            let r = f.reg();
            f.emit(format!("{r} = call i16 {func}(i64 {widened})"));
            Ok(r)
        }
        (Type::SInt(w), Type::Float(16) | Type::BFloat16) => {
            let widened = if *w < 64 {
                let r = f.reg();
                f.emit(format!("{r} = sext i{w} {src} to i64"));
                r
            } else {
                src.to_string()
            };
            let func = if *dst_ty == Type::Float(16) {
                m.require_decl("declare i16 @vg_i64_to_f16(i64)");
                "@vg_i64_to_f16"
            } else {
                m.require_decl("declare i16 @vg_i64_to_bf16(i64)");
                "@vg_i64_to_bf16"
            };
            let r = f.reg();
            f.emit(format!("{r} = call i16 {func}(i64 {widened})"));
            Ok(r)
        }
        (s, d) if s.is_float() && d.is_float() => emit_float_float_cast(f, m, s, d, src),
        (s, Type::UInt(w) | Type::SInt(w)) if s.is_float() => {
            let (fval, flty) = match s {
                Type::Float(32) => (src.to_string(), "float"),
                Type::Float(64) => (src.to_string(), "double"),
                Type::Float(16) => {
                    m.require_decl("declare double @vg_f16_to_f64(i16)");
                    let r = f.reg();
                    f.emit(format!("{r} = call double @vg_f16_to_f64(i16 {src})"));
                    (r, "double")
                }
                Type::BFloat16 => {
                    m.require_decl("declare double @vg_bf16_to_f64(i16)");
                    let r = f.reg();
                    f.emit(format!("{r} = call double @vg_bf16_to_f64(i16 {src})"));
                    (r, "double")
                }
                _ => unreachable!(),
            };
            let signed = matches!(dst_ty, Type::SInt(_));
            if m.runtime_checks {
                let (lo_expr, lo_pred, hi_expr) = float_to_int_bounds(*w, signed, flty);
                let c_lo = f.reg();
                f.emit(format!("{c_lo} = fcmp {lo_pred} {flty} {fval}, {lo_expr}"));
                let c_hi = f.reg();
                f.emit(format!("{c_hi} = fcmp olt {flty} {fval}, {hi_expr}"));
                let ok = f.reg();
                f.emit(format!("{ok} = and i1 {c_lo}, {c_hi}"));
                emit_branch_trap_if_false(f, m, &ok, ctx_ptr, "rt", ob, prefix);
            }
            let instr = if signed { "fptosi" } else { "fptoui" };
            let r = f.reg();
            f.emit(format!("{r} = {instr} {flty} {fval} to i{w}"));
            Ok(r)
        }
        _ => Err(Diagnostic::error(
            "VG-T0113",
            Span::default(),
            format!("CAST not allowed from {src_ty} to {dst_ty}"),
        )),
    }
}

fn float_to_int_bounds(w: u8, signed: bool, flty: &str) -> (String, &'static str, String) {
    let fexpr = |v: f64| -> String {
        if flty == "float" {
            f32_const_expr((v as f32).to_bits())
        } else {
            f64_const_expr(v.to_bits())
        }
    };
    if !signed {
        let upper = match w {
            8 => 256.0_f64,
            16 => 65536.0_f64,
            32 => 4294967296.0_f64,
            64 => 18446744073709551616.0_f64,
            _ => unreachable!(),
        };
        (fexpr(-1.0), "ogt", fexpr(upper))
    } else {
        match w {
            8 => (fexpr(-129.0), "ogt", fexpr(128.0)),
            16 => (fexpr(-32769.0), "ogt", fexpr(32768.0)),
            32 => {
                if flty == "float" {
                    (fexpr(-2147483648.0), "oge", fexpr(2147483648.0))
                } else {
                    (fexpr(-2147483649.0), "ogt", fexpr(2147483648.0))
                }
            }
            64 => (
                fexpr(-9223372036854775808.0),
                "oge",
                fexpr(9223372036854775808.0),
            ),
            _ => unreachable!(),
        }
    }
}

fn emit_float_float_cast(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    src_ty: &Type,
    dst_ty: &Type,
    src: &str,
) -> Result<String, Diagnostic> {
    let r = match (src_ty, dst_ty) {
        (Type::Float(16), Type::Float(32)) => {
            m.require_decl("declare float @vg_f16_to_f32(i16)");
            let r = f.reg();
            f.emit(format!("{r} = call float @vg_f16_to_f32(i16 {src})"));
            r
        }
        (Type::Float(16), Type::Float(64)) => {
            m.require_decl("declare double @vg_f16_to_f64(i16)");
            let r = f.reg();
            f.emit(format!("{r} = call double @vg_f16_to_f64(i16 {src})"));
            r
        }
        (Type::Float(16), Type::BFloat16) => {
            m.require_decl("declare float @vg_f16_to_f32(i16)");
            m.require_decl("declare i16 @vg_f32_to_bf16(float)");
            let t = f.reg();
            f.emit(format!("{t} = call float @vg_f16_to_f32(i16 {src})"));
            let r = f.reg();
            f.emit(format!("{r} = call i16 @vg_f32_to_bf16(float {t})"));
            r
        }
        (Type::BFloat16, Type::Float(32)) => {
            m.require_decl("declare float @vg_bf16_to_f32(i16)");
            let r = f.reg();
            f.emit(format!("{r} = call float @vg_bf16_to_f32(i16 {src})"));
            r
        }
        (Type::BFloat16, Type::Float(64)) => {
            m.require_decl("declare double @vg_bf16_to_f64(i16)");
            let r = f.reg();
            f.emit(format!("{r} = call double @vg_bf16_to_f64(i16 {src})"));
            r
        }
        (Type::BFloat16, Type::Float(16)) => {
            m.require_decl("declare float @vg_bf16_to_f32(i16)");
            m.require_decl("declare i16 @vg_f32_to_f16(float)");
            let t = f.reg();
            f.emit(format!("{t} = call float @vg_bf16_to_f32(i16 {src})"));
            let r = f.reg();
            f.emit(format!("{r} = call i16 @vg_f32_to_f16(float {t})"));
            r
        }
        (Type::Float(32), Type::Float(16)) => {
            m.require_decl("declare i16 @vg_f32_to_f16(float)");
            let r = f.reg();
            f.emit(format!("{r} = call i16 @vg_f32_to_f16(float {src})"));
            r
        }
        (Type::Float(32), Type::BFloat16) => {
            m.require_decl("declare i16 @vg_f32_to_bf16(float)");
            let r = f.reg();
            f.emit(format!("{r} = call i16 @vg_f32_to_bf16(float {src})"));
            r
        }
        (Type::Float(32), Type::Float(64)) => {
            let r = f.reg();
            f.emit(format!("{r} = fpext float {src} to double"));
            r
        }
        (Type::Float(64), Type::Float(16)) => {
            m.require_decl("declare i16 @vg_f64_to_f16(double)");
            let r = f.reg();
            f.emit(format!("{r} = call i16 @vg_f64_to_f16(double {src})"));
            r
        }
        (Type::Float(64), Type::BFloat16) => {
            m.require_decl("declare i16 @vg_f64_to_bf16(double)");
            let r = f.reg();
            f.emit(format!("{r} = call i16 @vg_f64_to_bf16(double {src})"));
            r
        }
        (Type::Float(64), Type::Float(32)) => {
            let r = f.reg();
            f.emit(format!("{r} = fptrunc double {src} to float"));
            r
        }
        _ => unreachable!(),
    };
    Ok(emit_canon_float(f, dst_ty, &r))
}

#[allow(clippy::too_many_arguments)]
fn emit_int_arith(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    node: &Node,
    op_idx: usize,
    op: &Op,
    ctx_ptr: &str,
    prefix: &str,
    inputs: &[IrVal],
    op_vals: &[Vec<IrVal>],
) -> Result<String, Diagnostic> {
    let ty = &op.results[0];
    let (w, signed) = match ty {
        Type::UInt(w) => (*w, false),
        Type::SInt(w) => (*w, true),
        _ => {
            return Err(Diagnostic::error(
                "VG-T0104",
                Span::default(),
                "int arith requires integer type",
            ));
        }
    };
    let lty = format!("i{w}");
    let a = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
    let ob = |k: &str| format!("@{prefix}/%op{op_idx}/{k}");

    if op.opcode == 0x0015 {
        // NEG
        return Ok(match op.mode {
            0 => {
                if m.runtime_checks {
                    let ok = f.reg();
                    if signed {
                        let min_v = -(1i128 << (w - 1));
                        f.emit(format!("{ok} = icmp ne {lty} {}, {min_v}", a.ssa));
                    } else {
                        f.emit(format!("{ok} = icmp eq {lty} {}, 0", a.ssa));
                    }
                    emit_branch_trap_if_false(
                        f,
                        m,
                        &ok,
                        ctx_ptr,
                        "rt",
                        &ob("NEG.no_overflow"),
                        prefix,
                    );
                }
                let flag = if m.proof_facts && !m.runtime_checks {
                    if signed { " nsw" } else { " nuw" }
                } else {
                    ""
                };
                let r = f.reg();
                f.emit(format!("{r} = sub{flag} {lty} 0, {}", a.ssa));
                r
            }
            1 => {
                let r = f.reg();
                f.emit(format!("{r} = sub {lty} 0, {}", a.ssa));
                r
            }
            2 => {
                if !signed {
                    "0".to_string()
                } else {
                    let min_v = -(1i128 << (w - 1));
                    let max_v = (1i128 << (w - 1)) - 1;
                    let is_min = f.reg();
                    f.emit(format!("{is_min} = icmp eq {lty} {}, {min_v}", a.ssa));
                    let neg = f.reg();
                    f.emit(format!("{neg} = sub {lty} 0, {}", a.ssa));
                    let r = f.reg();
                    f.emit(format!(
                        "{r} = select i1 {is_min}, {lty} {max_v}, {lty} {neg}"
                    ));
                    r
                }
            }
            _ => unreachable!(),
        });
    }

    let b = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
    match op.opcode {
        0x0010..=0x0012 => {
            let (base, intr_base, ob_name) = match op.opcode {
                0x0010 => ("add", "add", "ADD.no_overflow"),
                0x0011 => ("sub", "sub", "SUB.no_overflow"),
                0x0012 => ("mul", "mul", "MUL.no_overflow"),
                _ => unreachable!(),
            };
            let s_ch = if signed { 's' } else { 'u' };
            Ok(match op.mode {
                0 => {
                    if m.runtime_checks {
                        let intr = format!("@llvm.{s_ch}{intr_base}.with.overflow.{lty}");
                        m.require_decl(&format!("declare {{ {lty}, i1 }} {intr}({lty}, {lty})"));
                        let ov = f.reg();
                        f.emit(format!(
                            "{ov} = call {{ {lty}, i1 }} {intr}({lty} {}, {lty} {})",
                            a.ssa, b.ssa
                        ));
                        let val = f.reg();
                        f.emit(format!("{val} = extractvalue {{ {lty}, i1 }} {ov}, 0"));
                        let flag = f.reg();
                        f.emit(format!("{flag} = extractvalue {{ {lty}, i1 }} {ov}, 1"));
                        let ok = f.reg();
                        f.emit(format!("{ok} = xor i1 {flag}, true"));
                        emit_branch_trap_if_false(f, m, &ok, ctx_ptr, "rt", &ob(ob_name), prefix);
                        val
                    } else {
                        let flag = if m.proof_facts {
                            if signed { " nsw" } else { " nuw" }
                        } else {
                            ""
                        };
                        let r = f.reg();
                        f.emit(format!("{r} = {base}{flag} {lty} {}, {}", a.ssa, b.ssa));
                        r
                    }
                }
                1 => {
                    let r = f.reg();
                    f.emit(format!("{r} = {base} {lty} {}, {}", a.ssa, b.ssa));
                    r
                }
                2 => {
                    if op.opcode == 0x0012 {
                        let intr = format!("@llvm.{s_ch}mul.fix.sat.{lty}");
                        m.require_decl(&format!("declare {lty} {intr}({lty}, {lty}, i32)"));
                        let r = f.reg();
                        f.emit(format!(
                            "{r} = call {lty} {intr}({lty} {}, {lty} {}, i32 0)",
                            a.ssa, b.ssa
                        ));
                        r
                    } else {
                        let intr = format!("@llvm.{s_ch}{intr_base}.sat.{lty}");
                        m.require_decl(&format!("declare {lty} {intr}({lty}, {lty})"));
                        let r = f.reg();
                        f.emit(format!(
                            "{r} = call {lty} {intr}({lty} {}, {lty} {})",
                            a.ssa, b.ssa
                        ));
                        r
                    }
                }
                _ => unreachable!(),
            })
        }
        0x0013 | 0x0014 => {
            // DIV / MOD
            let is_div = op.opcode == 0x0013;
            let nz_ob = if is_div { "DIV.nonzero" } else { "MOD.nonzero" };
            if m.runtime_checks {
                let nz = f.reg();
                f.emit(format!("{nz} = icmp ne {lty} {}, 0", b.ssa));
                emit_branch_trap_if_false(f, m, &nz, ctx_ptr, "rt", &ob(nz_ob), prefix);
            }
            if !signed {
                let instr = if is_div { "udiv" } else { "urem" };
                let r = f.reg();
                f.emit(format!("{r} = {instr} {lty} {}, {}", a.ssa, b.ssa));
                return Ok(r);
            }
            let min_v = -(1i128 << (w - 1));
            let max_v = (1i128 << (w - 1)) - 1;
            if is_div && op.mode == 0 {
                if m.runtime_checks {
                    let is_min = f.reg();
                    f.emit(format!("{is_min} = icmp eq {lty} {}, {min_v}", a.ssa));
                    let is_m1 = f.reg();
                    f.emit(format!("{is_m1} = icmp eq {lty} {}, -1", b.ssa));
                    let ov = f.reg();
                    f.emit(format!("{ov} = and i1 {is_min}, {is_m1}"));
                    let ok = f.reg();
                    f.emit(format!("{ok} = xor i1 {ov}, true"));
                    emit_branch_trap_if_false(
                        f,
                        m,
                        &ok,
                        ctx_ptr,
                        "rt",
                        &ob("DIV.no_overflow"),
                        prefix,
                    );
                }
                let r = f.reg();
                f.emit(format!("{r} = sdiv {lty} {}, {}", a.ssa, b.ssa));
                return Ok(r);
            }
            // Signed DIV wrap/sat or signed MOD (all modes): guard MIN / -1 before LLVM sdiv/srem (`docs/14`)
            let is_min = f.reg();
            f.emit(format!("{is_min} = icmp eq {lty} {}, {min_v}", a.ssa));
            let is_m1 = f.reg();
            f.emit(format!("{is_m1} = icmp eq {lty} {}, -1", b.ssa));
            let is_edge = f.reg();
            f.emit(format!("{is_edge} = and i1 {is_min}, {is_m1}"));
            let safe_b = f.reg();
            f.emit(format!(
                "{safe_b} = select i1 {is_edge}, {lty} 1, {lty} {}",
                b.ssa
            ));
            let instr = if is_div { "sdiv" } else { "srem" };
            let q = f.reg();
            f.emit(format!("{q} = {instr} {lty} {}, {safe_b}", a.ssa));
            let edge_val = if !is_div {
                "0".to_string()
            } else if op.mode == 1 {
                format!("{min_v}")
            } else {
                format!("{max_v}")
            };
            let res = f.reg();
            f.emit(format!(
                "{res} = select i1 {is_edge}, {lty} {edge_val}, {lty} {q}"
            ));
            Ok(res)
        }
        _ => unreachable!(),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_bitwise_and_shift(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    node: &Node,
    op_idx: usize,
    op: &Op,
    ctx_ptr: &str,
    prefix: &str,
    inputs: &[IrVal],
    op_vals: &[Vec<IrVal>],
) -> Result<String, Diagnostic> {
    let ty = &op.results[0];
    let (w, signed) = match ty {
        Type::UInt(w) => (*w, false),
        Type::SInt(w) => (*w, true),
        _ => {
            return Err(Diagnostic::error(
                "VG-T0104",
                Span::default(),
                "bitwise op requires integer type",
            ));
        }
    };
    let lty = format!("i{w}");
    let a = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
    if op.opcode == 0x0033 {
        let r = f.reg();
        f.emit(format!("{r} = xor {lty} {}, -1", a.ssa));
        return Ok(r);
    }
    let b = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
    let ob = |k: &str| format!("@{prefix}/%op{op_idx}/{k}");

    match op.opcode {
        0x0030 => {
            let r = f.reg();
            f.emit(format!("{r} = and {lty} {}, {}", a.ssa, b.ssa));
            Ok(r)
        }
        0x0031 => {
            let r = f.reg();
            f.emit(format!("{r} = or {lty} {}, {}", a.ssa, b.ssa));
            Ok(r)
        }
        0x0032 => {
            let r = f.reg();
            f.emit(format!("{r} = xor {lty} {}, {}", a.ssa, b.ssa));
            Ok(r)
        }
        0x0034 | 0x0035 => {
            let sh_u32 = if op.mode == 0 {
                if m.runtime_checks {
                    let ok = f.reg();
                    f.emit(format!("{ok} = icmp ult i32 {}, {w}", b.ssa));
                    let ob_name = if op.opcode == 0x0034 {
                        "SHL.amount"
                    } else {
                        "SHR.amount"
                    };
                    emit_branch_trap_if_false(f, m, &ok, ctx_ptr, "rt", &ob(ob_name), prefix);
                }
                b.ssa.clone()
            } else {
                let masked = f.reg();
                f.emit(format!("{masked} = and i32 {}, {}", b.ssa, w - 1));
                masked
            };
            let sh = convert_u32_to_int_width(f, &sh_u32, w);
            let instr = if op.opcode == 0x0034 {
                "shl"
            } else if signed {
                "ashr"
            } else {
                "lshr"
            };
            let r = f.reg();
            f.emit(format!("{r} = {instr} {lty} {}, {sh}", a.ssa));
            Ok(r)
        }
        0x0036 | 0x0037 => {
            let sh = convert_u32_to_int_width(f, &b.ssa, w);
            let dir = if op.opcode == 0x0036 { "fshl" } else { "fshr" };
            let intr = format!("@llvm.{dir}.{lty}");
            m.require_decl(&format!("declare {lty} {intr}({lty}, {lty}, {lty})"));
            let r = f.reg();
            f.emit(format!(
                "{r} = call {lty} {intr}({lty} {}, {lty} {}, {lty} {sh})",
                a.ssa, a.ssa
            ));
            Ok(r)
        }
        _ => unreachable!(),
    }
}

fn convert_u32_to_int_width(f: &mut FnBuilder, v_i32: &str, w: u8) -> String {
    if w == 32 {
        v_i32.to_string()
    } else if w < 32 {
        let r = f.reg();
        f.emit(format!("{r} = trunc i32 {v_i32} to i{w}"));
        r
    } else {
        let r = f.reg();
        f.emit(format!("{r} = zext i32 {v_i32} to i{w}"));
        r
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_float_op(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    node: &Node,
    op_idx: usize,
    op: &Op,
    ctx_ptr: &str,
    prefix: &str,
    inputs: &[IrVal],
    op_vals: &[Vec<IrVal>],
) -> Result<String, Diagnostic> {
    let ty = &op.results[0];
    let lty = match ty {
        Type::Float(32) => "float",
        Type::Float(64) => "double",
        _ => {
            return Err(Diagnostic::error(
                "VG-T0105",
                Span::default(),
                "float arithmetic only allowed on f32 and f64",
            ));
        }
    };
    let sfx = if *ty == Type::Float(32) { "f32" } else { "f64" };
    let ob = |k: &str| format!("@{prefix}/%op{op_idx}/{k}");
    let a = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
    match op.opcode {
        0x0040..=0x0043 => {
            let b = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            let instr = match op.opcode {
                0x0040 => "fadd",
                0x0041 => "fsub",
                0x0042 => "fmul",
                0x0043 => "fdiv",
                _ => unreachable!(),
            };
            let r = f.reg();
            f.emit(format!("{r} = {instr} {lty} {}, {}", a.ssa, b.ssa));
            Ok(r)
        }
        0x0044 => {
            // FMA (never fmuladd!)
            let b = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            let c = operand_val(f, m, node, op.operands[2], inputs, op_vals)?;
            let intr = format!("@llvm.fma.{sfx}");
            m.require_decl(&format!("declare {lty} {intr}({lty}, {lty}, {lty})"));
            let r = f.reg();
            f.emit(format!(
                "{r} = call {lty} {intr}({lty} {}, {lty} {}, {lty} {})",
                a.ssa, b.ssa, c.ssa
            ));
            Ok(r)
        }
        0x0045 => {
            let r = f.reg();
            f.emit(format!("{r} = fneg {lty} {}", a.ssa));
            Ok(r)
        }
        0x0046 => {
            let intr = format!("@llvm.fabs.{sfx}");
            m.require_decl(&format!("declare {lty} {intr}({lty})"));
            let r = f.reg();
            f.emit(format!("{r} = call {lty} {intr}({lty} {})", a.ssa));
            Ok(r)
        }
        0x0047 | 0x0048 => {
            let b = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            let name = if op.opcode == 0x0047 {
                "minimum"
            } else {
                "maximum"
            };
            let intr = format!("@llvm.{name}.{sfx}");
            m.require_decl(&format!("declare {lty} {intr}({lty}, {lty})"));
            let r = f.reg();
            f.emit(format!(
                "{r} = call {lty} {intr}({lty} {}, {lty} {})",
                a.ssa, b.ssa
            ));
            Ok(r)
        }
        0x0049 => {
            // FCLAMP(x, lo, hi)
            let lo = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            let hi = operand_val(f, m, node, op.operands[2], inputs, op_vals)?;
            if m.runtime_checks {
                let ok = f.reg();
                f.emit(format!("{ok} = fcmp ole {lty} {}, {}", lo.ssa, hi.ssa));
                emit_branch_trap_if_false(f, m, &ok, ctx_ptr, "rt", &ob("FCLAMP.bounds"), prefix);
            }
            let max_intr = format!("@llvm.maximum.{sfx}");
            let min_intr = format!("@llvm.minimum.{sfx}");
            m.require_decl(&format!("declare {lty} {max_intr}({lty}, {lty})"));
            m.require_decl(&format!("declare {lty} {min_intr}({lty}, {lty})"));
            let m1 = f.reg();
            f.emit(format!(
                "{m1} = call {lty} {max_intr}({lty} {}, {lty} {})",
                a.ssa, lo.ssa
            ));
            let m2 = f.reg();
            f.emit(format!(
                "{m2} = call {lty} {min_intr}({lty} {m1}, {lty} {})",
                hi.ssa
            ));
            Ok(m2)
        }
        0x004A => {
            // FCOPYSIGN(x, y)
            let y = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
            if m.runtime_checks {
                let ok = f.reg();
                f.emit(format!("{ok} = fcmp ord {lty} {}, {}", y.ssa, y.ssa));
                emit_branch_trap_if_false(f, m, &ok, ctx_ptr, "rt", &ob("FCOPYSIGN.sign"), prefix);
            }
            let intr = format!("@llvm.copysign.{sfx}");
            m.require_decl(&format!("declare {lty} {intr}({lty}, {lty})"));
            let r = f.reg();
            f.emit(format!(
                "{r} = call {lty} {intr}({lty} {}, {lty} {})",
                a.ssa, y.ssa
            ));
            Ok(r)
        }
        0x004B => {
            let intr = format!("@llvm.sqrt.{sfx}");
            m.require_decl(&format!("declare {lty} {intr}({lty})"));
            let r = f.reg();
            f.emit(format!("{r} = call {lty} {intr}({lty} {})", a.ssa));
            Ok(r)
        }
        _ => unreachable!(),
    }
}

fn emit_cmp_and_logic(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    node: &Node,
    op: &Op,
    inputs: &[IrVal],
    op_vals: &[Vec<IrVal>],
) -> Result<String, Diagnostic> {
    let a = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
    if op.opcode == 0x0058 {
        // BNOT
        let b = f.reg();
        f.emit(format!("{b} = icmp eq i8 {}, 0", a.ssa));
        let r = f.reg();
        f.emit(format!("{r} = zext i1 {b} to i8"));
        return Ok(r);
    }
    let b = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
    if matches!(op.opcode, 0x0056 | 0x0057) {
        let ba = f.reg();
        f.emit(format!("{ba} = icmp ne i8 {}, 0", a.ssa));
        let bb = f.reg();
        f.emit(format!("{bb} = icmp ne i8 {}, 0", b.ssa));
        let instr = if op.opcode == 0x0056 { "and" } else { "or" };
        let bc = f.reg();
        f.emit(format!("{bc} = {instr} i1 {ba}, {bb}"));
        let r = f.reg();
        f.emit(format!("{r} = zext i1 {bc} to i8"));
        return Ok(r);
    }

    let c = f.reg();
    match &a.ty {
        Type::Float(32) | Type::Float(64) => {
            let lty = if a.ty == Type::Float(32) {
                "float"
            } else {
                "double"
            };
            let pred = match op.opcode {
                0x0050 => "oeq",
                0x0051 => "une",
                0x0052 => "olt",
                0x0053 => "ole",
                0x0054 => "ogt",
                0x0055 => "oge",
                _ => unreachable!(),
            };
            f.emit(format!("{c} = fcmp {pred} {lty} {}, {}", a.ssa, b.ssa));
        }
        Type::Bool => {
            let pred = if op.opcode == 0x0050 { "eq" } else { "ne" };
            f.emit(format!("{c} = icmp {pred} i8 {}, {}", a.ssa, b.ssa));
        }
        Type::UInt(w) | Type::SInt(w) => {
            let signed = matches!(a.ty, Type::SInt(_));
            let pred = match op.opcode {
                0x0050 => "eq",
                0x0051 => "ne",
                0x0052 => {
                    if signed || op.mode == 1 {
                        "slt"
                    } else {
                        "ult"
                    }
                }
                0x0053 => {
                    if signed || op.mode == 1 {
                        "sle"
                    } else {
                        "ule"
                    }
                }
                0x0054 => {
                    if signed || op.mode == 1 {
                        "sgt"
                    } else {
                        "ugt"
                    }
                }
                0x0055 => {
                    if signed || op.mode == 1 {
                        "sge"
                    } else {
                        "uge"
                    }
                }
                _ => unreachable!(),
            };
            f.emit(format!("{c} = icmp {pred} i{w} {}, {}", a.ssa, b.ssa));
        }
        other => {
            return Err(Diagnostic::error(
                "VG-T0105",
                Span::default(),
                format!("comparison not supported on {other}"),
            ));
        }
    }
    let r = f.reg();
    f.emit(format!("{r} = zext i1 {c} to i8"));
    Ok(r)
}

#[allow(clippy::too_many_arguments)]
fn emit_simd_map(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    graph: &CallGraph,
    node: &Node,
    an: &NodeAnalysis,
    op_idx: usize,
    op: &Op,
    ctx_ptr: &str,
    scratch_base: Option<&str>,
    inputs: &[IrVal],
    op_vals: &[Vec<IrVal>],
) -> Result<Vec<IrVal>, Diagnostic> {
    let Operand::Callee(ci) = op.operands[0] else {
        unreachable!()
    };
    let kh = node.calls[ci as usize];
    let knode = &graph.by_hash[&kh];
    let kprefix = &graph.prefixes[&kh];

    let xs = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
    let (src_dp, len) = collection_ptr_len(f, &xs)?;
    let mut broadcasts = Vec::new();
    for &bo in &op.operands[2..] {
        broadcasts.push(operand_val(f, m, node, bo, inputs, op_vals)?);
    }

    let out_ty = op.results[0].clone();
    let in_elem_ty = xs.ty.elem().expect("collection elem").clone();
    let out_elem_ty = out_ty.elem().expect("collection elem").clone();
    let in_elem_lty = llvm_type(&in_elem_ty, m.target)?;
    let out_elem_lty = llvm_type(&out_elem_ty, m.target)?;
    let out_elem_sz = size_of(&out_elem_ty, m.target)?;
    let out_buf = alloc_result_buf(f, m, an, op_idx, 0, &out_ty, scratch_base)?;

    let dst_dp = if matches!(out_ty, Type::Slice(_)) {
        let bytes = f.reg();
        f.emit(format!("{bytes} = mul i64 {len}, {out_elem_sz}"));
        let dp = f.reg();
        f.emit(format!(
            "{dp} = call ptr @vg_arena_alloc(ptr {ctx_ptr}, i64 {bytes})"
        ));
        emit_store_slice(f, &out_buf, &dp, &len);
        dp
    } else {
        out_buf.clone()
    };

    let idx_ptr = f.alloca("i64", 8);
    f.emit(format!("store i64 0, ptr {idx_ptr}, align 8"));
    let cond_bb = f.lbl("map.cond");
    let body_bb = f.lbl("map.body");
    let done_bb = f.lbl("map.done");
    f.br(&cond_bb);

    f.start_bb(&cond_bb);
    let idx = f.reg();
    f.emit(format!("{idx} = load i64, ptr {idx_ptr}, align 8"));
    let cmp = f.reg();
    f.emit(format!("{cmp} = icmp ult i64 {idx}, {len}"));
    f.cond_br(&cmp, &body_bb, &done_bb);

    f.start_bb(&body_bb);
    let sp = emit_gep_bytes(f, m.target, m.proof_facts, &in_elem_lty, &src_dp, &idx);
    let dp = emit_gep_bytes(f, m.target, m.proof_facts, &out_elem_lty, &dst_dp, &idx);

    let mut cargs = vec![format!("ptr {ctx_ptr}")];
    if is_scalar_abi(&in_elem_ty) {
        let ev = emit_load_scalar(f, &in_elem_ty, &sp, m.target)?;
        cargs.push(format!("{in_elem_lty} {ev}"));
    } else {
        cargs.push(format!("ptr {sp}"));
    }
    for bv in &broadcasts {
        if is_scalar_abi(&bv.ty) {
            cargs.push(format!("{} {}", llvm_type(&bv.ty, m.target)?, bv.ssa));
        } else {
            cargs.push(format!("ptr {}", bv.ssa));
        }
    }
    for l in &knode.learnable {
        let c = &knode.consts[l.lo as usize];
        let iv = emit_const_val(f, m, &c.ty, &c.bytes)?;
        cargs.push(format!("{} {}", llvm_type(&c.ty, m.target)?, iv.ssa));
    }
    cargs.push(format!("ptr {dp}"));
    f.emit(format!("call void @vgi_{kprefix}({})", cargs.join(", ")));

    let next = f.reg();
    f.emit(format!("{next} = add nuw i64 {idx}, 1"));
    f.emit(format!("store i64 {next}, ptr {idx_ptr}, align 8"));
    m.has_vec_loop = true;
    f.emit(format!("br label %{cond_bb}, !llvm.loop !0"));
    f.start_bb(&done_bb);
    Ok(vec![IrVal {
        ty: out_ty,
        ssa: out_buf,
    }])
}

#[allow(clippy::too_many_arguments)]
fn emit_fold_bounded(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    graph: &CallGraph,
    node: &Node,
    an: &NodeAnalysis,
    op_idx: usize,
    op: &Op,
    ctx_ptr: &str,
    scratch_base: Option<&str>,
    inputs: &[IrVal],
    op_vals: &[Vec<IrVal>],
) -> Result<Vec<IrVal>, Diagnostic> {
    // Operands: [Callee(f), xs, b..., init, Invariant(I)]
    let Operand::Callee(ci) = op.operands[0] else {
        unreachable!()
    };
    let kh = node.calls[ci as usize];
    let knode = &graph.by_hash[&kh];
    let kprefix = &graph.prefixes[&kh];

    let xs = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
    let (src_dp, len) = collection_ptr_len(f, &xs)?;
    let init_op = op.operands[op.operands.len() - 2];
    let init_v = operand_val(f, m, node, init_op, inputs, op_vals)?;
    let mut broadcasts = Vec::new();
    for &bo in &op.operands[2..op.operands.len() - 2] {
        broadcasts.push(operand_val(f, m, node, bo, inputs, op_vals)?);
    }

    let acc_ty = op.results[0].clone();
    let acc_lty = llvm_type(&acc_ty, m.target)?;
    let acc_align = align_of(&acc_ty, m.target)?;
    let elem_ty = xs.ty.elem().expect("fold elem").clone();
    let elem_lty = llvm_type(&elem_ty, m.target)?;

    let acc_buf = alloc_result_buf(f, m, an, op_idx, 0, &acc_ty, scratch_base)?;
    emit_store_value_canon(f, m, &acc_ty, &init_v, &acc_buf)?;
    let next_buf = f.alloca(&acc_lty, acc_align);

    let idx_ptr = f.alloca("i64", 8);
    f.emit(format!("store i64 0, ptr {idx_ptr}, align 8"));
    let cond_bb = f.lbl("fold.cond");
    let body_bb = f.lbl("fold.body");
    let done_bb = f.lbl("fold.done");
    f.br(&cond_bb);

    f.start_bb(&cond_bb);
    let idx = f.reg();
    f.emit(format!("{idx} = load i64, ptr {idx_ptr}, align 8"));
    let cmp = f.reg();
    f.emit(format!("{cmp} = icmp ult i64 {idx}, {len}"));
    f.cond_br(&cmp, &body_bb, &done_bb);

    f.start_bb(&body_bb);
    let sp = emit_gep_bytes(f, m.target, m.proof_facts, &elem_lty, &src_dp, &idx);
    let mut cargs = vec![format!("ptr {ctx_ptr}")];
    if is_scalar_abi(&acc_ty) {
        let av = emit_load_scalar(f, &acc_ty, &acc_buf, m.target)?;
        cargs.push(format!("{acc_lty} {av}"));
    } else {
        cargs.push(format!("ptr {acc_buf}"));
    }
    if is_scalar_abi(&elem_ty) {
        let ev = emit_load_scalar(f, &elem_ty, &sp, m.target)?;
        cargs.push(format!("{elem_lty} {ev}"));
    } else {
        cargs.push(format!("ptr {sp}"));
    }
    for bv in &broadcasts {
        if is_scalar_abi(&bv.ty) {
            cargs.push(format!("{} {}", llvm_type(&bv.ty, m.target)?, bv.ssa));
        } else {
            cargs.push(format!("ptr {}", bv.ssa));
        }
    }
    for l in &knode.learnable {
        let c = &knode.consts[l.lo as usize];
        let iv = emit_const_val(f, m, &c.ty, &c.bytes)?;
        cargs.push(format!("{} {}", llvm_type(&c.ty, m.target)?, iv.ssa));
    }
    cargs.push(format!("ptr {next_buf}"));
    f.emit(format!("call void @vgi_{kprefix}({})", cargs.join(", ")));

    if is_scalar_abi(&acc_ty) {
        let nv = emit_load_scalar(f, &acc_ty, &next_buf, m.target)?;
        f.emit(format!(
            "store {acc_lty} {nv}, ptr {acc_buf}, align {acc_align}"
        ));
    } else {
        let sz = size_of(&acc_ty, m.target)?;
        emit_memcpy(f, m, &acc_buf, &next_buf, sz);
    }

    let next = f.reg();
    f.emit(format!("{next} = add nuw i64 {idx}, 1"));
    f.emit(format!("store i64 {next}, ptr {idx_ptr}, align 8"));
    f.br(&cond_bb);

    f.start_bb(&done_bb);
    let ssa = if is_scalar_abi(&acc_ty) {
        emit_load_scalar(f, &acc_ty, &acc_buf, m.target)?
    } else {
        acc_buf
    };
    Ok(vec![IrVal { ty: acc_ty, ssa }])
}

#[allow(clippy::too_many_arguments)]
fn emit_generate(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    graph: &CallGraph,
    node: &Node,
    an: &NodeAnalysis,
    op_idx: usize,
    op: &Op,
    ctx_ptr: &str,
    scratch_base: Option<&str>,
    inputs: &[IrVal],
    op_vals: &[Vec<IrVal>],
) -> Result<Vec<IrVal>, Diagnostic> {
    let Operand::Callee(ci) = op.operands[0] else {
        unreachable!()
    };
    let kh = node.calls[ci as usize];
    let knode = &graph.by_hash[&kh];
    let kprefix = &graph.prefixes[&kh];

    let count = operand_val(f, m, node, op.operands[1], inputs, op_vals)?;
    let mut broadcasts = Vec::new();
    for &bo in &op.operands[2..] {
        broadcasts.push(operand_val(f, m, node, bo, inputs, op_vals)?);
    }

    let out_ty = op.results[0].clone();
    let out_elem_ty = out_ty.elem().expect("gen elem").clone();
    let out_elem_lty = llvm_type(&out_elem_ty, m.target)?;
    let out_elem_sz = size_of(&out_elem_ty, m.target)?;
    let out_buf = alloc_result_buf(f, m, an, op_idx, 0, &out_ty, scratch_base)?;

    let dst_dp = if matches!(out_ty, Type::Slice(_)) {
        let bytes = f.reg();
        f.emit(format!("{bytes} = mul i64 {}, {out_elem_sz}", count.ssa));
        let dp = f.reg();
        f.emit(format!(
            "{dp} = call ptr @vg_arena_alloc(ptr {ctx_ptr}, i64 {bytes})"
        ));
        emit_store_slice(f, &out_buf, &dp, &count.ssa);
        dp
    } else {
        out_buf.clone()
    };

    let idx_ptr = f.alloca("i64", 8);
    f.emit(format!("store i64 0, ptr {idx_ptr}, align 8"));
    let cond_bb = f.lbl("gen.cond");
    let body_bb = f.lbl("gen.body");
    let done_bb = f.lbl("gen.done");
    f.br(&cond_bb);

    f.start_bb(&cond_bb);
    let idx = f.reg();
    f.emit(format!("{idx} = load i64, ptr {idx_ptr}, align 8"));
    let cmp = f.reg();
    f.emit(format!("{cmp} = icmp ult i64 {idx}, {}", count.ssa));
    f.cond_br(&cmp, &body_bb, &done_bb);

    f.start_bb(&body_bb);
    let dp = emit_gep_bytes(f, m.target, m.proof_facts, &out_elem_lty, &dst_dp, &idx);
    let mut cargs = vec![format!("ptr {ctx_ptr}"), format!("i64 {idx}")];
    for bv in &broadcasts {
        if is_scalar_abi(&bv.ty) {
            cargs.push(format!("{} {}", llvm_type(&bv.ty, m.target)?, bv.ssa));
        } else {
            cargs.push(format!("ptr {}", bv.ssa));
        }
    }
    for l in &knode.learnable {
        let c = &knode.consts[l.lo as usize];
        let iv = emit_const_val(f, m, &c.ty, &c.bytes)?;
        cargs.push(format!("{} {}", llvm_type(&c.ty, m.target)?, iv.ssa));
    }
    cargs.push(format!("ptr {dp}"));
    f.emit(format!("call void @vgi_{kprefix}({})", cargs.join(", ")));

    let next = f.reg();
    f.emit(format!("{next} = add nuw i64 {idx}, 1"));
    f.emit(format!("store i64 {next}, ptr {idx_ptr}, align 8"));
    m.has_vec_loop = true;
    f.emit(format!("br label %{cond_bb}, !llvm.loop !0"));
    f.start_bb(&done_bb);
    Ok(vec![IrVal {
        ty: out_ty,
        ssa: out_buf,
    }])
}

#[allow(clippy::too_many_arguments)]
fn emit_assert_guard(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    graph: &CallGraph,
    prefix: &str,
    node: &Node,
    an: &NodeAnalysis,
    op_idx: usize,
    op: &Op,
    ctx_ptr: &str,
    scratch_base: Option<&str>,
    inputs: &[IrVal],
    op_vals: &[Vec<IrVal>],
) -> Result<Vec<IrVal>, Diagnostic> {
    // Operands: [cond, Callee(then), a..., Callee(else)]
    let cond = operand_val(f, m, node, op.operands[0], inputs, op_vals)?;
    let Operand::Callee(ct) = op.operands[1] else {
        unreachable!()
    };
    let Operand::Callee(ce) = *op.operands.last().unwrap() else {
        unreachable!()
    };
    let th = node.calls[ct as usize];
    let eh = node.calls[ce as usize];
    let tnode = &graph.by_hash[&th];
    let enode = &graph.by_hash[&eh];
    let tprefix = &graph.prefixes[&th];
    let eprefix = &graph.prefixes[&eh];

    let mut args = Vec::new();
    for &ao in &op.operands[2..op.operands.len() - 1] {
        args.push(operand_val(f, m, node, ao, inputs, op_vals)?);
    }

    let mut res_bufs = Vec::new();
    for (slot, ty) in op.results.iter().enumerate() {
        if is_erased(ty) {
            res_bufs.push(None);
        } else {
            res_bufs.push(Some(alloc_result_buf(
                f,
                m,
                an,
                op_idx,
                slot,
                ty,
                scratch_base,
            )?));
        }
    }

    let emit_branch_call = |f: &mut FnBuilder,
                            m: &mut ModBuilder,
                            bnode: &Node,
                            bprefix: &str|
     -> Result<(), Diagnostic> {
        let mut cargs = vec![format!("ptr {ctx_ptr}")];
        for av in &args {
            if is_erased(&av.ty) {
                continue;
            }
            if is_scalar_abi(&av.ty) {
                cargs.push(format!("{} {}", llvm_type(&av.ty, m.target)?, av.ssa));
            } else {
                cargs.push(format!("ptr {}", av.ssa));
            }
        }
        for l in &bnode.learnable {
            let c = &bnode.consts[l.lo as usize];
            let iv = emit_const_val(f, m, &c.ty, &c.bytes)?;
            cargs.push(format!("{} {}", llvm_type(&c.ty, m.target)?, iv.ssa));
        }
        for ob in res_bufs.iter().flatten() {
            cargs.push(format!("ptr {ob}"));
        }
        f.emit(format!("call void @vgi_{bprefix}({})", cargs.join(", ")));
        Ok(())
    };

    if op.mode == 0 && is_const_bool(node, op.operands[0], true) {
        // Proven true guard erased (`docs/06`)
        emit_branch_call(f, m, tnode, tprefix)?;
    } else {
        let cond_i1 = f.reg();
        f.emit(format!("{cond_i1} = icmp ne i8 {}, 0", cond.ssa));
        let then_bb = f.lbl("guard.then");
        let else_bb = f.lbl("guard.else");
        let done_bb = f.lbl("guard.done");
        f.cond_br(&cond_i1, &then_bb, &else_bb);

        f.start_bb(&then_bb);
        emit_branch_call(f, m, tnode, tprefix)?;
        f.br(&done_bb);

        f.start_bb(&else_bb);
        if op.mode == 0
            && let Some(ginfo) = an.guards.get(&op_idx)
        {
            let gid = m.intern_cstr(&ginfo.guard_id);
            let gob = m.intern_cstr(&ginfo.obligation);
            let gn = m.intern_cstr(prefix);
            f.emit(format!(
                "call void @vg_emit_guard(ptr {ctx_ptr}, ptr {gid}, ptr {gob}, ptr {gn})"
            ));
        }
        emit_branch_call(f, m, enode, eprefix)?;
        f.br(&done_bb);

        f.start_bb(&done_bb);
    }

    let mut outs = Vec::new();
    for (ty, ob) in op.results.iter().zip(&res_bufs) {
        if let Some(buf) = ob {
            let ssa = if is_scalar_abi(ty) {
                emit_load_scalar(f, ty, buf, m.target)?
            } else {
                buf.clone()
            };
            outs.push(IrVal {
                ty: ty.clone(),
                ssa,
            });
        } else {
            outs.push(IrVal {
                ty: Type::Eff,
                ssa: String::new(),
            });
        }
    }
    Ok(outs)
}

#[allow(clippy::too_many_arguments)]
fn emit_call_hash(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    graph: &CallGraph,
    node: &Node,
    an: &NodeAnalysis,
    op_idx: usize,
    op: &Op,
    ctx_ptr: &str,
    scratch_base: Option<&str>,
    inputs: &[IrVal],
    op_vals: &[Vec<IrVal>],
) -> Result<Vec<IrVal>, Diagnostic> {
    let Operand::Callee(ci) = op.operands[0] else {
        unreachable!()
    };
    let ch = node.calls[ci as usize];
    let cnode = &graph.by_hash[&ch];
    let cprefix = &graph.prefixes[&ch];

    let mut cargs = vec![format!("ptr {ctx_ptr}")];
    for &ao in &op.operands[1..] {
        let av = operand_val(f, m, node, ao, inputs, op_vals)?;
        if is_erased(&av.ty) {
            continue;
        }
        if is_scalar_abi(&av.ty) {
            cargs.push(format!("{} {}", llvm_type(&av.ty, m.target)?, av.ssa));
        } else {
            cargs.push(format!("ptr {}", av.ssa));
        }
    }
    for l in &cnode.learnable {
        let c = &cnode.consts[l.lo as usize];
        let iv = emit_const_val(f, m, &c.ty, &c.bytes)?;
        cargs.push(format!("{} {}", llvm_type(&c.ty, m.target)?, iv.ssa));
    }

    let mut res_bufs = Vec::new();
    for (slot, ty) in op.results.iter().enumerate() {
        if is_erased(ty) {
            res_bufs.push(None);
        } else {
            let buf = alloc_result_buf(f, m, an, op_idx, slot, ty, scratch_base)?;
            cargs.push(format!("ptr {buf}"));
            res_bufs.push(Some(buf));
        }
    }

    f.emit(format!("call void @vgi_{cprefix}({})", cargs.join(", ")));

    let mut outs = Vec::new();
    for (ty, ob) in op.results.iter().zip(&res_bufs) {
        if let Some(buf) = ob {
            let ssa = if is_scalar_abi(ty) {
                emit_load_scalar(f, ty, buf, m.target)?
            } else {
                buf.clone()
            };
            outs.push(IrVal {
                ty: ty.clone(),
                ssa,
            });
        } else {
            outs.push(IrVal {
                ty: Type::Eff,
                ssa: String::new(),
            });
        }
    }
    Ok(outs)
}

fn emit_validate_encoding(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ty: &Type,
    val: &IrVal,
    bad_bb: &str,
) -> Result<(), Diagnostic> {
    if !needs_input_validation(ty) {
        return Ok(());
    }
    match ty {
        Type::Bool => {
            let ok = f.reg();
            f.emit(format!("{ok} = icmp ule i8 {}, 1", val.ssa));
            let next_bb = f.lbl("enc.ok");
            f.cond_br(&ok, &next_bb, bad_bb);
            f.start_bb(&next_bb);
        }
        Type::Float(16) => {
            let abs = f.reg();
            f.emit(format!("{abs} = and i16 {}, 32767", val.ssa));
            let is_nan = f.reg();
            f.emit(format!("{is_nan} = icmp ugt i16 {abs}, 31744"));
            let is_canon = f.reg();
            f.emit(format!("{is_canon} = icmp eq i16 {}, 32256", val.ssa));
            let not_nan = f.reg();
            f.emit(format!("{not_nan} = xor i1 {is_nan}, true"));
            let ok = f.reg();
            f.emit(format!("{ok} = or i1 {not_nan}, {is_canon}"));
            let next_bb = f.lbl("enc.ok");
            f.cond_br(&ok, &next_bb, bad_bb);
            f.start_bb(&next_bb);
        }
        Type::BFloat16 => {
            let abs = f.reg();
            f.emit(format!("{abs} = and i16 {}, 32767", val.ssa));
            let is_nan = f.reg();
            f.emit(format!("{is_nan} = icmp ugt i16 {abs}, 32640"));
            let is_canon = f.reg();
            f.emit(format!("{is_canon} = icmp eq i16 {}, 32704", val.ssa));
            let not_nan = f.reg();
            f.emit(format!("{not_nan} = xor i1 {is_nan}, true"));
            let ok = f.reg();
            f.emit(format!("{ok} = or i1 {not_nan}, {is_canon}"));
            let next_bb = f.lbl("enc.ok");
            f.cond_br(&ok, &next_bb, bad_bb);
            f.start_bb(&next_bb);
        }
        Type::Float(32) => {
            let bits = f.reg();
            f.emit(format!("{bits} = bitcast float {} to i32", val.ssa));
            let abs = f.reg();
            f.emit(format!("{abs} = and i32 {bits}, 2147483647"));
            let is_nan = f.reg();
            f.emit(format!("{is_nan} = icmp ugt i32 {abs}, 2139095040"));
            let is_canon = f.reg();
            f.emit(format!("{is_canon} = icmp eq i32 {bits}, 2143289344"));
            let not_nan = f.reg();
            f.emit(format!("{not_nan} = xor i1 {is_nan}, true"));
            let ok = f.reg();
            f.emit(format!("{ok} = or i1 {not_nan}, {is_canon}"));
            let next_bb = f.lbl("enc.ok");
            f.cond_br(&ok, &next_bb, bad_bb);
            f.start_bb(&next_bb);
        }
        Type::Float(64) => {
            let bits = f.reg();
            f.emit(format!("{bits} = bitcast double {} to i64", val.ssa));
            let abs = f.reg();
            f.emit(format!("{abs} = and i64 {bits}, 9223372036854775807"));
            let is_nan = f.reg();
            f.emit(format!(
                "{is_nan} = icmp ugt i64 {abs}, 9218868437227405312"
            ));
            let is_canon = f.reg();
            f.emit(format!(
                "{is_canon} = icmp eq i64 {bits}, 9221120237041090560"
            ));
            let not_nan = f.reg();
            f.emit(format!("{not_nan} = xor i1 {is_nan}, true"));
            let ok = f.reg();
            f.emit(format!("{ok} = or i1 {not_nan}, {is_canon}"));
            let next_bb = f.lbl("enc.ok");
            f.cond_br(&ok, &next_bb, bad_bb);
            f.start_bb(&next_bb);
        }
        Type::Array(elem, n) => {
            emit_validate_seq(f, m, elem, &val.ssa, &format!("{n}"), bad_bb)?;
        }
        Type::Tensor(elem, dims) => {
            let cnt = tensor_elem_count(dims)?;
            emit_validate_seq(f, m, elem, &val.ssa, &format!("{cnt}"), bad_bb)?;
        }
        Type::Slice(elem) => {
            let (dp, len) = emit_slice_parts(f, &val.ssa);
            emit_validate_seq(f, m, elem, &dp, &len, bad_bb)?;
        }
        Type::Tuple(ms) => {
            let lty = llvm_type(ty, m.target)?;
            for (idx, member) in ms.iter().enumerate() {
                if !needs_input_validation(member) {
                    continue;
                }
                let mp = f.reg();
                f.emit(format!(
                    "{mp} = getelementptr inbounds {lty}, ptr {}, i32 0, i32 {idx}",
                    val.ssa
                ));
                let mv = if is_scalar_abi(member) {
                    IrVal {
                        ty: member.clone(),
                        ssa: emit_load_scalar(f, member, &mp, m.target)?,
                    }
                } else {
                    IrVal {
                        ty: member.clone(),
                        ssa: mp,
                    }
                };
                emit_validate_encoding(f, m, member, &mv, bad_bb)?;
            }
        }
        Type::Result(ok_t, err_t) => {
            let lty = llvm_type(ty, m.target)?;
            let tp = f.reg();
            f.emit(format!(
                "{tp} = getelementptr inbounds {lty}, ptr {}, i32 0, i32 0",
                val.ssa
            ));
            let tag = f.reg();
            f.emit(format!("{tag} = load i8, ptr {tp}, align 1"));
            let tag_ok = f.reg();
            f.emit(format!("{tag_ok} = icmp ule i8 {tag}, 1"));
            let pay_bb = f.lbl("enc.res_pay");
            f.cond_br(&tag_ok, &pay_bb, bad_bb);
            f.start_bb(&pay_bb);

            let is_ok = f.reg();
            f.emit(format!("{is_ok} = icmp ne i8 {tag}, 0"));
            let pp = f.reg();
            f.emit(format!(
                "{pp} = getelementptr inbounds {lty}, ptr {}, i32 0, i32 1",
                val.ssa
            ));
            let ok_bb = f.lbl("enc.res_ok");
            let err_bb = f.lbl("enc.res_err");
            let done_bb = f.lbl("enc.res_done");
            f.cond_br(&is_ok, &ok_bb, &err_bb);

            f.start_bb(&ok_bb);
            if needs_input_validation(ok_t) {
                let ov = if is_scalar_abi(ok_t) {
                    IrVal {
                        ty: (**ok_t).clone(),
                        ssa: emit_load_scalar(f, ok_t, &pp, m.target)?,
                    }
                } else {
                    IrVal {
                        ty: (**ok_t).clone(),
                        ssa: pp.clone(),
                    }
                };
                emit_validate_encoding(f, m, ok_t, &ov, bad_bb)?;
            }
            f.br(&done_bb);

            f.start_bb(&err_bb);
            if needs_input_validation(err_t) {
                let ev = if is_scalar_abi(err_t) {
                    IrVal {
                        ty: (**err_t).clone(),
                        ssa: emit_load_scalar(f, err_t, &pp, m.target)?,
                    }
                } else {
                    IrVal {
                        ty: (**err_t).clone(),
                        ssa: pp,
                    }
                };
                emit_validate_encoding(f, m, err_t, &ev, bad_bb)?;
            }
            f.br(&done_bb);

            f.start_bb(&done_bb);
        }
        _ => {}
    }
    Ok(())
}

fn emit_validate_seq(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    elem: &Type,
    data_ptr: &str,
    len_i64: &str,
    bad_bb: &str,
) -> Result<(), Diagnostic> {
    if !needs_input_validation(elem) {
        return Ok(());
    }
    let elem_lty = llvm_type(elem, m.target)?;
    let idx_ptr = f.alloca("i64", 8);
    f.emit(format!("store i64 0, ptr {idx_ptr}, align 8"));
    let cond_bb = f.lbl("encseq.cond");
    let body_bb = f.lbl("encseq.body");
    let done_bb = f.lbl("encseq.done");
    f.br(&cond_bb);

    f.start_bb(&cond_bb);
    let idx = f.reg();
    f.emit(format!("{idx} = load i64, ptr {idx_ptr}, align 8"));
    let cmp = f.reg();
    f.emit(format!("{cmp} = icmp ult i64 {idx}, {len_i64}"));
    f.cond_br(&cmp, &body_bb, &done_bb);

    f.start_bb(&body_bb);
    let ep = emit_gep_bytes(f, m.target, true, &elem_lty, data_ptr, &idx);
    let ev = if is_scalar_abi(elem) {
        IrVal {
            ty: elem.clone(),
            ssa: emit_load_scalar(f, elem, &ep, m.target)?,
        }
    } else {
        IrVal {
            ty: elem.clone(),
            ssa: ep,
        }
    };
    emit_validate_encoding(f, m, elem, &ev, bad_bb)?;
    let next = f.reg();
    f.emit(format!("{next} = add nuw i64 {idx}, 1"));
    f.emit(format!("store i64 {next}, ptr {idx_ptr}, align 8"));
    f.br(&cond_bb);

    f.start_bb(&done_bb);
    Ok(())
}

fn emit_checked_entry(
    m: &mut ModBuilder,
    graph: &CallGraph,
    _analyses: &BTreeMap<Hash, NodeAnalysis>,
) -> Result<(), Diagnostic> {
    let root = &graph.order.last().expect("graph has root").1;
    let prefix = &graph.prefixes[&graph.root_hash];

    let mut sig_params = vec!["ptr %ctx".to_string()];
    let mut arena_args = Vec::new();
    let mut call_args = vec!["ptr %ctx".to_string()];
    let mut inputs: Vec<IrVal> = vec![
        IrVal {
            ty: Type::Eff,
            ssa: String::new(),
        };
        root.input_count()
    ];

    for (i, ty) in root
        .params
        .iter()
        .chain(root.learnable.iter().map(|l| &l.ty))
        .enumerate()
    {
        if is_erased(ty) {
            continue;
        }
        let name = format!("%in{i}");
        if is_scalar_abi(ty) {
            let lty = llvm_type(ty, m.target)?;
            sig_params.push(format!("{lty} {name}"));
            arena_args.push(format!("{lty} {name}"));
            call_args.push(format!("{lty} {name}"));
        } else {
            sig_params.push(format!("ptr readonly {name}"));
            arena_args.push(format!("ptr {name}"));
            call_args.push(format!("ptr {name}"));
        }
        inputs[i] = IrVal {
            ty: ty.clone(),
            ssa: name,
        };
    }

    for (j, ty) in root.results.iter().enumerate() {
        if is_erased(ty) {
            continue;
        }
        let name = format!("%out{j}");
        sig_params.push(format!("ptr {name}"));
        call_args.push(format!("ptr {name}"));
    }

    let mut f = FnBuilder::new();
    let bad_enc_bb = f.lbl("entry.bad_enc");
    let bad_pre_bb = f.lbl("entry.bad_pre");
    let bad_arena_bb = f.lbl("entry.bad_arena");

    // 1. Validate encodings (exit 2)
    for (i, ty) in root
        .params
        .iter()
        .chain(root.learnable.iter().map(|l| &l.ty))
        .enumerate()
    {
        if is_erased(ty) {
            continue;
        }
        emit_validate_encoding(&mut f, m, ty, &inputs[i], &bad_enc_bb)?;
    }

    // 2. Validate learnable bounds & PRE (exit 1)
    for (j, l) in root.learnable.iter().enumerate() {
        let idx = root.params.len() + j;
        let tv = &inputs[idx];
        let lo_c = &root.consts[l.lo as usize];
        let hi_c = &root.consts[l.hi as usize];
        let lo_v = emit_const_val(&mut f, m, &lo_c.ty, &lo_c.bytes)?;
        let hi_v = emit_const_val(&mut f, m, &hi_c.ty, &hi_c.bytes)?;
        let (tl, th, tval, flty) = match l.ty {
            Type::Float(32) => (lo_v.ssa, hi_v.ssa, tv.ssa.clone(), "float"),
            Type::Float(64) => (lo_v.ssa, hi_v.ssa, tv.ssa.clone(), "double"),
            Type::Float(16) => {
                m.require_decl("declare double @vg_f16_to_f64(i16)");
                let r1 = f.reg();
                f.emit(format!(
                    "{r1} = call double @vg_f16_to_f64(i16 {})",
                    lo_v.ssa
                ));
                let r2 = f.reg();
                f.emit(format!(
                    "{r2} = call double @vg_f16_to_f64(i16 {})",
                    hi_v.ssa
                ));
                let r3 = f.reg();
                f.emit(format!("{r3} = call double @vg_f16_to_f64(i16 {})", tv.ssa));
                (r1, r2, r3, "double")
            }
            Type::BFloat16 => {
                m.require_decl("declare double @vg_bf16_to_f64(i16)");
                let r1 = f.reg();
                f.emit(format!(
                    "{r1} = call double @vg_bf16_to_f64(i16 {})",
                    lo_v.ssa
                ));
                let r2 = f.reg();
                f.emit(format!(
                    "{r2} = call double @vg_bf16_to_f64(i16 {})",
                    hi_v.ssa
                ));
                let r3 = f.reg();
                f.emit(format!(
                    "{r3} = call double @vg_bf16_to_f64(i16 {})",
                    tv.ssa
                ));
                (r1, r2, r3, "double")
            }
            _ => unreachable!(),
        };
        let c1 = f.reg();
        f.emit(format!("{c1} = fcmp ole {flty} {tl}, {tval}"));
        let c2 = f.reg();
        f.emit(format!("{c2} = fcmp ole {flty} {tval}, {th}"));
        let ok = f.reg();
        f.emit(format!("{ok} = and i1 {c1}, {c2}"));
        let next_bb = f.lbl("theta.ok");
        f.cond_br(&ok, &next_bb, &bad_pre_bb);
        f.start_bb(&next_bb);
    }

    if !root.contract.pre.is_empty() {
        let mut fcx = FormulaCx {
            node: root,
            graph,
            ctx_ptr: "%ctx",
            inputs: &inputs,
            bound: Vec::new(),
        };
        for term in &root.contract.pre {
            let ok = eval_bool_term(&mut f, m, &mut fcx, term)?;
            let next_bb = f.lbl("pre.ok");
            f.cond_br(&ok, &next_bb, &bad_pre_bb);
            f.start_bb(&next_bb);
        }
    }

    // 3. Check arena space (exit 3)
    let needed = f.reg();
    f.emit(format!(
        "{needed} = call i64 @vg_{prefix}_arena_bytes({})",
        arena_args.join(", ")
    ));
    let has_space = f.reg();
    f.emit(format!(
        "{has_space} = call i32 @vg_arena_has_space(ptr %ctx, i64 {needed})"
    ));
    let space_ok = f.reg();
    f.emit(format!("{space_ok} = icmp ne i32 {has_space}, 0"));
    let run_bb = f.lbl("entry.run");
    f.cond_br(&space_ok, &run_bb, &bad_arena_bb);

    f.start_bb(&run_bb);
    let saved_fp = f.reg();
    f.emit(format!("{saved_fp} = call i64 @vg_fp_enter()"));
    f.emit(format!("call void @vgi_{prefix}({})", call_args.join(", ")));
    f.emit(format!("call void @vg_fp_leave(i64 {saved_fp})"));
    f.ret("i32 0");

    f.start_bb(&bad_enc_bb);
    f.ret("i32 2");

    f.start_bb(&bad_pre_bb);
    f.ret("i32 1");

    f.start_bb(&bad_arena_bb);
    f.ret("i32 3");

    let header = format!("define i32 @vg_{prefix}({})", sig_params.join(", "));
    m.fns.push(f.finish(&header));
    Ok(())
}

fn emit_main_runner(m: &mut ModBuilder, graph: &CallGraph) -> Result<(), Diagnostic> {
    m.require_decl("declare void @vg_io_read_exact(ptr, i64)");
    m.require_decl("declare i32 @vg_io_read_opt_first(ptr, i64)");
    m.require_decl("declare void @vg_io_write_exact(ptr, i64)");
    m.require_decl("declare void @vg_io_flush_stdout()");
    m.require_decl("declare i32 @vg_rt_resolve_handle(ptr, i32, i8)");

    let root = &graph.order.last().expect("graph has root").1;
    let prefix = &graph.prefixes[&graph.root_hash];

    let mut f = FnBuilder::new();
    // Generous opaque vg_ctx buffer (16-byte aligned)
    let ctx_ptr = f.alloca("[128 x i8]", 16);
    let triple_str = m.intern_cstr(m.target.triple());

    // Allocate slots for every non-Eff input & learnable
    let mut in_bufs: Vec<(usize, Type, String)> = Vec::new();
    let mut handle_slots: BTreeMap<usize, u32> = BTreeMap::new();
    let mut handle_ord = 0u32;

    for (i, ty) in root.params.iter().enumerate() {
        if is_erased(ty) {
            continue;
        }
        if let Type::Handle(_) = ty {
            handle_slots.insert(i, handle_ord);
            handle_ord += 1;
            continue;
        }
        let lty = llvm_type(ty, m.target)?;
        let align = align_of(ty, m.target)?;
        in_bufs.push((i, ty.clone(), f.alloca(&lty, align)));
    }
    let learnable_start = root.params.len();
    let mut learn_bufs: Vec<(usize, Type, String)> = Vec::new();
    for (j, l) in root.learnable.iter().enumerate() {
        let lty = llvm_type(&l.ty, m.target)?;
        let align = align_of(&l.ty, m.target)?;
        learn_bufs.push((learnable_start + j, l.ty.clone(), f.alloca(&lty, align)));
    }

    // Allocate temporary ctx pointer to read stdin slices via vg_input_alloc.
    // Wait: initialize ctx early with 0 arena, read inputs, compute needed, or call vg_rt_init first
    // after reading stdin! If we zero-init ctx_ptr before reading stdin, vg_input_alloc tracks allocs
    // even before vg_rt_init? Wait: vg_rt_init does memset(ctx, 0)! So let's keep input slice allocs
    // in a small temporary ctx or read stdin AFTER vg_rt_init?
    // Wait: vg_<prefix>_arena_bytes needs input lengths! So we read stdin FIRST, passing `ptr null`
    // to vg_input_alloc (since process exits right after main anyway, or call vg_rt_init with
    // needed afterwards — passing null avoids vg_rt_init wiping input_allocs!).
    for (_, ty, ptr) in &in_bufs {
        emit_read_stdin_value(&mut f, m, ty, ptr, "null")?;
    }

    if !learn_bufs.is_empty() {
        // Optional learnable trailing bytes: if EOF at first learnable, fill frozen bounds.
        let (_, first_ty, first_ptr) = &learn_bufs[0];
        let first_sz = size_of(first_ty, m.target)?;
        let has_opt = f.reg();
        f.emit(format!(
            "{has_opt} = call i32 @vg_io_read_opt_first(ptr {first_ptr}, i64 {first_sz})"
        ));
        let got = f.reg();
        f.emit(format!("{got} = icmp ne i32 {has_opt}, 0"));
        let read_rest_bb = f.lbl("theta_in.read");
        let def_bb = f.lbl("theta_in.default");
        let done_bb = f.lbl("theta_in.done");
        f.cond_br(&got, &read_rest_bb, &def_bb);

        f.start_bb(&read_rest_bb);
        for (_, ty, ptr) in &learn_bufs[1..] {
            emit_read_stdin_value(&mut f, m, ty, ptr, "null")?;
        }
        f.br(&done_bb);

        f.start_bb(&def_bb);
        for (j, l) in root.learnable.iter().enumerate() {
            let c = &root.consts[l.lo as usize];
            let iv = emit_const_val(&mut f, m, &c.ty, &c.bytes)?;
            let lty = llvm_type(&c.ty, m.target)?;
            let align = align_of(&c.ty, m.target)?;
            f.emit(format!(
                "store {lty} {}, ptr {}, align {align}",
                iv.ssa, learn_bufs[j].2
            ));
        }
        f.br(&done_bb);

        f.start_bb(&done_bb);
    }

    // Build arguments for arena_bytes and checked entry
    let mut all_in_map: BTreeMap<usize, (Type, String, bool)> = BTreeMap::new();
    for (i, ty, ptr) in &in_bufs {
        all_in_map.insert(*i, (ty.clone(), ptr.clone(), false));
    }
    for (i, ty, ptr) in &learn_bufs {
        all_in_map.insert(*i, (ty.clone(), ptr.clone(), false));
    }

    let mut arena_args = Vec::new();
    for (i, ty) in root
        .params
        .iter()
        .chain(root.learnable.iter().map(|l| &l.ty))
        .enumerate()
    {
        if is_erased(ty) {
            continue;
        }
        if matches!(ty, Type::Handle(_)) {
            arena_args.push("i32 0".to_string());
            continue;
        }
        let (_, ptr, _) = &all_in_map[&i];
        if is_scalar_abi(ty) {
            let v = emit_load_scalar(&mut f, ty, ptr, m.target)?;
            arena_args.push(format!("{} {v}", llvm_type(ty, m.target)?));
        } else {
            arena_args.push(format!("ptr {ptr}"));
        }
    }

    let needed = f.reg();
    f.emit(format!(
        "{needed} = call i64 @vg_{prefix}_arena_bytes({})",
        arena_args.join(", ")
    ));
    f.emit(format!(
        "call void @vg_rt_init(ptr {ctx_ptr}, i64 {needed}, ptr {triple_str})"
    ));

    // Resolve handles now that ctx is initialized
    let mut entry_args = vec![format!("ptr {ctx_ptr}")];
    for (i, ty) in root
        .params
        .iter()
        .chain(root.learnable.iter().map(|l| &l.ty))
        .enumerate()
    {
        if is_erased(ty) {
            continue;
        }
        if let Type::Handle(hk) = ty {
            let ord = handle_slots[&i];
            let kind_code = match hk {
                Handle::FileIn => 1u8,
                Handle::FileOut => 2u8,
                Handle::Clock => 3u8,
                _ => 0u8,
            };
            let hid = f.reg();
            f.emit(format!(
                "{hid} = call i32 @vg_rt_resolve_handle(ptr {ctx_ptr}, i32 {ord}, i8 {kind_code})"
            ));
            entry_args.push(format!("i32 {hid}"));
            continue;
        }
        let (_, ptr, _) = &all_in_map[&i];
        if is_scalar_abi(ty) {
            let v = emit_load_scalar(&mut f, ty, ptr, m.target)?;
            entry_args.push(format!("{} {v}", llvm_type(ty, m.target)?));
        } else {
            entry_args.push(format!("ptr {ptr}"));
        }
    }

    let mut out_bufs = Vec::new();
    for ty in &root.results {
        if is_erased(ty) {
            continue;
        }
        let lty = llvm_type(ty, m.target)?;
        let align = align_of(ty, m.target)?;
        let buf = f.alloca(&lty, align);
        entry_args.push(format!("ptr {buf}"));
        out_bufs.push((ty.clone(), buf));
    }

    let rc = f.reg();
    f.emit(format!(
        "{rc} = call i32 @vg_{prefix}({})",
        entry_args.join(", ")
    ));
    let ok = f.reg();
    f.emit(format!("{ok} = icmp eq i32 {rc}, 0"));
    let write_bb = f.lbl("main.write");
    let fail_bb = f.lbl("main.fail");
    f.cond_br(&ok, &write_bb, &fail_bb);

    f.start_bb(&write_bb);
    for (ty, buf) in &out_bufs {
        emit_write_stdout_value(&mut f, m, ty, buf)?;
    }
    f.emit("call void @vg_io_flush_stdout()");
    f.emit(format!("call void @vg_rt_destroy(ptr {ctx_ptr})"));
    f.ret("i32 0");

    f.start_bb(&fail_bb);
    f.emit(format!("call void @vg_rt_destroy(ptr {ctx_ptr})"));
    f.ret(&format!("i32 {rc}"));

    m.fns
        .push(f.finish("define i32 @main(i32 %argc, ptr %argv)"));
    Ok(())
}

fn emit_read_stdin_value(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ty: &Type,
    dst_ptr: &str,
    ctx_ptr: &str,
) -> Result<(), Diagnostic> {
    if let Some(packed_sz) = docs13_fixed_byte_size(ty)? {
        if packed_sz > 0 {
            f.emit(format!(
                "call void @vg_io_read_exact(ptr {dst_ptr}, i64 {packed_sz})"
            ));
        }
        return Ok(());
    }
    match ty {
        Type::Array(elem, n) => {
            let elem_lty = llvm_type(elem, m.target)?;
            for idx in 0..u64::from(*n) {
                let ep = emit_gep_bytes(f, m.target, true, &elem_lty, dst_ptr, &format!("{idx}"));
                emit_read_stdin_value(f, m, elem, &ep, ctx_ptr)?;
            }
            Ok(())
        }
        Type::Tensor(elem, dims) => {
            let cnt = tensor_elem_count(dims)?;
            let elem_lty = llvm_type(elem, m.target)?;
            let i_ptr = f.alloca("i64", 8);
            f.emit(format!("store i64 0, ptr {i_ptr}, align 8"));
            let cond_bb = f.lbl("rd_ten.cond");
            let body_bb = f.lbl("rd_ten.body");
            let done_bb = f.lbl("rd_ten.done");
            f.br(&cond_bb);
            f.start_bb(&cond_bb);
            let idx = f.reg();
            f.emit(format!("{idx} = load i64, ptr {i_ptr}, align 8"));
            let cmp = f.reg();
            f.emit(format!("{cmp} = icmp ult i64 {idx}, {cnt}"));
            f.cond_br(&cmp, &body_bb, &done_bb);
            f.start_bb(&body_bb);
            let ep = emit_gep_bytes(f, m.target, true, &elem_lty, dst_ptr, &idx);
            emit_read_stdin_value(f, m, elem, &ep, ctx_ptr)?;
            let next = f.reg();
            f.emit(format!("{next} = add nuw i64 {idx}, 1"));
            f.emit(format!("store i64 {next}, ptr {i_ptr}, align 8"));
            f.br(&cond_bb);
            f.start_bb(&done_bb);
            Ok(())
        }
        Type::Slice(elem) => {
            let len_ptr = f.alloca("i64", 8);
            f.emit(format!("call void @vg_io_read_exact(ptr {len_ptr}, i64 8)"));
            let len = f.reg();
            f.emit(format!("{len} = load i64, ptr {len_ptr}, align 8"));
            let elem_sz = size_of(elem, m.target)?;
            let elem_lty = llvm_type(elem, m.target)?;
            let bytes = f.reg();
            f.emit(format!("{bytes} = mul i64 {len}, {elem_sz}"));
            let dp = f.reg();
            f.emit(format!(
                "{dp} = call ptr @vg_input_alloc(ptr {ctx_ptr}, i64 {bytes})"
            ));
            emit_store_slice(f, dst_ptr, &dp, &len);
            if let Some(packed_elem) = docs13_fixed_byte_size(elem)? {
                let total_bytes = f.reg();
                f.emit(format!("{total_bytes} = mul i64 {len}, {packed_elem}"));
                let nz = f.reg();
                f.emit(format!("{nz} = icmp ne i64 {total_bytes}, 0"));
                let rd_bb = f.lbl("rd_sl.bulk");
                let done_bb = f.lbl("rd_sl.done");
                f.cond_br(&nz, &rd_bb, &done_bb);
                f.start_bb(&rd_bb);
                f.emit(format!(
                    "call void @vg_io_read_exact(ptr {dp}, i64 {total_bytes})"
                ));
                f.br(&done_bb);
                f.start_bb(&done_bb);
            } else {
                let i_ptr = f.alloca("i64", 8);
                f.emit(format!("store i64 0, ptr {i_ptr}, align 8"));
                let cond_bb = f.lbl("rd_sl.cond");
                let body_bb = f.lbl("rd_sl.body");
                let done_bb = f.lbl("rd_sl.done");
                f.br(&cond_bb);
                f.start_bb(&cond_bb);
                let idx = f.reg();
                f.emit(format!("{idx} = load i64, ptr {i_ptr}, align 8"));
                let cmp = f.reg();
                f.emit(format!("{cmp} = icmp ult i64 {idx}, {len}"));
                f.cond_br(&cmp, &body_bb, &done_bb);
                f.start_bb(&body_bb);
                let ep = emit_gep_bytes(f, m.target, true, &elem_lty, &dp, &idx);
                emit_read_stdin_value(f, m, elem, &ep, ctx_ptr)?;
                let next = f.reg();
                f.emit(format!("{next} = add nuw i64 {idx}, 1"));
                f.emit(format!("store i64 {next}, ptr {i_ptr}, align 8"));
                f.br(&cond_bb);
                f.start_bb(&done_bb);
            }
            Ok(())
        }
        Type::Tuple(ms) => {
            let lty = llvm_type(ty, m.target)?;
            for (idx, member) in ms.iter().enumerate() {
                let mp = f.reg();
                f.emit(format!(
                    "{mp} = getelementptr inbounds {lty}, ptr {dst_ptr}, i32 0, i32 {idx}"
                ));
                emit_read_stdin_value(f, m, member, &mp, ctx_ptr)?;
            }
            Ok(())
        }
        Type::Result(ok_t, err_t) => {
            let lty = llvm_type(ty, m.target)?;
            let tp = f.reg();
            f.emit(format!(
                "{tp} = getelementptr inbounds {lty}, ptr {dst_ptr}, i32 0, i32 0"
            ));
            f.emit(format!("call void @vg_io_read_exact(ptr {tp}, i64 1)"));
            let tag = f.reg();
            f.emit(format!("{tag} = load i8, ptr {tp}, align 1"));
            let is_ok = f.reg();
            f.emit(format!("{is_ok} = icmp eq i8 {tag}, 1"));
            let is_err = f.reg();
            f.emit(format!("{is_err} = icmp eq i8 {tag}, 0"));
            let pp = f.reg();
            f.emit(format!(
                "{pp} = getelementptr inbounds {lty}, ptr {dst_ptr}, i32 0, i32 1"
            ));
            let ok_bb = f.lbl("rd_res.ok");
            let chk_err_bb = f.lbl("rd_res.chk_err");
            let err_bb = f.lbl("rd_res.err");
            let done_bb = f.lbl("rd_res.done");
            f.cond_br(&is_ok, &ok_bb, &chk_err_bb);

            f.start_bb(&ok_bb);
            emit_read_stdin_value(f, m, ok_t, &pp, ctx_ptr)?;
            f.br(&done_bb);

            f.start_bb(&chk_err_bb);
            f.cond_br(&is_err, &err_bb, &done_bb);

            f.start_bb(&err_bb);
            emit_read_stdin_value(f, m, err_t, &pp, ctx_ptr)?;
            f.br(&done_bb);

            f.start_bb(&done_bb);
            Ok(())
        }
        _ => Err(Diagnostic::error(
            "VG-T0119",
            Span::default(),
            format!("cannot deserialize {ty} from stdin"),
        )),
    }
}

fn emit_write_stdout_value(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ty: &Type,
    src_ptr: &str,
) -> Result<(), Diagnostic> {
    if let Some(packed_sz) = docs13_fixed_byte_size(ty)? {
        if packed_sz > 0 {
            f.emit(format!(
                "call void @vg_io_write_exact(ptr {src_ptr}, i64 {packed_sz})"
            ));
        }
        return Ok(());
    }
    match ty {
        Type::Array(elem, n) => {
            let elem_lty = llvm_type(elem, m.target)?;
            for idx in 0..u64::from(*n) {
                let ep = emit_gep_bytes(f, m.target, true, &elem_lty, src_ptr, &format!("{idx}"));
                emit_write_stdout_value(f, m, elem, &ep)?;
            }
            Ok(())
        }
        Type::Tensor(elem, dims) => {
            let cnt = tensor_elem_count(dims)?;
            let elem_lty = llvm_type(elem, m.target)?;
            let i_ptr = f.alloca("i64", 8);
            f.emit(format!("store i64 0, ptr {i_ptr}, align 8"));
            let cond_bb = f.lbl("wr_ten.cond");
            let body_bb = f.lbl("wr_ten.body");
            let done_bb = f.lbl("wr_ten.done");
            f.br(&cond_bb);
            f.start_bb(&cond_bb);
            let idx = f.reg();
            f.emit(format!("{idx} = load i64, ptr {i_ptr}, align 8"));
            let cmp = f.reg();
            f.emit(format!("{cmp} = icmp ult i64 {idx}, {cnt}"));
            f.cond_br(&cmp, &body_bb, &done_bb);
            f.start_bb(&body_bb);
            let ep = emit_gep_bytes(f, m.target, true, &elem_lty, src_ptr, &idx);
            emit_write_stdout_value(f, m, elem, &ep)?;
            let next = f.reg();
            f.emit(format!("{next} = add nuw i64 {idx}, 1"));
            f.emit(format!("store i64 {next}, ptr {i_ptr}, align 8"));
            f.br(&cond_bb);
            f.start_bb(&done_bb);
            Ok(())
        }
        Type::Slice(elem) => {
            let (dp, len) = emit_slice_parts(f, src_ptr);
            let len_ptr = f.alloca("i64", 8);
            f.emit(format!("store i64 {len}, ptr {len_ptr}, align 8"));
            f.emit(format!(
                "call void @vg_io_write_exact(ptr {len_ptr}, i64 8)"
            ));
            let elem_lty = llvm_type(elem, m.target)?;
            if let Some(packed_elem) = docs13_fixed_byte_size(elem)? {
                let total_bytes = f.reg();
                f.emit(format!("{total_bytes} = mul i64 {len}, {packed_elem}"));
                let nz = f.reg();
                f.emit(format!("{nz} = icmp ne i64 {total_bytes}, 0"));
                let wr_bb = f.lbl("wr_sl.bulk");
                let done_bb = f.lbl("wr_sl.done");
                f.cond_br(&nz, &wr_bb, &done_bb);
                f.start_bb(&wr_bb);
                f.emit(format!(
                    "call void @vg_io_write_exact(ptr {dp}, i64 {total_bytes})"
                ));
                f.br(&done_bb);
                f.start_bb(&done_bb);
            } else {
                let i_ptr = f.alloca("i64", 8);
                f.emit(format!("store i64 0, ptr {i_ptr}, align 8"));
                let cond_bb = f.lbl("wr_sl.cond");
                let body_bb = f.lbl("wr_sl.body");
                let done_bb = f.lbl("wr_sl.done");
                f.br(&cond_bb);
                f.start_bb(&cond_bb);
                let idx = f.reg();
                f.emit(format!("{idx} = load i64, ptr {i_ptr}, align 8"));
                let cmp = f.reg();
                f.emit(format!("{cmp} = icmp ult i64 {idx}, {len}"));
                f.cond_br(&cmp, &body_bb, &done_bb);
                f.start_bb(&body_bb);
                let ep = emit_gep_bytes(f, m.target, true, &elem_lty, &dp, &idx);
                emit_write_stdout_value(f, m, elem, &ep)?;
                let next = f.reg();
                f.emit(format!("{next} = add nuw i64 {idx}, 1"));
                f.emit(format!("store i64 {next}, ptr {i_ptr}, align 8"));
                f.br(&cond_bb);
                f.start_bb(&done_bb);
            }
            Ok(())
        }
        Type::Tuple(ms) => {
            let lty = llvm_type(ty, m.target)?;
            for (idx, member) in ms.iter().enumerate() {
                let mp = f.reg();
                f.emit(format!(
                    "{mp} = getelementptr inbounds {lty}, ptr {src_ptr}, i32 0, i32 {idx}"
                ));
                emit_write_stdout_value(f, m, member, &mp)?;
            }
            Ok(())
        }
        Type::Result(ok_t, err_t) => {
            let lty = llvm_type(ty, m.target)?;
            let tp = f.reg();
            f.emit(format!(
                "{tp} = getelementptr inbounds {lty}, ptr {src_ptr}, i32 0, i32 0"
            ));
            f.emit(format!("call void @vg_io_write_exact(ptr {tp}, i64 1)"));
            let tag = f.reg();
            f.emit(format!("{tag} = load i8, ptr {tp}, align 1"));
            let is_ok = f.reg();
            f.emit(format!("{is_ok} = icmp ne i8 {tag}, 0"));
            let pp = f.reg();
            f.emit(format!(
                "{pp} = getelementptr inbounds {lty}, ptr {src_ptr}, i32 0, i32 1"
            ));
            let ok_bb = f.lbl("wr_res.ok");
            let err_bb = f.lbl("wr_res.err");
            let done_bb = f.lbl("wr_res.done");
            f.cond_br(&is_ok, &ok_bb, &err_bb);

            f.start_bb(&ok_bb);
            emit_write_stdout_value(f, m, ok_t, &pp)?;
            f.br(&done_bb);

            f.start_bb(&err_bb);
            emit_write_stdout_value(f, m, err_t, &pp)?;
            f.br(&done_bb);

            f.start_bb(&done_bb);
            Ok(())
        }
        _ => Err(Diagnostic::error(
            "VG-T0119",
            Span::default(),
            format!("cannot serialize {ty} to stdout"),
        )),
    }
}
