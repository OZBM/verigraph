//! Compiles contract formulas (ADR 0009 PRE validation and COST `arena_bytes` sizing) to LLVM IR.

use std::collections::BTreeMap;

use vg_core::diag::{Diagnostic, Span};
use vg_core::ir::{Bin, Builtin, Hash, Node, Operand, Resource, Term};
use vg_core::lower::int_value;
use vg_core::types::Type;

use crate::analysis::{
    NodeAnalysis, OP_ASSERT_GUARD, OP_CALL_HASH, OP_CONCAT, OP_GENERATE, OP_SIMD_MAP, OP_UPDATE,
    is_const_bool,
};
use crate::ir::{
    FnBuilder, IrVal, ModBuilder, align_up_dyn, emit_canon_float, emit_const_val, emit_gep_bytes,
    emit_load_scalar, emit_memcpy, emit_slice_parts, emit_store_slice, emit_store_value_canon,
    f32_const_expr, f64_const_expr,
};
use crate::layout::{
    CallGraph, align_of, align_up, docs13_fixed_byte_size, is_erased, is_scalar_abi, llvm_type,
    size_of, tensor_elem_count,
};

#[derive(Clone, Debug)]
pub enum FVal {
    Bool(String),
    Int(String),
    Float(Type, String),
    Agg(Type, String),
}

impl FVal {
    pub fn ty(&self) -> Type {
        match self {
            FVal::Bool(_) => Type::Bool,
            FVal::Int(_) => Type::Int,
            FVal::Float(t, _) => t.clone(),
            FVal::Agg(t, _) => t.clone(),
        }
    }
}

pub struct FormulaCx<'a> {
    pub node: &'a Node,
    pub graph: &'a CallGraph,
    pub ctx_ptr: &'a str,
    pub inputs: &'a [IrVal],
    pub bound: Vec<String>,
}

/// Evaluates a `Bool` contract formula `term` to an `i1` SSA register.
pub fn eval_bool_term(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    cx: &mut FormulaCx<'_>,
    term: &Term,
) -> Result<String, Diagnostic> {
    match eval_term(f, m, cx, term)? {
        FVal::Bool(b) => Ok(b),
        other => Err(Diagnostic::error(
            "VG-C0101",
            Span::default(),
            format!("expected Bool formula, got {:?}", other.ty()),
        )),
    }
}

fn to_f64(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ty: &Type,
    val: &str,
) -> Result<String, Diagnostic> {
    Ok(match ty {
        Type::Float(64) => val.to_string(),
        Type::Float(32) => {
            let r = f.reg();
            f.emit(format!("{r} = fpext float {val} to double"));
            r
        }
        Type::Float(16) => {
            m.require_decl("declare double @vg_f16_to_f64(i16)");
            let r = f.reg();
            f.emit(format!("{r} = call double @vg_f16_to_f64(i16 {val})"));
            r
        }
        Type::BFloat16 => {
            m.require_decl("declare double @vg_bf16_to_f64(i16)");
            let r = f.reg();
            f.emit(format!("{r} = call double @vg_bf16_to_f64(i16 {val})"));
            r
        }
        _ => {
            return Err(Diagnostic::error(
                "VG-C0101",
                Span::default(),
                format!("expected float type, found {ty}"),
            ));
        }
    })
}

fn from_f64(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ty: &Type,
    val: &str,
) -> Result<String, Diagnostic> {
    Ok(match ty {
        Type::Float(64) => val.to_string(),
        Type::Float(32) => {
            let r = f.reg();
            f.emit(format!("{r} = fptrunc double {val} to float"));
            r
        }
        Type::Float(16) => {
            m.require_decl("declare i16 @vg_f64_to_f16(double)");
            let r = f.reg();
            f.emit(format!("{r} = call i16 @vg_f64_to_f16(double {val})"));
            r
        }
        Type::BFloat16 => {
            m.require_decl("declare i16 @vg_f64_to_bf16(double)");
            let r = f.reg();
            f.emit(format!("{r} = call i16 @vg_f64_to_bf16(double {val})"));
            r
        }
        _ => {
            return Err(Diagnostic::error(
                "VG-C0101",
                Span::default(),
                format!("expected float type, found {ty}"),
            ));
        }
    })
}

fn to_arith_float(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ty: &Type,
    val: &str,
) -> Result<(String, &'static str), Diagnostic> {
    match ty {
        Type::Float(32) => Ok((val.to_string(), "float")),
        Type::Float(64) => Ok((val.to_string(), "double")),
        Type::Float(16) => {
            m.require_decl("declare float @vg_f16_to_f32(i16)");
            let r = f.reg();
            f.emit(format!("{r} = call float @vg_f16_to_f32(i16 {val})"));
            Ok((r, "float"))
        }
        Type::BFloat16 => {
            m.require_decl("declare float @vg_bf16_to_f32(i16)");
            let r = f.reg();
            f.emit(format!("{r} = call float @vg_bf16_to_f32(i16 {val})"));
            Ok((r, "float"))
        }
        _ => Err(Diagnostic::error(
            "VG-C0101",
            Span::default(),
            format!("expected float type, found {ty}"),
        )),
    }
}

fn from_arith_float(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ty: &Type,
    val: &str,
) -> Result<String, Diagnostic> {
    match ty {
        Type::Float(32) | Type::Float(64) => Ok(val.to_string()),
        Type::Float(16) => {
            m.require_decl("declare i16 @vg_f32_to_f16(float)");
            let r = f.reg();
            f.emit(format!("{r} = call i16 @vg_f32_to_f16(float {val})"));
            Ok(r)
        }
        Type::BFloat16 => {
            m.require_decl("declare i16 @vg_f32_to_bf16(float)");
            let r = f.reg();
            f.emit(format!("{r} = call i16 @vg_f32_to_bf16(float {val})"));
            Ok(r)
        }
        _ => Err(Diagnostic::error(
            "VG-C0101",
            Span::default(),
            format!("expected float type, found {ty}"),
        )),
    }
}

pub fn ir_val_to_fval(f: &mut FnBuilder, val: &IrVal) -> Result<FVal, Diagnostic> {
    Ok(match &val.ty {
        Type::Bool => {
            let r = f.reg();
            f.emit(format!("{r} = icmp ne i8 {}, 0", val.ssa));
            FVal::Bool(r)
        }
        Type::UInt(w) => {
            let r = f.reg();
            f.emit(format!("{r} = zext i{w} {} to i128", val.ssa));
            FVal::Int(r)
        }
        Type::SInt(w) => {
            let r = f.reg();
            f.emit(format!("{r} = sext i{w} {} to i128", val.ssa));
            FVal::Int(r)
        }
        t if t.is_float() => FVal::Float(t.clone(), val.ssa.clone()),
        t => FVal::Agg(t.clone(), val.ssa.clone()),
    })
}

pub fn fval_to_ir_val(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    fv: &FVal,
    want_ty: &Type,
) -> Result<IrVal, Diagnostic> {
    match (fv, want_ty) {
        (FVal::Bool(b), Type::Bool) => {
            let r = f.reg();
            f.emit(format!("{r} = zext i1 {b} to i8"));
            Ok(IrVal {
                ty: Type::Bool,
                ssa: r,
            })
        }
        (FVal::Int(i), Type::UInt(w) | Type::SInt(w)) => {
            let r = f.reg();
            f.emit(format!("{r} = trunc i128 {i} to i{w}"));
            Ok(IrVal {
                ty: want_ty.clone(),
                ssa: r,
            })
        }
        (FVal::Float(ft, s), t) if ft == t => Ok(IrVal {
            ty: t.clone(),
            ssa: s.clone(),
        }),
        (FVal::Float(ft, s), t) if t.is_float() => {
            let d = to_f64(f, m, ft, s)?;
            let out = from_f64(f, m, t, &d)?;
            Ok(IrVal {
                ty: t.clone(),
                ssa: out,
            })
        }
        (FVal::Agg(_, p), t) => Ok(IrVal {
            ty: t.clone(),
            ssa: p.clone(),
        }),
        _ => Err(Diagnostic::error(
            "VG-C0101",
            Span::default(),
            format!("cannot convert formula value {:?} to {want_ty}", fv.ty()),
        )),
    }
}

pub fn eval_term(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    cx: &mut FormulaCx<'_>,
    term: &Term,
) -> Result<FVal, Diagnostic> {
    match term {
        Term::Input(idx) => {
            let v = cx.inputs.get(*idx as usize).ok_or_else(|| {
                Diagnostic::error("VG-S0203", Span::default(), "formula input out of range")
            })?;
            ir_val_to_fval(f, v)
        }
        Term::Bound(db) => {
            let len = cx.bound.len();
            let idx = len.checked_sub(1 + *db as usize).ok_or_else(|| {
                Diagnostic::error("VG-S0203", Span::default(), "bound variable out of range")
            })?;
            Ok(FVal::Int(cx.bound[idx].clone()))
        }
        Term::Const(ci) => {
            let c = cx.node.consts.get(*ci as usize).ok_or_else(|| {
                Diagnostic::error("VG-S0203", Span::default(), "formula const out of range")
            })?;
            if c.ty == Type::Int {
                let v = int_value(&c.bytes).ok_or_else(|| {
                    Diagnostic::error("VG-S0204", Span::default(), "invalid Int constant bytes")
                })?;
                Ok(FVal::Int(format!("{v}")))
            } else {
                let iv = emit_const_val(f, m, &c.ty, &c.bytes)?;
                ir_val_to_fval(f, &iv)
            }
        }
        Term::Not(inner) => {
            let b = eval_bool_term(f, m, cx, inner)?;
            let r = f.reg();
            f.emit(format!("{r} = xor i1 {b}, true"));
            Ok(FVal::Bool(r))
        }
        Term::Neg(inner) => match eval_term(f, m, cx, inner)? {
            FVal::Int(v) => {
                let r = f.reg();
                f.emit(format!("{r} = sub i128 0, {v}"));
                Ok(FVal::Int(r))
            }
            FVal::Float(ty, v) => {
                let (fv, lty) = to_arith_float(f, m, &ty, &v)?;
                let neg = f.reg();
                f.emit(format!("{neg} = fneg {lty} {fv}"));
                let res = from_arith_float(f, m, &ty, &neg)?;
                Ok(FVal::Float(ty, res))
            }
            other => Err(Diagnostic::error(
                "VG-C0101",
                Span::default(),
                format!("cannot negate {:?}", other.ty()),
            )),
        },
        Term::Bin(op @ (Bin::And | Bin::Or | Bin::Implies), a, b) => {
            eval_bool_shortcircuit(f, m, cx, *op, a, b)
        }
        Term::Bin(op, a, b) => {
            let va = eval_term(f, m, cx, a)?;
            let vb = eval_term(f, m, cx, b)?;
            eval_bin_vals(f, m, *op, &va, &vb)
        }
        Term::Index(base, idxs) => eval_index(f, m, cx, base, idxs),
        Term::Proj(base, j) => {
            let FVal::Agg(Type::Tuple(ms), ptr) = eval_term(f, m, cx, base)? else {
                return Err(Diagnostic::error(
                    "VG-C0101",
                    Span::default(),
                    "PROJ in formula requires a Tuple",
                ));
            };
            let idx = *j as usize;
            let member_ty = ms
                .get(idx)
                .ok_or_else(|| {
                    Diagnostic::error(
                        "VG-S0203",
                        Span::default(),
                        "formula tuple proj out of range",
                    )
                })?
                .clone();
            let tup_lty = llvm_type(&Type::Tuple(ms), m.target)?;
            let mp = f.reg();
            f.emit(format!(
                "{mp} = getelementptr inbounds {tup_lty}, ptr {ptr}, i32 0, i32 {idx}"
            ));
            load_agg_or_scalar_as_fval(f, m, &member_ty, &mp)
        }
        Term::Forall(ranges, body) => eval_forall(f, m, cx, ranges, body),
        Term::Builtin(b, args) => eval_builtin(f, m, cx, *b, args),
        Term::Spec(ci, args) | Term::Apply(ci, args) => eval_call_in_formula(f, m, cx, *ci, args),
        Term::Fold(ci, args) => eval_fold_in_formula(f, m, cx, *ci, args),
        Term::Result(_) | Term::Port(..) | Term::Resource(_) => Err(Diagnostic::error(
            "VG-C0104",
            Span::default(),
            "Result/Port/Resource cannot appear in PRE evaluation",
        )),
    }
}

fn eval_bool_shortcircuit(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    cx: &mut FormulaCx<'_>,
    op: Bin,
    a: &Term,
    b: &Term,
) -> Result<FVal, Diagnostic> {
    let res_ptr = f.alloca("i1", 1);
    let lhs = eval_bool_term(f, m, cx, a)?;
    let rhs_bb = f.lbl("sc.rhs");
    let short_bb = f.lbl("sc.short");
    let done_bb = f.lbl("sc.done");

    let short_val = match op {
        Bin::And => {
            f.cond_br(&lhs, &rhs_bb, &short_bb);
            "false"
        }
        Bin::Or => {
            f.cond_br(&lhs, &short_bb, &rhs_bb);
            "true"
        }
        Bin::Implies => {
            f.cond_br(&lhs, &rhs_bb, &short_bb);
            "true"
        }
        _ => unreachable!(),
    };

    f.start_bb(&short_bb);
    f.emit(format!("store i1 {short_val}, ptr {res_ptr}, align 1"));
    f.br(&done_bb);

    f.start_bb(&rhs_bb);
    let rhs = eval_bool_term(f, m, cx, b)?;
    f.emit(format!("store i1 {rhs}, ptr {res_ptr}, align 1"));
    f.br(&done_bb);

    f.start_bb(&done_bb);
    let out = f.reg();
    f.emit(format!("{out} = load i1, ptr {res_ptr}, align 1"));
    Ok(FVal::Bool(out))
}

fn eval_bin_vals(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    op: Bin,
    va: &FVal,
    vb: &FVal,
) -> Result<FVal, Diagnostic> {
    match (va, vb) {
        (FVal::Int(a), FVal::Int(b)) => Ok(match op {
            Bin::Add => {
                let r = f.reg();
                f.emit(format!("{r} = add i128 {a}, {b}"));
                FVal::Int(r)
            }
            Bin::Sub => {
                let r = f.reg();
                f.emit(format!("{r} = sub i128 {a}, {b}"));
                FVal::Int(r)
            }
            Bin::Mul => {
                let r = f.reg();
                f.emit(format!("{r} = mul i128 {a}, {b}"));
                FVal::Int(r)
            }
            Bin::Div | Bin::Mod => {
                let is_z = f.reg();
                f.emit(format!("{is_z} = icmp eq i128 {b}, 0"));
                let safe_b = f.reg();
                f.emit(format!("{safe_b} = select i1 {is_z}, i128 1, i128 {b}"));
                let instr = if op == Bin::Div { "sdiv" } else { "srem" };
                let r = f.reg();
                f.emit(format!("{r} = {instr} i128 {a}, {safe_b}"));
                FVal::Int(r)
            }
            Bin::Eq | Bin::Ne | Bin::Lt | Bin::Le | Bin::Gt | Bin::Ge => {
                let pred = match op {
                    Bin::Eq => "eq",
                    Bin::Ne => "ne",
                    Bin::Lt => "slt",
                    Bin::Le => "sle",
                    Bin::Gt => "sgt",
                    Bin::Ge => "sge",
                    _ => unreachable!(),
                };
                let r = f.reg();
                f.emit(format!("{r} = icmp {pred} i128 {a}, {b}"));
                FVal::Bool(r)
            }
            _ => unreachable!(),
        }),
        (FVal::Float(ta, a), FVal::Float(tb, b)) if ta == tb => {
            let (fa, lty) = to_arith_float(f, m, ta, a)?;
            let (fb, _) = to_arith_float(f, m, tb, b)?;
            match op {
                Bin::Add | Bin::Sub | Bin::Mul | Bin::Div => {
                    let instr = match op {
                        Bin::Add => "fadd",
                        Bin::Sub => "fsub",
                        Bin::Mul => "fmul",
                        Bin::Div => "fdiv",
                        _ => unreachable!(),
                    };
                    let r = f.reg();
                    f.emit(format!("{r} = {instr} {lty} {fa}, {fb}"));
                    let out = from_arith_float(f, m, ta, &r)?;
                    Ok(FVal::Float(ta.clone(), out))
                }
                Bin::Eq | Bin::Ne | Bin::Lt | Bin::Le | Bin::Gt | Bin::Ge => {
                    let pred = match op {
                        Bin::Eq => "oeq",
                        Bin::Ne => "une",
                        Bin::Lt => "olt",
                        Bin::Le => "ole",
                        Bin::Gt => "ogt",
                        Bin::Ge => "oge",
                        _ => unreachable!(),
                    };
                    let r = f.reg();
                    f.emit(format!("{r} = fcmp {pred} {lty} {fa}, {fb}"));
                    Ok(FVal::Bool(r))
                }
                _ => Err(Diagnostic::error(
                    "VG-C0101",
                    Span::default(),
                    "invalid operator on floats",
                )),
            }
        }
        (FVal::Bool(a), FVal::Bool(b)) if matches!(op, Bin::Eq | Bin::Ne) => {
            let pred = if op == Bin::Eq { "eq" } else { "ne" };
            let r = f.reg();
            f.emit(format!("{r} = icmp {pred} i1 {a}, {b}"));
            Ok(FVal::Bool(r))
        }
        (FVal::Agg(ta, pa), FVal::Agg(tb, pb)) if ta == tb && matches!(op, Bin::Eq | Bin::Ne) => {
            let eq = eval_agg_eq(f, m, ta, pa, pb, false)?;
            if op == Bin::Ne {
                let r = f.reg();
                f.emit(format!("{r} = xor i1 {eq}, true"));
                Ok(FVal::Bool(r))
            } else {
                Ok(FVal::Bool(eq))
            }
        }
        _ => Err(Diagnostic::error(
            "VG-C0101",
            Span::default(),
            format!(
                "incompatible formula operand types: {:?} vs {:?}",
                va.ty(),
                vb.ty()
            ),
        )),
    }
}

fn load_agg_or_scalar_as_fval(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ty: &Type,
    ptr: &str,
) -> Result<FVal, Diagnostic> {
    if is_scalar_abi(ty) {
        let s = emit_load_scalar(f, ty, ptr, m.target)?;
        ir_val_to_fval(
            f,
            &IrVal {
                ty: ty.clone(),
                ssa: s,
            },
        )
    } else {
        Ok(FVal::Agg(ty.clone(), ptr.to_string()))
    }
}

fn eval_agg_eq(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ty: &Type,
    pa: &str,
    pb: &str,
    same_mode: bool,
) -> Result<String, Diagnostic> {
    if is_scalar_abi(ty) {
        let la = emit_load_scalar(f, ty, pa, m.target)?;
        let lb = emit_load_scalar(f, ty, pb, m.target)?;
        let fa = ir_val_to_fval(
            f,
            &IrVal {
                ty: ty.clone(),
                ssa: la,
            },
        )?;
        let fb = ir_val_to_fval(
            f,
            &IrVal {
                ty: ty.clone(),
                ssa: lb,
            },
        )?;
        return eval_scalar_eq_or_same(f, m, &fa, &fb, same_mode);
    }
    match ty {
        Type::Array(elem, n) => eval_seq_eq(f, m, elem, pa, pb, &format!("{n}"), same_mode),
        Type::Tensor(elem, dims) => {
            let cnt = tensor_elem_count(dims)?;
            eval_seq_eq(f, m, elem, pa, pb, &format!("{cnt}"), same_mode)
        }
        Type::Slice(elem) => {
            let (p1, l1) = emit_slice_parts(f, pa);
            let (p2, l2) = emit_slice_parts(f, pb);
            let len_eq = f.reg();
            f.emit(format!("{len_eq} = icmp eq i64 {l1}, {l2}"));
            let res_ptr = f.alloca("i1", 1);
            f.emit(format!("store i1 false, ptr {res_ptr}, align 1"));
            let chk_bb = f.lbl("sleq.elems");
            let done_bb = f.lbl("sleq.done");
            f.cond_br(&len_eq, &chk_bb, &done_bb);

            f.start_bb(&chk_bb);
            let elems_eq = eval_seq_eq(f, m, elem, &p1, &p2, &l1, same_mode)?;
            f.emit(format!("store i1 {elems_eq}, ptr {res_ptr}, align 1"));
            f.br(&done_bb);

            f.start_bb(&done_bb);
            let out = f.reg();
            f.emit(format!("{out} = load i1, ptr {res_ptr}, align 1"));
            Ok(out)
        }
        Type::Tuple(ms) => {
            let lty = llvm_type(ty, m.target)?;
            let res_ptr = f.alloca("i1", 1);
            f.emit(format!("store i1 true, ptr {res_ptr}, align 1"));
            let done_bb = f.lbl("tupeq.done");
            for (idx, member) in ms.iter().enumerate() {
                let fa = f.reg();
                f.emit(format!(
                    "{fa} = getelementptr inbounds {lty}, ptr {pa}, i32 0, i32 {idx}"
                ));
                let fb = f.reg();
                f.emit(format!(
                    "{fb} = getelementptr inbounds {lty}, ptr {pb}, i32 0, i32 {idx}"
                ));
                let meq = eval_agg_eq(f, m, member, &fa, &fb, same_mode)?;
                let next_bb = f.lbl("tupeq.next");
                let fail_bb = f.lbl("tupeq.fail");
                f.cond_br(&meq, &next_bb, &fail_bb);
                f.start_bb(&fail_bb);
                f.emit(format!("store i1 false, ptr {res_ptr}, align 1"));
                f.br(&done_bb);
                f.start_bb(&next_bb);
            }
            f.br(&done_bb);
            f.start_bb(&done_bb);
            let out = f.reg();
            f.emit(format!("{out} = load i1, ptr {res_ptr}, align 1"));
            Ok(out)
        }
        Type::Result(ok_t, err_t) => {
            let lty = llvm_type(ty, m.target)?;
            let ta_p = f.reg();
            f.emit(format!(
                "{ta_p} = getelementptr inbounds {lty}, ptr {pa}, i32 0, i32 0"
            ));
            let tb_p = f.reg();
            f.emit(format!(
                "{tb_p} = getelementptr inbounds {lty}, ptr {pb}, i32 0, i32 0"
            ));
            let ta = f.reg();
            f.emit(format!("{ta} = load i8, ptr {ta_p}, align 1"));
            let tb = f.reg();
            f.emit(format!("{tb} = load i8, ptr {tb_p}, align 1"));
            let tag_eq = f.reg();
            f.emit(format!("{tag_eq} = icmp eq i8 {ta}, {tb}"));
            let res_ptr = f.alloca("i1", 1);
            f.emit(format!("store i1 false, ptr {res_ptr}, align 1"));
            let same_tag_bb = f.lbl("reseq.sametag");
            let ok_bb = f.lbl("reseq.ok");
            let err_bb = f.lbl("reseq.err");
            let done_bb = f.lbl("reseq.done");
            f.cond_br(&tag_eq, &same_tag_bb, &done_bb);

            f.start_bb(&same_tag_bb);
            let is_ok = f.reg();
            f.emit(format!("{is_ok} = icmp ne i8 {ta}, 0"));
            let pay_a = f.reg();
            f.emit(format!(
                "{pay_a} = getelementptr inbounds {lty}, ptr {pa}, i32 0, i32 1"
            ));
            let pay_b = f.reg();
            f.emit(format!(
                "{pay_b} = getelementptr inbounds {lty}, ptr {pb}, i32 0, i32 1"
            ));
            f.cond_br(&is_ok, &ok_bb, &err_bb);

            f.start_bb(&ok_bb);
            let oeq = eval_agg_eq(f, m, ok_t, &pay_a, &pay_b, same_mode)?;
            f.emit(format!("store i1 {oeq}, ptr {res_ptr}, align 1"));
            f.br(&done_bb);

            f.start_bb(&err_bb);
            let eeq = eval_agg_eq(f, m, err_t, &pay_a, &pay_b, same_mode)?;
            f.emit(format!("store i1 {eeq}, ptr {res_ptr}, align 1"));
            f.br(&done_bb);

            f.start_bb(&done_bb);
            let out = f.reg();
            f.emit(format!("{out} = load i1, ptr {res_ptr}, align 1"));
            Ok(out)
        }
        _ => Err(Diagnostic::error(
            "VG-C0101",
            Span::default(),
            format!("cannot compare {ty} in formula"),
        )),
    }
}

fn eval_seq_eq(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    elem: &Type,
    pa: &str,
    pb: &str,
    len_i64: &str,
    same_mode: bool,
) -> Result<String, Diagnostic> {
    let elem_lty = llvm_type(elem, m.target)?;
    let res_ptr = f.alloca("i1", 1);
    f.emit(format!("store i1 true, ptr {res_ptr}, align 1"));
    let i_ptr = f.alloca("i64", 8);
    f.emit(format!("store i64 0, ptr {i_ptr}, align 8"));
    let cond_bb = f.lbl("seqeq.cond");
    let body_bb = f.lbl("seqeq.body");
    let step_bb = f.lbl("seqeq.step");
    let fail_bb = f.lbl("seqeq.fail");
    let done_bb = f.lbl("seqeq.done");
    f.br(&cond_bb);

    f.start_bb(&cond_bb);
    let idx = f.reg();
    f.emit(format!("{idx} = load i64, ptr {i_ptr}, align 8"));
    let cmp = f.reg();
    f.emit(format!("{cmp} = icmp ult i64 {idx}, {len_i64}"));
    f.cond_br(&cmp, &body_bb, &done_bb);

    f.start_bb(&body_bb);
    let ea = emit_gep_bytes(f, m.target, true, &elem_lty, pa, &idx);
    let eb = emit_gep_bytes(f, m.target, true, &elem_lty, pb, &idx);
    let eq = eval_agg_eq(f, m, elem, &ea, &eb, same_mode)?;
    f.cond_br(&eq, &step_bb, &fail_bb);

    f.start_bb(&fail_bb);
    f.emit(format!("store i1 false, ptr {res_ptr}, align 1"));
    f.br(&done_bb);

    f.start_bb(&step_bb);
    let next = f.reg();
    f.emit(format!("{next} = add nuw i64 {idx}, 1"));
    f.emit(format!("store i64 {next}, ptr {i_ptr}, align 8"));
    f.br(&cond_bb);

    f.start_bb(&done_bb);
    let out = f.reg();
    f.emit(format!("{out} = load i1, ptr {res_ptr}, align 1"));
    Ok(out)
}

fn eval_scalar_eq_or_same(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    va: &FVal,
    vb: &FVal,
    same_mode: bool,
) -> Result<String, Diagnostic> {
    if !same_mode {
        let FVal::Bool(b) = eval_bin_vals(f, m, Bin::Eq, va, vb)? else {
            unreachable!();
        };
        return Ok(b);
    }
    match (va, vb) {
        (FVal::Bool(a), FVal::Bool(b)) => {
            let r = f.reg();
            f.emit(format!("{r} = icmp eq i1 {a}, {b}"));
            Ok(r)
        }
        (FVal::Int(a), FVal::Int(b)) => {
            let r = f.reg();
            f.emit(format!("{r} = icmp eq i128 {a}, {b}"));
            Ok(r)
        }
        (FVal::Float(ta, a), FVal::Float(tb, b)) if ta == tb => {
            let ca = emit_canon_float(f, ta, a);
            let cb = emit_canon_float(f, tb, b);
            let r = f.reg();
            match ta {
                Type::Float(16) | Type::BFloat16 => f.emit(format!("{r} = icmp eq i16 {ca}, {cb}")),
                Type::Float(32) => {
                    let ba = f.reg();
                    let bb = f.reg();
                    f.emit(format!("{ba} = bitcast float {ca} to i32"));
                    f.emit(format!("{bb} = bitcast float {cb} to i32"));
                    f.emit(format!("{r} = icmp eq i32 {ba}, {bb}"));
                }
                Type::Float(64) => {
                    let ba = f.reg();
                    let bb = f.reg();
                    f.emit(format!("{ba} = bitcast double {ca} to i64"));
                    f.emit(format!("{bb} = bitcast double {cb} to i64"));
                    f.emit(format!("{r} = icmp eq i64 {ba}, {bb}"));
                }
                _ => unreachable!(),
            }
            Ok(r)
        }
        _ => Err(Diagnostic::error(
            "VG-C0101",
            Span::default(),
            "incompatible types for same()",
        )),
    }
}

fn eval_index(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    cx: &mut FormulaCx<'_>,
    base: &Term,
    idxs: &[Term],
) -> Result<FVal, Diagnostic> {
    let FVal::Agg(base_ty, base_ptr) = eval_term(f, m, cx, base)? else {
        return Err(Diagnostic::error(
            "VG-C0101",
            Span::default(),
            "formula indexing requires an Array, Tensor or Slice",
        ));
    };
    let (elem_ty, flat_i64, data_ptr, bound_len) = match &base_ty {
        Type::Array(elem, n) => {
            let FVal::Int(i) = eval_term(f, m, cx, &idxs[0])? else {
                return Err(Diagnostic::error(
                    "VG-C0101",
                    Span::default(),
                    "index must be integer",
                ));
            };
            let i64v = clamp_index_i64(f, &i, &format!("{n}"));
            ((**elem).clone(), i64v, base_ptr, format!("{n}"))
        }
        Type::Slice(elem) => {
            let FVal::Int(i) = eval_term(f, m, cx, &idxs[0])? else {
                return Err(Diagnostic::error(
                    "VG-C0101",
                    Span::default(),
                    "index must be integer",
                ));
            };
            let (dp, len) = emit_slice_parts(f, &base_ptr);
            let i64v = clamp_index_i64(f, &i, &len);
            ((**elem).clone(), i64v, dp, len)
        }
        Type::Tensor(elem, dims) => {
            let mut flat = "0".to_string();
            for (k, d) in dims.iter().enumerate() {
                let FVal::Int(ik) = eval_term(f, m, cx, &idxs[k])? else {
                    return Err(Diagnostic::error(
                        "VG-C0101",
                        Span::default(),
                        "tensor index must be integer",
                    ));
                };
                if k == 0 {
                    flat = ik;
                } else {
                    let mul = f.reg();
                    f.emit(format!("{mul} = mul i128 {flat}, {d}"));
                    let add = f.reg();
                    f.emit(format!("{add} = add i128 {mul}, {ik}"));
                    flat = add;
                }
            }
            let total = tensor_elem_count(dims)?;
            let i64v = clamp_index_i64(f, &flat, &format!("{total}"));
            ((**elem).clone(), i64v, base_ptr, format!("{total}"))
        }
        _ => {
            return Err(Diagnostic::error(
                "VG-C0101",
                Span::default(),
                format!("cannot index {base_ty}"),
            ));
        }
    };

    let is_empty = f.reg();
    f.emit(format!("{is_empty} = icmp eq i64 {bound_len}, 0"));
    let elem_lty = llvm_type(&elem_ty, m.target)?;
    let elem_align = align_of(&elem_ty, m.target)?;
    let zero_buf = f.alloca(&elem_lty, elem_align);
    let ep = emit_gep_bytes(f, m.target, false, &elem_lty, &data_ptr, &flat_i64);
    let safe_ptr = f.reg();
    f.emit(format!(
        "{safe_ptr} = select i1 {is_empty}, ptr {zero_buf}, ptr {ep}"
    ));
    load_agg_or_scalar_as_fval(f, m, &elem_ty, &safe_ptr)
}

fn clamp_index_i64(f: &mut FnBuilder, idx_i128: &str, len_i64: &str) -> String {
    let len128 = f.reg();
    f.emit(format!("{len128} = zext i64 {len_i64} to i128"));
    let ok_lo = f.reg();
    f.emit(format!("{ok_lo} = icmp sge i128 {idx_i128}, 0"));
    let ok_hi = f.reg();
    f.emit(format!("{ok_hi} = icmp slt i128 {idx_i128}, {len128}"));
    let ok = f.reg();
    f.emit(format!("{ok} = and i1 {ok_lo}, {ok_hi}"));
    let trunc = f.reg();
    f.emit(format!("{trunc} = trunc i128 {idx_i128} to i64"));
    let safe = f.reg();
    f.emit(format!("{safe} = select i1 {ok}, i64 {trunc}, i64 0"));
    safe
}

fn eval_forall(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    cx: &mut FormulaCx<'_>,
    ranges: &[(Term, Term)],
    body: &Term,
) -> Result<FVal, Diagnostic> {
    let res_ptr = f.alloca("i1", 1);
    f.emit(format!("store i1 true, ptr {res_ptr}, align 1"));
    let done_bb = f.lbl("forall.done");
    let depth0 = cx.bound.len();

    fn emit_level(
        f: &mut FnBuilder,
        m: &mut ModBuilder,
        cx: &mut FormulaCx<'_>,
        ranges: &[(Term, Term)],
        body: &Term,
        res_ptr: &str,
        done_bb: &str,
    ) -> Result<(), Diagnostic> {
        if ranges.is_empty() {
            let ok = eval_bool_term(f, m, cx, body)?;
            let next_bb = f.lbl("forall.cont");
            let fail_bb = f.lbl("forall.fail");
            f.cond_br(&ok, &next_bb, &fail_bb);
            f.start_bb(&fail_bb);
            f.emit(format!("store i1 false, ptr {res_ptr}, align 1"));
            f.br(done_bb);
            f.start_bb(&next_bb);
            return Ok(());
        }
        let (lo_t, hi_t) = &ranges[0];
        let FVal::Int(lo) = eval_term(f, m, cx, lo_t)? else {
            return Err(Diagnostic::error(
                "VG-C0101",
                Span::default(),
                "forall range lo must be int",
            ));
        };
        let FVal::Int(hi) = eval_term(f, m, cx, hi_t)? else {
            return Err(Diagnostic::error(
                "VG-C0101",
                Span::default(),
                "forall range hi must be int",
            ));
        };
        let var_ptr = f.alloca("i128", 16);
        f.emit(format!("store i128 {lo}, ptr {var_ptr}, align 16"));

        let cond_bb = f.lbl("forall.cond");
        let loop_bb = f.lbl("forall.body");
        let exit_bb = f.lbl("forall.exit");
        f.br(&cond_bb);

        f.start_bb(&cond_bb);
        let cur = f.reg();
        f.emit(format!("{cur} = load i128, ptr {var_ptr}, align 16"));
        let cmp = f.reg();
        f.emit(format!("{cmp} = icmp slt i128 {cur}, {hi}"));
        f.cond_br(&cmp, &loop_bb, &exit_bb);

        f.start_bb(&loop_bb);
        cx.bound.push(cur.clone());
        emit_level(f, m, cx, &ranges[1..], body, res_ptr, done_bb)?;
        cx.bound.pop();

        let next = f.reg();
        f.emit(format!("{next} = add i128 {cur}, 1"));
        f.emit(format!("store i128 {next}, ptr {var_ptr}, align 16"));
        f.br(&cond_bb);

        f.start_bb(&exit_bb);
        Ok(())
    }

    let r = emit_level(f, m, cx, ranges, body, &res_ptr, &done_bb);
    cx.bound.truncate(depth0);
    r?;
    f.br(&done_bb);

    f.start_bb(&done_bb);
    let out = f.reg();
    f.emit(format!("{out} = load i1, ptr {res_ptr}, align 1"));
    Ok(FVal::Bool(out))
}

fn eval_builtin(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    cx: &mut FormulaCx<'_>,
    b: Builtin,
    args: &[Term],
) -> Result<FVal, Diagnostic> {
    match b {
        Builtin::Len => match eval_term(f, m, cx, &args[0])? {
            FVal::Agg(Type::Array(_, n), _) => Ok(FVal::Int(format!("{n}"))),
            FVal::Agg(Type::Tensor(_, dims), _) => {
                let cnt = tensor_elem_count(&dims)?;
                Ok(FVal::Int(format!("{cnt}")))
            }
            FVal::Agg(Type::Slice(_), p) => {
                let (_, len) = emit_slice_parts(f, &p);
                let r = f.reg();
                f.emit(format!("{r} = zext i64 {len} to i128"));
                Ok(FVal::Int(r))
            }
            _ => Err(Diagnostic::error(
                "VG-C0101",
                Span::default(),
                "len() expects collection",
            )),
        },
        Builtin::IsFinite | Builtin::IsNan => {
            let FVal::Float(ty, v) = eval_term(f, m, cx, &args[0])? else {
                return Err(Diagnostic::error(
                    "VG-C0101",
                    Span::default(),
                    "float expected",
                ));
            };
            let (fv, lty) = to_arith_float(f, m, &ty, &v)?;
            if b == Builtin::IsNan {
                let r = f.reg();
                f.emit(format!("{r} = fcmp uno {lty} {fv}, {fv}"));
                Ok(FVal::Bool(r))
            } else {
                let abs_decl = if lty == "float" {
                    "declare float @llvm.fabs.f32(float)"
                } else {
                    "declare double @llvm.fabs.f64(double)"
                };
                m.require_decl(abs_decl);
                let fn_name = if lty == "float" {
                    "@llvm.fabs.f32"
                } else {
                    "@llvm.fabs.f64"
                };
                let abs = f.reg();
                f.emit(format!("{abs} = call {lty} {fn_name}({lty} {fv})"));
                let inf = if lty == "float" {
                    f32_const_expr(0x7F80_0000)
                } else {
                    f64_const_expr(0x7FF0_0000_0000_0000)
                };
                let r = f.reg();
                f.emit(format!("{r} = fcmp olt {lty} {abs}, {inf}"));
                Ok(FVal::Bool(r))
            }
        }
        Builtin::Abs => match eval_term(f, m, cx, &args[0])? {
            FVal::Int(v) => {
                let neg = f.reg();
                f.emit(format!("{neg} = icmp slt i128 {v}, 0"));
                let nv = f.reg();
                f.emit(format!("{nv} = sub i128 0, {v}"));
                let r = f.reg();
                f.emit(format!("{r} = select i1 {neg}, i128 {nv}, i128 {v}"));
                Ok(FVal::Int(r))
            }
            FVal::Float(ty, v) => {
                let (fv, lty) = to_arith_float(f, m, &ty, &v)?;
                let abs_decl = if lty == "float" {
                    "declare float @llvm.fabs.f32(float)"
                } else {
                    "declare double @llvm.fabs.f64(double)"
                };
                m.require_decl(abs_decl);
                let fn_name = if lty == "float" {
                    "@llvm.fabs.f32"
                } else {
                    "@llvm.fabs.f64"
                };
                let abs = f.reg();
                f.emit(format!("{abs} = call {lty} {fn_name}({lty} {fv})"));
                let res = from_arith_float(f, m, &ty, &abs)?;
                Ok(FVal::Float(ty, res))
            }
            _ => Err(Diagnostic::error(
                "VG-C0101",
                Span::default(),
                "abs() expects number",
            )),
        },
        Builtin::Min | Builtin::Max => {
            let va = eval_term(f, m, cx, &args[0])?;
            let vb = eval_term(f, m, cx, &args[1])?;
            match (va, vb) {
                (FVal::Int(a), FVal::Int(b_)) => {
                    let pred = if b == Builtin::Min { "slt" } else { "sgt" };
                    let cmp = f.reg();
                    f.emit(format!("{cmp} = icmp {pred} i128 {a}, {b_}"));
                    let r = f.reg();
                    f.emit(format!("{r} = select i1 {cmp}, i128 {a}, i128 {b_}"));
                    Ok(FVal::Int(r))
                }
                (FVal::Float(ta, a), FVal::Float(tb, b_)) if ta == tb => {
                    let (fa, lty) = to_arith_float(f, m, &ta, &a)?;
                    let (fb, _) = to_arith_float(f, m, &tb, &b_)?;
                    let (decl, fn_name) = match (b, lty) {
                        (Builtin::Min, "float") => (
                            "declare float @llvm.minimum.f32(float, float)",
                            "@llvm.minimum.f32",
                        ),
                        (Builtin::Min, "double") => (
                            "declare double @llvm.minimum.f64(double, double)",
                            "@llvm.minimum.f64",
                        ),
                        (Builtin::Max, "float") => (
                            "declare float @llvm.maximum.f32(float, float)",
                            "@llvm.maximum.f32",
                        ),
                        (Builtin::Max, "double") => (
                            "declare double @llvm.maximum.f64(double, double)",
                            "@llvm.maximum.f64",
                        ),
                        _ => unreachable!(),
                    };
                    m.require_decl(decl);
                    let r = f.reg();
                    f.emit(format!(
                        "{r} = call {lty} {fn_name}({lty} {fa}, {lty} {fb})"
                    ));
                    let out = from_arith_float(f, m, &ta, &r)?;
                    Ok(FVal::Float(ta, out))
                }
                _ => Err(Diagnostic::error(
                    "VG-C0101",
                    Span::default(),
                    "min/max type mismatch",
                )),
            }
        }
        Builtin::AsBytes => {
            let v = eval_term(f, m, cx, &args[0])?;
            let slice_ptr = f.alloca("{ ptr, i64 }", 8);
            match v {
                FVal::Bool(b_) => {
                    let buf = f.alloca("i8", 1);
                    let z = f.reg();
                    f.emit(format!("{z} = zext i1 {b_} to i8"));
                    f.emit(format!("store i8 {z}, ptr {buf}, align 1"));
                    emit_store_slice(f, &slice_ptr, &buf, "1");
                    Ok(FVal::Agg(Type::Slice(Box::new(Type::UInt(8))), slice_ptr))
                }
                FVal::Float(ty, ssa) => {
                    let lty = llvm_type(&ty, m.target)?;
                    let align = align_of(&ty, m.target)?;
                    let sz = size_of(&ty, m.target)?;
                    let buf = f.alloca(&lty, align);
                    let clean = emit_canon_float(f, &ty, &ssa);
                    f.emit(format!("store {lty} {clean}, ptr {buf}, align {align}"));
                    emit_store_slice(f, &slice_ptr, &buf, &format!("{sz}"));
                    Ok(FVal::Agg(Type::Slice(Box::new(Type::UInt(8))), slice_ptr))
                }
                FVal::Agg(ty, p) => match &ty {
                    Type::Slice(elem) => {
                        let (dp, len) = emit_slice_parts(f, &p);
                        let elem_sz = docs13_fixed_byte_size(elem)?.ok_or_else(|| {
                            Diagnostic::error(
                                "VG-T0114",
                                Span::default(),
                                "as_bytes invalid slice elem",
                            )
                        })?;
                        let total = f.reg();
                        f.emit(format!("{total} = mul i64 {len}, {elem_sz}"));
                        emit_store_slice(f, &slice_ptr, &dp, &total);
                        Ok(FVal::Agg(Type::Slice(Box::new(Type::UInt(8))), slice_ptr))
                    }
                    _ => {
                        let sz = docs13_fixed_byte_size(&ty)?.ok_or_else(|| {
                            Diagnostic::error("VG-T0114", Span::default(), "as_bytes invalid type")
                        })?;
                        emit_store_slice(f, &slice_ptr, &p, &format!("{sz}"));
                        Ok(FVal::Agg(Type::Slice(Box::new(Type::UInt(8))), slice_ptr))
                    }
                },
                FVal::Int(_) => Err(Diagnostic::error(
                    "VG-T0114",
                    Span::default(),
                    "as_bytes requires typed scalar or collection",
                )),
            }
        }
        Builtin::Same => {
            let va = eval_term(f, m, cx, &args[0])?;
            let vb = eval_term(f, m, cx, &args[1])?;
            let res = match (&va, &vb) {
                (FVal::Agg(ta, pa), FVal::Agg(tb, pb)) if ta == tb => {
                    eval_agg_eq(f, m, ta, pa, pb, true)?
                }
                _ => eval_scalar_eq_or_same(f, m, &va, &vb, true)?,
            };
            Ok(FVal::Bool(res))
        }
        Builtin::IsOk | Builtin::Ok | Builtin::Err => {
            let arg0 = eval_term(f, m, cx, &args[0])?;
            let FVal::Agg(ty, ptr) = &arg0 else {
                return Err(Diagnostic::error(
                    "VG-C0101",
                    Span::default(),
                    "expected Result",
                ));
            };
            let Type::Result(ok_t, err_t) = ty else {
                return Err(Diagnostic::error(
                    "VG-C0101",
                    Span::default(),
                    "expected Result",
                ));
            };
            let lty = llvm_type(ty, m.target)?;
            if b == Builtin::IsOk {
                let tp = f.reg();
                f.emit(format!(
                    "{tp} = getelementptr inbounds {lty}, ptr {ptr}, i32 0, i32 0"
                ));
                let tag = f.reg();
                f.emit(format!("{tag} = load i8, ptr {tp}, align 1"));
                let r = f.reg();
                f.emit(format!("{r} = icmp ne i8 {tag}, 0"));
                Ok(FVal::Bool(r))
            } else {
                let pp = f.reg();
                f.emit(format!(
                    "{pp} = getelementptr inbounds {lty}, ptr {ptr}, i32 0, i32 1"
                ));
                let payload_ty = if b == Builtin::Ok { ok_t } else { err_t };
                load_agg_or_scalar_as_fval(f, m, payload_ty, &pp)
            }
        }
    }
}

fn eval_call_in_formula(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    cx: &mut FormulaCx<'_>,
    call_ix: u32,
    args: &[Term],
) -> Result<FVal, Diagnostic> {
    let ch = *cx.node.calls.get(call_ix as usize).ok_or_else(|| {
        Diagnostic::error("VG-S0203", Span::default(), "formula callee out of range")
    })?;
    let cnode = cx.graph.by_hash.get(&ch).ok_or_else(|| {
        Diagnostic::error(
            "VG-S0009",
            Span::default(),
            "formula callee missing from graph",
        )
    })?;
    let cprefix = &cx.graph.prefixes[&ch];

    let mut ir_args = Vec::new();
    let non_eff_params: Vec<&Type> = cnode.params.iter().filter(|t| !is_erased(t)).collect();
    if non_eff_params.len() != args.len() {
        return Err(Diagnostic::error(
            "VG-C0103",
            Span::default(),
            "formula call argument count mismatch",
        ));
    }
    for (arg_t, param_ty) in args.iter().zip(&non_eff_params) {
        let fv = eval_term(f, m, cx, arg_t)?;
        if let (FVal::Agg(Type::Array(_, n), arr_ptr), Type::Slice(_)) = (&fv, *param_ty) {
            let sl = f.alloca("{ ptr, i64 }", 8);
            emit_store_slice(f, &sl, arr_ptr, &format!("{n}"));
            ir_args.push(IrVal {
                ty: (*param_ty).clone(),
                ssa: sl,
            });
        } else {
            ir_args.push(fval_to_ir_val(f, m, &fv, param_ty)?);
        }
    }
    for l in &cnode.learnable {
        let c = &cnode.consts[l.lo as usize];
        ir_args.push(emit_const_val(f, m, &c.ty, &c.bytes)?);
    }

    let non_eff_res: Vec<&Type> = cnode.results.iter().filter(|t| !is_erased(t)).collect();
    let mut out_ptrs = Vec::new();
    for rty in &non_eff_res {
        let lty = llvm_type(rty, m.target)?;
        let align = align_of(rty, m.target)?;
        out_ptrs.push(f.alloca(&lty, align));
    }

    let mut call_args_str = vec![format!("ptr {}", cx.ctx_ptr)];
    for iv in &ir_args {
        if is_scalar_abi(&iv.ty) {
            call_args_str.push(format!("{} {}", llvm_type(&iv.ty, m.target)?, iv.ssa));
        } else {
            call_args_str.push(format!("ptr {}", iv.ssa));
        }
    }
    for op in &out_ptrs {
        call_args_str.push(format!("ptr {op}"));
    }
    f.emit(format!(
        "call void @vgi_{cprefix}({})",
        call_args_str.join(", ")
    ));

    if non_eff_res.len() == 1 {
        load_agg_or_scalar_as_fval(f, m, non_eff_res[0], &out_ptrs[0])
    } else {
        let tup_ty = Type::Tuple(non_eff_res.iter().map(|t| (*t).clone()).collect());
        let tup_lty = llvm_type(&tup_ty, m.target)?;
        let tup_align = align_of(&tup_ty, m.target)?;
        let tup_ptr = f.alloca(&tup_lty, tup_align);
        for (idx, (rty, op)) in non_eff_res.iter().zip(&out_ptrs).enumerate() {
            let dst = f.reg();
            f.emit(format!(
                "{dst} = getelementptr inbounds {tup_lty}, ptr {tup_ptr}, i32 0, i32 {idx}"
            ));
            if is_scalar_abi(rty) {
                let s = emit_load_scalar(f, rty, op, m.target)?;
                emit_store_value_canon(
                    f,
                    m,
                    rty,
                    &IrVal {
                        ty: (*rty).clone(),
                        ssa: s,
                    },
                    &dst,
                )?;
            } else {
                let sz = size_of(rty, m.target)?;
                emit_memcpy(f, m, &dst, op, sz);
            }
        }
        Ok(FVal::Agg(tup_ty, tup_ptr))
    }
}

fn eval_fold_in_formula(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    cx: &mut FormulaCx<'_>,
    call_ix: u32,
    args: &[Term],
) -> Result<FVal, Diagnostic> {
    let ch = *cx.node.calls.get(call_ix as usize).ok_or_else(|| {
        Diagnostic::error("VG-S0203", Span::default(), "fold callee out of range")
    })?;
    let cnode = cx
        .graph
        .by_hash
        .get(&ch)
        .ok_or_else(|| Diagnostic::error("VG-S0009", Span::default(), "fold callee missing"))?;
    let cprefix = &cx.graph.prefixes[&ch];
    let acc_ty = cnode.results[0].clone();
    let elem_ty = cnode.params[1].clone();
    let acc_lty = llvm_type(&acc_ty, m.target)?;
    let acc_align = align_of(&acc_ty, m.target)?;
    let elem_lty = llvm_type(&elem_ty, m.target)?;

    let init_fv = eval_term(f, m, cx, &args[0])?;
    let init_iv = fval_to_ir_val(f, m, &init_fv, &acc_ty)?;
    let acc_buf = f.alloca(&acc_lty, acc_align);
    emit_store_value_canon(f, m, &acc_ty, &init_iv, &acc_buf)?;

    let xs_fv = eval_term(f, m, cx, &args[1])?;
    let (data_ptr, xs_len) = match xs_fv {
        FVal::Agg(Type::Array(_, n), p) => (p, format!("{n}")),
        FVal::Agg(Type::Slice(_), p) => emit_slice_parts(f, &p),
        _ => {
            return Err(Diagnostic::error(
                "VG-C0108",
                Span::default(),
                "fold expects Array or Slice",
            ));
        }
    };

    let FVal::Int(k128) = eval_term(f, m, cx, &args[2])? else {
        return Err(Diagnostic::error(
            "VG-C0108",
            Span::default(),
            "fold k must be integer",
        ));
    };
    let len128 = f.reg();
    f.emit(format!("{len128} = zext i64 {xs_len} to i128"));
    let k_pos = f.reg();
    f.emit(format!("{k_pos} = icmp sgt i128 {k128}, 0"));
    let k_nonneg = f.reg();
    f.emit(format!(
        "{k_nonneg} = select i1 {k_pos}, i128 {k128}, i128 0"
    ));
    let k_le = f.reg();
    f.emit(format!("{k_le} = icmp sle i128 {k_nonneg}, {len128}"));
    let k_clamped128 = f.reg();
    f.emit(format!(
        "{k_clamped128} = select i1 {k_le}, i128 {k_nonneg}, i128 {len128}"
    ));
    let k_i64 = f.reg();
    f.emit(format!("{k_i64} = trunc i128 {k_clamped128} to i64"));

    let mut b_vals = Vec::new();
    for (ba, bp) in args[3..].iter().zip(&cnode.params[2..]) {
        let fv = eval_term(f, m, cx, ba)?;
        b_vals.push(fval_to_ir_val(f, m, &fv, bp)?);
    }

    let out_buf = f.alloca(&acc_lty, acc_align);
    let i_ptr = f.alloca("i64", 8);
    f.emit(format!("store i64 0, ptr {i_ptr}, align 8"));
    let cond_bb = f.lbl("ffold.cond");
    let body_bb = f.lbl("ffold.body");
    let done_bb = f.lbl("ffold.done");
    f.br(&cond_bb);

    f.start_bb(&cond_bb);
    let idx = f.reg();
    f.emit(format!("{idx} = load i64, ptr {i_ptr}, align 8"));
    let cmp = f.reg();
    f.emit(format!("{cmp} = icmp ult i64 {idx}, {k_i64}"));
    f.cond_br(&cmp, &body_bb, &done_bb);

    f.start_bb(&body_bb);
    let ep = emit_gep_bytes(f, m.target, true, &elem_lty, &data_ptr, &idx);
    let mut cargs = vec![format!("ptr {}", cx.ctx_ptr)];
    if is_scalar_abi(&acc_ty) {
        let av = emit_load_scalar(f, &acc_ty, &acc_buf, m.target)?;
        cargs.push(format!("{acc_lty} {av}"));
    } else {
        cargs.push(format!("ptr {acc_buf}"));
    }
    if is_scalar_abi(&elem_ty) {
        let ev = emit_load_scalar(f, &elem_ty, &ep, m.target)?;
        cargs.push(format!("{elem_lty} {ev}"));
    } else {
        cargs.push(format!("ptr {ep}"));
    }
    for bv in &b_vals {
        if is_scalar_abi(&bv.ty) {
            cargs.push(format!("{} {}", llvm_type(&bv.ty, m.target)?, bv.ssa));
        } else {
            cargs.push(format!("ptr {}", bv.ssa));
        }
    }
    for l in &cnode.learnable {
        let c = &cnode.consts[l.lo as usize];
        let iv = emit_const_val(f, m, &c.ty, &c.bytes)?;
        cargs.push(format!("{} {}", llvm_type(&c.ty, m.target)?, iv.ssa));
    }
    cargs.push(format!("ptr {out_buf}"));
    f.emit(format!("call void @vgi_{cprefix}({})", cargs.join(", ")));
    if is_scalar_abi(&acc_ty) {
        let nv = emit_load_scalar(f, &acc_ty, &out_buf, m.target)?;
        f.emit(format!(
            "store {acc_lty} {nv}, ptr {acc_buf}, align {acc_align}"
        ));
    } else {
        let sz = size_of(&acc_ty, m.target)?;
        emit_memcpy(f, m, &acc_buf, &out_buf, sz);
    }
    let next = f.reg();
    f.emit(format!("{next} = add nuw i64 {idx}, 1"));
    f.emit(format!("store i64 {next}, ptr {i_ptr}, align 8"));
    f.br(&cond_bb);

    f.start_bb(&done_bb);
    load_agg_or_scalar_as_fval(f, m, &acc_ty, &acc_buf)
}

fn find_cost_arena_bound_in(t: &Term) -> Option<&Term> {
    if let Term::Bin(Bin::And, a, b) = t {
        return find_cost_arena_bound_in(a).or_else(|| find_cost_arena_bound_in(b));
    }
    if let Term::Bin(op, lhs, rhs) = t {
        match (op, &**lhs, &**rhs) {
            (Bin::Le | Bin::Eq, Term::Resource(Resource::ArenaBytes), e) => return Some(e),
            (Bin::Ge | Bin::Eq, e, Term::Resource(Resource::ArenaBytes)) => return Some(e),
            _ => {}
        }
    }
    None
}

fn find_cost_arena_bound(node: &Node) -> Option<&Term> {
    for t in &node.contract.cost {
        if let Some(found) = find_cost_arena_bound_in(t) {
            return Some(found);
        }
    }
    None
}

/// Emits `uint64_t vg_<prefix>_arena_bytes(inputs...)` returning the arena budget + static scratch.
pub fn emit_arena_bytes_fn(
    m: &mut ModBuilder,
    graph: &CallGraph,
    analyses: &BTreeMap<Hash, NodeAnalysis>,
) -> Result<(), Diagnostic> {
    let root = graph.order.last().expect("graph has root").1.clone();
    let prefix = &graph.prefixes[&graph.root_hash];
    let root_an = &analyses[&graph.root_hash];

    let mut sig_params = Vec::new();
    let mut param_map: Vec<Option<IrVal>> = vec![None; root.input_count()];
    for (i, ty) in root
        .params
        .iter()
        .chain(root.learnable.iter().map(|l| &l.ty))
        .enumerate()
    {
        if is_erased(ty) {
            continue;
        }
        let pname = format!("%arg{i}");
        if is_scalar_abi(ty) {
            sig_params.push(format!("{} {pname}", llvm_type(ty, m.target)?));
        } else {
            sig_params.push(format!("ptr {pname}"));
        }
        param_map[i] = Some(IrVal {
            ty: ty.clone(),
            ssa: pname,
        });
    }
    let inputs: Vec<IrVal> = param_map
        .into_iter()
        .map(|o| {
            o.unwrap_or(IrVal {
                ty: Type::Eff,
                ssa: String::new(),
            })
        })
        .collect();

    let mut f = FnBuilder::new();
    let dummy_ctx = f.alloca("i8", 16);
    let mut cx = FormulaCx {
        node: &root,
        graph,
        ctx_ptr: &dummy_ctx,
        inputs: &inputs,
        bound: Vec::new(),
    };

    let base_arena = if let Some(bound_term) = find_cost_arena_bound(&root) {
        let FVal::Int(v128) = eval_term(&mut f, m, &mut cx, bound_term)? else {
            return Err(Diagnostic::error(
                "VG-C0101",
                Span::default(),
                "COST arena_bytes bound must be integer",
            ));
        };
        let is_pos = f.reg();
        f.emit(format!("{is_pos} = icmp sgt i128 {v128}, 0"));
        let clamped = f.reg();
        f.emit(format!(
            "{clamped} = select i1 {is_pos}, i128 {v128}, i128 0"
        ));
        let res = f.reg();
        f.emit(format!("{res} = trunc i128 {clamped} to i64"));
        res
    } else {
        emit_dynamic_arena_sum(&mut f, m, &root, &inputs, graph, analyses)?
    };

    let with_scratch = if root_an.static_scratch_bytes > 0 {
        let r = f.reg();
        f.emit(format!(
            "{r} = add i64 {base_arena}, {}",
            root_an.static_scratch_bytes
        ));
        r
    } else {
        base_arena
    };
    f.ret(&format!("i64 {with_scratch}"));

    let header = format!(
        "define i64 @vg_{prefix}_arena_bytes({})",
        sig_params.join(", ")
    );
    m.fns.push(f.finish(&header));
    Ok(())
}

fn operand_len_i64(
    f: &mut FnBuilder,
    node: &Node,
    op: Operand,
    inputs: &[IrVal],
    op_lens: &[Option<String>],
) -> Result<String, Diagnostic> {
    match op {
        Operand::Input(i) => match &inputs[i as usize].ty {
            Type::Array(_, n) => Ok(format!("{n}")),
            Type::Tensor(_, dims) => Ok(format!("{}", tensor_elem_count(dims)?)),
            Type::Slice(_) => {
                let (_, len) = emit_slice_parts(f, &inputs[i as usize].ssa);
                Ok(len)
            }
            t => Err(Diagnostic::error(
                "VG-T0101",
                Span::default(),
                format!("expected collection for length, found {t}"),
            )),
        },
        Operand::Const(c) => match &node.consts[c as usize].ty {
            Type::Array(_, n) => Ok(format!("{n}")),
            Type::Tensor(_, dims) => Ok(format!("{}", tensor_elem_count(dims)?)),
            _ => Err(Diagnostic::error(
                "VG-T0101",
                Span::default(),
                "const has no length",
            )),
        },
        Operand::Op(i, 0) => match &node.ops[i as usize].results[0] {
            Type::Array(_, n) => Ok(format!("{n}")),
            Type::Tensor(_, dims) => Ok(format!("{}", tensor_elem_count(dims)?)),
            Type::Slice(_) => Ok(op_lens[i as usize]
                .clone()
                .unwrap_or_else(|| "0".to_string())),
            _ => Ok("0".to_string()),
        },
        _ => Ok("0".to_string()),
    }
}

fn emit_dynamic_arena_sum(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    node: &Node,
    inputs: &[IrVal],
    graph: &CallGraph,
    analyses: &BTreeMap<Hash, NodeAnalysis>,
) -> Result<String, Diagnostic> {
    let node_hash = vg_core::canon::hashes(node)?.node;
    let an = &analyses[&node_hash];

    let mut acc = "0".to_string();
    let mut slice_lens: Vec<Option<String>> = vec![None; node.ops.len()];
    let mut scalar_u64_vals: BTreeMap<usize, String> = BTreeMap::new();

    for (i, op) in node.ops.iter().enumerate() {
        if op.opcode == OP_SIMD_MAP && matches!(op.results[0], Type::Slice(_)) {
            let len = operand_len_i64(f, node, op.operands[1], inputs, &slice_lens)?;
            slice_lens[i] = Some(len.clone());
            let elem = op.results[0].elem().expect("slice elem");
            let elem_sz = size_of(elem, m.target)?;
            let bytes = f.reg();
            f.emit(format!("{bytes} = mul i64 {len}, {elem_sz}"));
            let aligned = align_up_dyn(f, &bytes);
            let next = f.reg();
            f.emit(format!("{next} = add i64 {acc}, {aligned}"));
            acc = next;
        } else if op.opcode == OP_GENERATE && matches!(op.results[0], Type::Slice(_)) {
            let cnt = match op.operands[1] {
                Operand::Input(k) => inputs[k as usize].ssa.clone(),
                Operand::Const(ci) => {
                    let b = &node.consts[ci as usize].bytes;
                    format!(
                        "{}",
                        u64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8]))
                    )
                }
                Operand::Op(k, 0) => scalar_u64_vals
                    .get(&(k as usize))
                    .cloned()
                    .unwrap_or_else(|| "0".to_string()),
                _ => "0".to_string(),
            };
            slice_lens[i] = Some(cnt.clone());
            let elem = op.results[0].elem().expect("slice elem");
            let elem_sz = size_of(elem, m.target)?;
            let bytes = f.reg();
            f.emit(format!("{bytes} = mul i64 {cnt}, {elem_sz}"));
            let aligned = align_up_dyn(f, &bytes);
            let next = f.reg();
            f.emit(format!("{next} = add i64 {acc}, {aligned}"));
            acc = next;
        } else if op.opcode == OP_CONCAT && matches!(op.results[0], Type::Slice(_)) {
            let l1 = operand_len_i64(f, node, op.operands[0], inputs, &slice_lens)?;
            let l2 = operand_len_i64(f, node, op.operands[1], inputs, &slice_lens)?;
            let len = f.reg();
            f.emit(format!("{len} = add i64 {l1}, {l2}"));
            slice_lens[i] = Some(len.clone());
            let elem = op.results[0].elem().expect("slice elem");
            let elem_sz = size_of(elem, m.target)?;
            let bytes = f.reg();
            f.emit(format!("{bytes} = mul i64 {len}, {elem_sz}"));
            let aligned = align_up_dyn(f, &bytes);
            let next = f.reg();
            f.emit(format!("{next} = add i64 {acc}, {aligned}"));
            acc = next;
        } else if op.opcode == OP_UPDATE && matches!(op.results[0], Type::Slice(_)) {
            let len = operand_len_i64(f, node, op.operands[0], inputs, &slice_lens)?;
            slice_lens[i] = Some(len.clone());
            if !an.inplace_update[i] {
                let elem = op.results[0].elem().expect("slice elem");
                let elem_sz = size_of(elem, m.target)?;
                let bytes = f.reg();
                f.emit(format!("{bytes} = mul i64 {len}, {elem_sz}"));
                let aligned = align_up_dyn(f, &bytes);
                let next = f.reg();
                f.emit(format!("{next} = add i64 {acc}, {aligned}"));
                acc = next;
            }
        } else if op.opcode == 0x0400 {
            let max_b = match op.operands[2] {
                Operand::Input(k) => inputs[k as usize].ssa.clone(),
                Operand::Const(ci) => {
                    let b = &node.consts[ci as usize].bytes;
                    format!(
                        "{}",
                        u64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8]))
                    )
                }
                _ => "0".to_string(),
            };
            let aligned = align_up_dyn(f, &max_b);
            let next = f.reg();
            f.emit(format!("{next} = add i64 {acc}, {aligned}"));
            acc = next;
        } else if op.opcode == OP_CALL_HASH {
            if let Some(Operand::Callee(c)) = op.operands.first().copied() {
                let ch = node.calls[c as usize];
                if let Some(cal_node) = graph.by_hash.get(&ch) {
                    if let Some(bound) = find_cost_arena_bound(cal_node) {
                        if let Term::Const(ci) = bound
                            && let Some(v) = int_value(&cal_node.consts[*ci as usize].bytes)
                        {
                            let r = f.reg();
                            f.emit(format!("{r} = add i64 {acc}, {}", v.max(0)));
                            acc = r;
                        }
                    } else {
                        for cop in &cal_node.ops {
                            if cop.opcode == 0x0400
                                && let Operand::Const(ci) = cop.operands[2]
                            {
                                let b = &cal_node.consts[ci as usize].bytes;
                                let max_b = u64::from_le_bytes(b[..8].try_into().unwrap_or([0; 8]));
                                let r = f.reg();
                                f.emit(format!("{r} = add i64 {acc}, {}", align_up(max_b, 16)));
                                acc = r;
                            }
                        }
                    }
                }
            }
        } else if op.opcode == OP_ASSERT_GUARD {
            if let (Some(Operand::Callee(ct)), Some(Operand::Callee(ce))) =
                (op.operands.get(1).copied(), op.operands.last().copied())
            {
                let branch_cost = |ch: &Hash| -> u64 {
                    graph
                        .by_hash
                        .get(ch)
                        .and_then(|cn| {
                            if let Some(Term::Const(ci)) = find_cost_arena_bound(cn) {
                                int_value(&cn.consts[*ci as usize].bytes).map(|v| v.max(0) as u64)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0)
                };
                let ct_h = node.calls[ct as usize];
                let ce_h = node.calls[ce as usize];
                let add_b = if op.mode == 0 && is_const_bool(node, op.operands[0], true) {
                    branch_cost(&ct_h)
                } else {
                    branch_cost(&ct_h).max(branch_cost(&ce_h))
                };
                if add_b > 0 {
                    let r = f.reg();
                    f.emit(format!("{r} = add i64 {acc}, {add_b}"));
                    acc = r;
                }
            }
        } else if op.opcode == 0x0062 {
            let l = operand_len_i64(f, node, op.operands[0], inputs, &slice_lens)?;
            scalar_u64_vals.insert(i, l);
        }
    }
    Ok(acc)
}
