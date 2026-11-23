//! Shared textual LLVM IR module/function builders and low-level ABI/NaN helpers.

use std::collections::{BTreeMap, BTreeSet};

use vg_core::diag::{Diagnostic, Span};
use vg_core::types::Type;

use crate::Target;
use crate::layout::{
    align_of, align_up, llvm_type, result_union_layout, size_of, tuple_member_offsets,
};

#[derive(Clone, Debug)]
pub struct IrVal {
    pub ty: Type,
    pub ssa: String,
}

pub struct ModBuilder {
    pub target: Target,
    pub proof_facts: bool,
    pub runtime_checks: bool,
    pub decls: BTreeSet<String>,
    pub globals: BTreeMap<String, String>,
    str_pool: BTreeMap<String, String>,
    bytes_pool: BTreeMap<(Vec<u8>, u64), String>,
    pub fns: Vec<String>,
    pub has_vec_loop: bool,
    next_id: u32,
}

impl ModBuilder {
    pub fn new(target: Target, proof_facts: bool, runtime_checks: bool) -> Self {
        Self {
            target,
            proof_facts,
            runtime_checks,
            decls: BTreeSet::new(),
            globals: BTreeMap::new(),
            str_pool: BTreeMap::new(),
            bytes_pool: BTreeMap::new(),
            fns: Vec::new(),
            has_vec_loop: false,
            next_id: 0,
        }
    }

    pub fn require_decl(&mut self, decl: &str) {
        self.decls.insert(decl.to_string());
    }

    pub fn require_vg_rt_core(&mut self) {
        self.require_decl("declare void @vg_rt_init(ptr, i64, ptr)");
        self.require_decl("declare void @vg_rt_destroy(ptr)");
        self.require_decl("declare i32 @vg_arena_has_space(ptr, i64)");
        self.require_decl("declare ptr @vg_arena_alloc(ptr, i64)");
        self.require_decl("declare ptr @vg_input_alloc(ptr, i64)");
        self.require_decl("declare i64 @vg_fp_enter()");
        self.require_decl("declare void @vg_fp_leave(i64)");
        self.require_decl("declare void @vg_emit_guard(ptr, ptr, ptr, ptr)");
        self.require_decl("declare void @vg_trap_guard(ptr, ptr, ptr, ptr) noreturn");
        self.require_decl("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
    }

    pub fn intern_cstr(&mut self, s: &str) -> String {
        if let Some(name) = self.str_pool.get(s) {
            return name.clone();
        }
        let id = self.next_id;
        self.next_id += 1;
        let name = format!("@.str.{id}");
        let bytes = s.as_bytes();
        let len = bytes.len() + 1;
        let escaped = llvm_c_bytes(bytes, true);
        self.globals.insert(
            name.clone(),
            format!("{name} = private unnamed_addr constant [{len} x i8] c\"{escaped}\", align 1"),
        );
        self.str_pool.insert(s.to_string(), name.clone());
        name
    }

    pub fn intern_bytes_const(&mut self, bytes: &[u8], align: u64) -> String {
        let key = (bytes.to_vec(), align.max(1));
        if let Some(name) = self.bytes_pool.get(&key) {
            return name.clone();
        }
        let id = self.next_id;
        self.next_id += 1;
        let name = format!("@.const.{id}");
        let len = bytes.len();
        let body = if len == 0 {
            "zeroinitializer".to_string()
        } else {
            format!("c\"{}\"", llvm_c_bytes(bytes, false))
        };
        self.globals.insert(
            name.clone(),
            format!(
                "{name} = private unnamed_addr constant [{len} x i8] {body}, align {}",
                align.max(1)
            ),
        );
        self.bytes_pool.insert(key, name.clone());
        name
    }

    pub fn render(self) -> String {
        let mut out = String::new();
        out.push_str("; VeriGraph generated LLVM IR\n");
        out.push_str("source_filename = \"vg_module\"\n");
        out.push_str(&format!(
            "target datalayout = \"{}\"\n",
            self.target.datalayout()
        ));
        out.push_str(&format!("target triple = \"{}\"\n\n", self.target.triple()));

        if !self.globals.is_empty() {
            for def in self.globals.values() {
                out.push_str(def);
                out.push('\n');
            }
            out.push('\n');
        }
        if !self.decls.is_empty() {
            for decl in &self.decls {
                out.push_str(decl);
                out.push('\n');
            }
            out.push('\n');
        }
        for (i, func) in self.fns.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(func);
            if !func.ends_with('\n') {
                out.push('\n');
            }
        }
        if self.has_vec_loop {
            out.push_str("\n!0 = distinct !{!0, !1}\n");
            out.push_str("!1 = !{!\"llvm.loop.vectorize.enable\", i1 true}\n");
        }
        out
    }
}

fn llvm_c_bytes(bytes: &[u8], nul: bool) -> String {
    let mut s = String::new();
    for &b in bytes {
        if b.is_ascii_graphic() && b != b'\\' && b != b'"' {
            s.push(b as char);
        } else {
            s.push_str(&format!("\\{b:02X}"));
        }
    }
    if nul {
        s.push_str("\\00");
    }
    s
}

pub struct FnBuilder {
    allocas: Vec<String>,
    body: Vec<String>,
    next_reg: u32,
    next_lbl: u32,
    terminated: bool,
}

impl Default for FnBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl FnBuilder {
    pub fn new() -> Self {
        Self {
            allocas: Vec::new(),
            body: Vec::new(),
            next_reg: 0,
            next_lbl: 0,
            terminated: false,
        }
    }

    pub fn reg(&mut self) -> String {
        let r = format!("%r{}", self.next_reg);
        self.next_reg += 1;
        r
    }

    pub fn lbl(&mut self, prefix: &str) -> String {
        let l = format!("{prefix}.{}", self.next_lbl);
        self.next_lbl += 1;
        l
    }

    pub fn alloca(&mut self, llvm_ty: &str, align: u64) -> String {
        let r = self.reg();
        self.allocas
            .push(format!("  {r} = alloca {llvm_ty}, align {}", align.max(1)));
        r
    }

    pub fn emit(&mut self, line: impl Into<String>) {
        if !self.terminated {
            self.body.push(format!("  {}", line.into()));
        }
    }

    pub fn start_bb(&mut self, label: &str) {
        if !self.terminated {
            self.body.push(format!("  br label %{label}"));
        }
        self.body.push(format!("{label}:"));
        self.terminated = false;
    }

    pub fn br(&mut self, target: &str) {
        if !self.terminated {
            self.body.push(format!("  br label %{target}"));
            self.terminated = true;
        }
    }

    pub fn br_loop(&mut self, target: &str, md: &str) {
        if !self.terminated {
            self.body
                .push(format!("  br label %{target}, !llvm.loop {md}"));
            self.terminated = true;
        }
    }

    pub fn cond_br(&mut self, cond: &str, then_bb: &str, else_bb: &str) {
        if !self.terminated {
            self.body.push(format!(
                "  br i1 {cond}, label %{then_bb}, label %{else_bb}"
            ));
            self.terminated = true;
        }
    }

    pub fn ret(&mut self, typed_val: &str) {
        if !self.terminated {
            self.body.push(format!("  ret {typed_val}"));
            self.terminated = true;
        }
    }

    pub fn ret_void(&mut self) {
        if !self.terminated {
            self.body.push("  ret void".to_string());
            self.terminated = true;
        }
    }

    pub fn unreachable(&mut self) {
        if !self.terminated {
            self.body.push("  unreachable".to_string());
            self.terminated = true;
        }
    }

    pub fn finish(self, header: &str) -> String {
        let mut out = String::new();
        out.push_str(header);
        out.push_str(" {\nentry:\n");
        for a in &self.allocas {
            out.push_str(a);
            out.push('\n');
        }
        for line in &self.body {
            out.push_str(line);
            out.push('\n');
        }
        if !self.terminated {
            out.push_str("  unreachable\n");
        }
        out.push_str("}\n");
        out
    }
}

pub fn f32_const_expr(bits: u32) -> String {
    format!("bitcast (i32 {bits} to float)")
}

pub fn f64_const_expr(bits: u64) -> String {
    format!("bitcast (i64 {bits} to double)")
}

/// Canonicalizes NaN on float values (ADR 0002).
pub fn emit_canon_float(f: &mut FnBuilder, ty: &Type, val: &str) -> String {
    match ty {
        Type::Float(32) => {
            let is_nan = f.reg();
            f.emit(format!("{is_nan} = fcmp uno float {val}, {val}"));
            let res = f.reg();
            let canon = f32_const_expr(0x7FC0_0000);
            f.emit(format!(
                "{res} = select i1 {is_nan}, float {canon}, float {val}"
            ));
            res
        }
        Type::Float(64) => {
            let is_nan = f.reg();
            f.emit(format!("{is_nan} = fcmp uno double {val}, {val}"));
            let res = f.reg();
            let canon = f64_const_expr(0x7FF8_0000_0000_0000);
            f.emit(format!(
                "{res} = select i1 {is_nan}, double {canon}, double {val}"
            ));
            res
        }
        Type::Float(16) => {
            let abs = f.reg();
            f.emit(format!("{abs} = and i16 {val}, 32767"));
            let is_nan = f.reg();
            f.emit(format!("{is_nan} = icmp ugt i16 {abs}, 31744"));
            let res = f.reg();
            f.emit(format!("{res} = select i1 {is_nan}, i16 32256, i16 {val}"));
            res
        }
        Type::BFloat16 => {
            let abs = f.reg();
            f.emit(format!("{abs} = and i16 {val}, 32767"));
            let is_nan = f.reg();
            f.emit(format!("{is_nan} = icmp ugt i16 {abs}, 32640"));
            let res = f.reg();
            f.emit(format!("{res} = select i1 {is_nan}, i16 32704, i16 {val}"));
            res
        }
        _ => val.to_string(),
    }
}

pub fn emit_memcpy(f: &mut FnBuilder, m: &mut ModBuilder, dst: &str, src: &str, bytes: u64) {
    if bytes == 0 {
        return;
    }
    m.require_decl("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
    f.emit(format!(
        "call void @llvm.memcpy.p0.p0.i64(ptr {dst}, ptr {src}, i64 {bytes}, i1 false)"
    ));
}

pub fn emit_memcpy_dyn(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    dst: &str,
    src: &str,
    bytes_i64: &str,
) {
    m.require_decl("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)");
    f.emit(format!(
        "call void @llvm.memcpy.p0.p0.i64(ptr {dst}, ptr {src}, i64 {bytes_i64}, i1 false)"
    ));
}

pub fn emit_gep_bytes(
    f: &mut FnBuilder,
    target: Target,
    proof_facts: bool,
    elem_ty: &str,
    base: &str,
    idx_i64: &str,
) -> String {
    let idx_use = if target == Target::Wasm32Wasi {
        let t = f.reg();
        f.emit(format!("{t} = trunc i64 {idx_i64} to i32"));
        (target.ptr_int_ty(), t)
    } else {
        ("i64", idx_i64.to_string())
    };
    let inb = if proof_facts { " inbounds" } else { "" };
    let r = f.reg();
    f.emit(format!(
        "{r} = getelementptr{inb} {elem_ty}, ptr {base}, {} {}",
        idx_use.0, idx_use.1
    ));
    r
}

pub fn emit_slice_parts(f: &mut FnBuilder, slice_ptr: &str) -> (String, String) {
    let p_field = f.reg();
    f.emit(format!(
        "{p_field} = getelementptr inbounds {{ ptr, i64 }}, ptr {slice_ptr}, i32 0, i32 0"
    ));
    let data_ptr = f.reg();
    f.emit(format!("{data_ptr} = load ptr, ptr {p_field}, align 8"));
    let l_field = f.reg();
    f.emit(format!(
        "{l_field} = getelementptr inbounds {{ ptr, i64 }}, ptr {slice_ptr}, i32 0, i32 1"
    ));
    let len = f.reg();
    f.emit(format!("{len} = load i64, ptr {l_field}, align 8"));
    (data_ptr, len)
}

pub fn emit_store_slice(f: &mut FnBuilder, slice_ptr: &str, data_ptr: &str, len_i64: &str) {
    let p_field = f.reg();
    f.emit(format!(
        "{p_field} = getelementptr inbounds {{ ptr, i64 }}, ptr {slice_ptr}, i32 0, i32 0"
    ));
    f.emit(format!("store ptr {data_ptr}, ptr {p_field}, align 8"));
    let l_field = f.reg();
    f.emit(format!(
        "{l_field} = getelementptr inbounds {{ ptr, i64 }}, ptr {slice_ptr}, i32 0, i32 1"
    ));
    f.emit(format!("store i64 {len_i64}, ptr {l_field}, align 8"));
}

/// Loads a value from `ptr` in `elem_ty` (scalar SSA or aggregate `ptr` untouched/allocated).
pub fn emit_load_scalar(
    f: &mut FnBuilder,
    ty: &Type,
    ptr: &str,
    target: Target,
) -> Result<String, Diagnostic> {
    let lty = llvm_type(ty, target)?;
    let align = align_of(ty, target)?;
    let r = f.reg();
    f.emit(format!("{r} = load {lty}, ptr {ptr}, align {align}"));
    Ok(r)
}

/// Stores a scalar `val` of `ty` to `ptr`, canonicalizing NaN if `ty` is a float (`docs/06`).
pub fn emit_store_scalar_canon(
    f: &mut FnBuilder,
    ty: &Type,
    val: &str,
    ptr: &str,
    target: Target,
) -> Result<(), Diagnostic> {
    let lty = llvm_type(ty, target)?;
    let align = align_of(ty, target)?;
    let v = emit_canon_float(f, ty, val);
    f.emit(format!("store {lty} {v}, ptr {ptr}, align {align}"));
    Ok(())
}

/// Copies a value into `dst_ptr`, canonicalizing floats if scalar or recursively if aggregate
/// containing floats (`docs/06`, ADR 0002).
pub fn emit_store_value_canon(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ty: &Type,
    src: &IrVal,
    dst_ptr: &str,
) -> Result<(), Diagnostic> {
    if crate::layout::is_scalar_abi(ty) {
        return emit_store_scalar_canon(f, ty, &src.ssa, dst_ptr, m.target);
    }
    if !contains_float(ty) {
        let sz = size_of(ty, m.target)?;
        emit_memcpy(f, m, dst_ptr, &src.ssa, sz);
        return Ok(());
    }
    match ty {
        Type::Array(elem, n) => emit_canon_array_copy(f, m, elem, u64::from(*n), &src.ssa, dst_ptr),
        Type::Tensor(elem, dims) => {
            let count = crate::layout::tensor_elem_count(dims)?;
            emit_canon_array_copy(f, m, elem, count, &src.ssa, dst_ptr)
        }
        Type::Slice(_) => {
            // Slice elements live in input/arena and were canonicalized on store/validated on entry.
            let sz = size_of(ty, m.target)?;
            emit_memcpy(f, m, dst_ptr, &src.ssa, sz);
            Ok(())
        }
        Type::Tuple(ms) => {
            let lty = llvm_type(ty, m.target)?;
            for (idx, member) in ms.iter().enumerate() {
                let sp = f.reg();
                f.emit(format!(
                    "{sp} = getelementptr inbounds {lty}, ptr {}, i32 0, i32 {idx}",
                    src.ssa
                ));
                let dp = f.reg();
                f.emit(format!(
                    "{dp} = getelementptr inbounds {lty}, ptr {dst_ptr}, i32 0, i32 {idx}"
                ));
                if crate::layout::is_scalar_abi(member) {
                    let loaded = emit_load_scalar(f, member, &sp, m.target)?;
                    emit_store_scalar_canon(f, member, &loaded, &dp, m.target)?;
                } else {
                    emit_store_value_canon(
                        f,
                        m,
                        member,
                        &IrVal {
                            ty: member.clone(),
                            ssa: sp,
                        },
                        &dp,
                    )?;
                }
            }
            Ok(())
        }
        Type::Result(ok_t, err_t) => {
            let lty = llvm_type(ty, m.target)?;
            let sz = size_of(ty, m.target)?;
            emit_memcpy(f, m, dst_ptr, &src.ssa, sz);
            let tag_p = f.reg();
            f.emit(format!(
                "{tag_p} = getelementptr inbounds {lty}, ptr {dst_ptr}, i32 0, i32 0"
            ));
            let tag = f.reg();
            f.emit(format!("{tag} = load i8, ptr {tag_p}, align 1"));
            let is_ok = f.reg();
            f.emit(format!("{is_ok} = icmp ne i8 {tag}, 0"));
            let pay_p = f.reg();
            f.emit(format!(
                "{pay_p} = getelementptr inbounds {lty}, ptr {dst_ptr}, i32 0, i32 1"
            ));
            let ok_bb = f.lbl("res_canon.ok");
            let err_bb = f.lbl("res_canon.err");
            let done_bb = f.lbl("res_canon.done");
            f.cond_br(&is_ok, &ok_bb, &err_bb);

            f.start_bb(&ok_bb);
            if contains_float(ok_t) {
                if crate::layout::is_scalar_abi(ok_t) {
                    let v = emit_load_scalar(f, ok_t, &pay_p, m.target)?;
                    emit_store_scalar_canon(f, ok_t, &v, &pay_p, m.target)?;
                } else {
                    emit_canon_inplace(f, m, ok_t, &pay_p)?;
                }
            }
            f.br(&done_bb);

            f.start_bb(&err_bb);
            if contains_float(err_t) {
                if crate::layout::is_scalar_abi(err_t) {
                    let v = emit_load_scalar(f, err_t, &pay_p, m.target)?;
                    emit_store_scalar_canon(f, err_t, &v, &pay_p, m.target)?;
                } else {
                    emit_canon_inplace(f, m, err_t, &pay_p)?;
                }
            }
            f.br(&done_bb);
            f.start_bb(&done_bb);
            Ok(())
        }
        _ => Err(Diagnostic::error(
            "VG-T0101",
            Span::default(),
            format!("cannot store value of type {ty}"),
        )),
    }
}

fn emit_canon_inplace(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ty: &Type,
    ptr: &str,
) -> Result<(), Diagnostic> {
    emit_store_value_canon(
        f,
        m,
        ty,
        &IrVal {
            ty: ty.clone(),
            ssa: ptr.to_string(),
        },
        ptr,
    )
}

fn emit_canon_array_copy(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    elem: &Type,
    count: u64,
    src_ptr: &str,
    dst_ptr: &str,
) -> Result<(), Diagnostic> {
    if count == 0 {
        return Ok(());
    }
    let elem_lty = llvm_type(elem, m.target)?;
    let idx_ptr = f.alloca("i64", 8);
    f.emit(format!("store i64 0, ptr {idx_ptr}, align 8"));
    let cond_bb = f.lbl("arr_copy.cond");
    let body_bb = f.lbl("arr_copy.body");
    let done_bb = f.lbl("arr_copy.done");
    f.br(&cond_bb);

    f.start_bb(&cond_bb);
    let idx = f.reg();
    f.emit(format!("{idx} = load i64, ptr {idx_ptr}, align 8"));
    let cmp = f.reg();
    f.emit(format!("{cmp} = icmp ult i64 {idx}, {count}"));
    f.cond_br(&cmp, &body_bb, &done_bb);

    f.start_bb(&body_bb);
    let sp = emit_gep_bytes(f, m.target, true, &elem_lty, src_ptr, &idx);
    let dp = emit_gep_bytes(f, m.target, true, &elem_lty, dst_ptr, &idx);
    if crate::layout::is_scalar_abi(elem) {
        let v = emit_load_scalar(f, elem, &sp, m.target)?;
        emit_store_scalar_canon(f, elem, &v, &dp, m.target)?;
    } else {
        emit_store_value_canon(
            f,
            m,
            elem,
            &IrVal {
                ty: elem.clone(),
                ssa: sp,
            },
            &dp,
        )?;
    }
    let next = f.reg();
    f.emit(format!("{next} = add nuw i64 {idx}, 1"));
    f.emit(format!("store i64 {next}, ptr {idx_ptr}, align 8"));
    f.br(&cond_bb);

    f.start_bb(&done_bb);
    Ok(())
}

pub fn contains_float(ty: &Type) -> bool {
    match ty {
        Type::Float(_) | Type::BFloat16 => true,
        Type::Array(e, _) | Type::Tensor(e, _) | Type::Slice(e) => contains_float(e),
        Type::Tuple(ms) => ms.iter().any(contains_float),
        Type::Result(a, b) => contains_float(a) || contains_float(b),
        _ => false,
    }
}

pub fn needs_input_validation(ty: &Type) -> bool {
    match ty {
        Type::Bool | Type::Float(_) | Type::BFloat16 | Type::Result(..) => true,
        Type::Array(e, _) | Type::Tensor(e, _) | Type::Slice(e) => needs_input_validation(e),
        Type::Tuple(ms) => ms.iter().any(needs_input_validation),
        _ => false,
    }
}

/// Emits a scalar/array/tensor `Const` as an `IrVal`.
pub fn emit_const_val(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ty: &Type,
    bytes: &[u8],
) -> Result<IrVal, Diagnostic> {
    let ssa = match ty {
        Type::Bool => format!("i8 {}", u8::from(bytes.first().copied().unwrap_or(0) != 0)),
        Type::UInt(w) => {
            let mut raw = [0u8; 16];
            let n = bytes.len().min(16);
            raw[..n].copy_from_slice(&bytes[..n]);
            let v = u128::from_le_bytes(raw);
            let mask = if *w == 128 {
                u128::MAX
            } else {
                (1u128 << w) - 1
            };
            let _ = f;
            format!("{}", v & mask)
        }
        Type::SInt(_) => {
            let fill = if bytes.last().is_some_and(|b| b & 0x80 != 0) {
                0xFF
            } else {
                0
            };
            let mut raw = [fill; 16];
            let n = bytes.len().min(16);
            raw[..n].copy_from_slice(&bytes[..n]);
            let v = i128::from_le_bytes(raw);
            let _ = f;
            format!("{v}")
        }
        Type::Float(16) | Type::BFloat16 => {
            let mut b = [0u8; 2];
            b.copy_from_slice(&bytes[..2]);
            format!("{}", u16::from_le_bytes(b))
        }
        Type::Float(32) => {
            let mut b = [0u8; 4];
            b.copy_from_slice(&bytes[..4]);
            f32_const_expr(u32::from_le_bytes(b))
        }
        Type::Float(64) => {
            let mut b = [0u8; 8];
            b.copy_from_slice(&bytes[..8]);
            f64_const_expr(u64::from_le_bytes(b))
        }
        Type::Array(..) | Type::Tensor(..) => {
            let align = align_of(ty, m.target)?;
            m.intern_bytes_const(bytes, align)
        }
        _ => {
            return Err(Diagnostic::error(
                "VG-T0101",
                Span::default(),
                format!("unsupported constant type {ty}"),
            ));
        }
    };
    let clean_ssa = if matches!(ty, Type::Bool) {
        ssa.strip_prefix("i8 ").unwrap_or(&ssa).to_string()
    } else {
        ssa
    };
    Ok(IrVal {
        ty: ty.clone(),
        ssa: clean_ssa,
    })
}

pub fn tuple_member_ptr(
    f: &mut FnBuilder,
    ty: &Type,
    base: &str,
    idx: usize,
    target: Target,
) -> Result<String, Diagnostic> {
    let lty = llvm_type(ty, target)?;
    let r = f.reg();
    f.emit(format!(
        "{r} = getelementptr inbounds {lty}, ptr {base}, i32 0, i32 {idx}"
    ));
    Ok(r)
}

pub fn tuple_byte_offset(ms: &[Type], idx: usize, target: Target) -> Result<u64, Diagnostic> {
    let offsets = tuple_member_offsets(ms, target)?;
    offsets
        .get(idx)
        .copied()
        .ok_or_else(|| Diagnostic::error("VG-S0203", Span::default(), "tuple index out of range"))
}

pub fn emit_trap(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    ctx_ptr: &str,
    guard_id: &str,
    obligation: &str,
    node_prefix: &str,
) {
    m.require_decl("declare void @vg_trap_guard(ptr, ptr, ptr, ptr) noreturn");
    let g = m.intern_cstr(guard_id);
    let o = m.intern_cstr(obligation);
    let n = m.intern_cstr(node_prefix);
    f.emit(format!(
        "call void @vg_trap_guard(ptr {ctx_ptr}, ptr {g}, ptr {o}, ptr {n})"
    ));
    f.unreachable();
}

pub fn emit_branch_trap_if_false(
    f: &mut FnBuilder,
    m: &mut ModBuilder,
    cond_i1: &str,
    ctx_ptr: &str,
    guard_id: &str,
    obligation: &str,
    node_prefix: &str,
) {
    let ok_bb = f.lbl("chk.ok");
    let fail_bb = f.lbl("chk.fail");
    f.cond_br(cond_i1, &ok_bb, &fail_bb);
    f.start_bb(&fail_bb);
    emit_trap(f, m, ctx_ptr, guard_id, obligation, node_prefix);
    f.start_bb(&ok_bb);
}

pub fn align_up_dyn(f: &mut FnBuilder, val_i64: &str) -> String {
    let p15 = f.reg();
    f.emit(format!("{p15} = add i64 {val_i64}, 15"));
    let res = f.reg();
    f.emit(format!("{res} = and i64 {p15}, -16"));
    res
}

pub fn result_layout_info(
    ok: &Type,
    err: &Type,
    target: Target,
) -> Result<(u64, u64, String), Diagnostic> {
    let (u_align, count, _) = result_union_layout(ok, err, target)?;
    let lty = llvm_type(
        &Type::Result(Box::new(ok.clone()), Box::new(err.clone())),
        target,
    )?;
    let _ = align_up(1, 1);
    Ok((u_align, count, lty))
}
