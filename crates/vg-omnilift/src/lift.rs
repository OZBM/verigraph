//! P8.3 & P8.4: Mechanical lifting from normalized LLVM IR to VG-Sym / VG-IR with UB curing.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use vg_core::Assembled;
use vg_core::diag::{Diagnostic, Span};
use vg_core::ir::Hash;
use vg_core::types::Type;
use vg_core::value::Value;

use crate::analysis::{
    Interval, NaturalLoop, ParamKind, PtrBase, analyze_memory_and_params, compute_intervals,
    find_natural_loops, inline_and_simplify, is_nsw_proven_safe, term_succs,
};
use crate::ingest::{Language, Provenance, ingest_to_llvm_ir};
use crate::llvm::{
    BasicBlock, BinOpKind, CastKind, Function, GlobalInit, ICmpPred, Inst, InstKind, LlvmType,
    Module, Terminator, ValueRef, parse_module_reachable,
};

fn err(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error("VG-L0001", Span::default(), msg)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UbFinding {
    pub kind: String,
    pub input: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiftedParam {
    Slice {
        name: String,
        elem_bits: u32,
        len_param_idx: Option<usize>,
        ptr_param_idx: usize,
    },
    Array {
        name: String,
        elem_bits: u32,
        len: u64,
        writable: bool,
        ptr_param_idx: usize,
    },
    Scalar {
        name: String,
        bits: u32,
        signed: bool,
        param_idx: usize,
    },
}

#[derive(Clone, Debug)]
pub struct Lifted {
    pub vgsym: String,
    pub nodes: Vec<Assembled>,
    pub root_label: String,
    pub node_hash: Hash,
    pub provenance: Provenance,
    pub original_llvm_ir: String,
    pub entry_fn_name: String,
    pub params: Vec<LiftedParam>,
    pub returns_result: bool,
    pub return_type: Type,
    pub ub_found: Vec<UbFinding>,
    pub bounded_check: String,
    pub unroll: u32,
    pub fuzz_cases: u64,
}

#[derive(Clone, Debug)]
enum VgVal {
    Scalar { port: String, ty: String, bits: u32 },
    Ptr { base: PtrBase, off_port: String },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MemSlot {
    Alloca(usize),
    WritableParam(usize),
}

#[derive(Clone, Debug)]
struct MemSpec {
    elem_bits: u32,
    len: u64,
    vg_type: String,
}

#[derive(Clone, Debug)]
struct NodeBuilder {
    label: String,
    params: Vec<(String, String)>,
    results: Vec<String>,
    effects: BTreeSet<String>,
    pre: Vec<String>,
    post: Vec<String>,
    invariants: Vec<(String, String)>,
    lines: Vec<String>,
    next_port: usize,
    ret_exprs: Vec<String>,
}

impl NodeBuilder {
    fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            params: Vec::new(),
            results: Vec::new(),
            effects: BTreeSet::new(),
            pre: Vec::new(),
            post: Vec::new(),
            invariants: Vec::new(),
            lines: Vec::new(),
            next_port: 0,
            ret_exprs: Vec::new(),
        }
    }

    fn fresh(&mut self) -> String {
        let p = format!("%{}", self.next_port);
        self.next_port += 1;
        p
    }

    fn emit(&mut self, expr: impl Into<String>) -> String {
        let p = self.fresh();
        self.lines.push(format!("    {p} = {};", expr.into()));
        p
    }

    fn emit_annotated(&mut self, ty: &str, expr: impl Into<String>) -> String {
        let p = self.fresh();
        self.lines.push(format!("    {p}: {ty} = {};", expr.into()));
        p
    }

    fn emit_multi(&mut self, count: usize, expr: impl Into<String>) -> Vec<String> {
        let ports: Vec<String> = (0..count).map(|_| self.fresh()).collect();
        self.lines
            .push(format!("    {} = {};", ports.join(", "), expr.into()));
        ports
    }

    fn render(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("NODE @{} {{\n", self.label));
        let ps: Vec<String> = self
            .params
            .iter()
            .map(|(n, t)| format!("{n}: {t}"))
            .collect();
        s.push_str(&format!(
            "  SIGNATURE: ({}) -> ({});\n",
            ps.join(", "),
            self.results.join(", ")
        ));
        let effs: Vec<String> = self.effects.iter().cloned().collect();
        s.push_str(&format!("  EFFECTS: [{}];\n", effs.join(", ")));
        s.push_str("  CONTRACT {\n");
        for p in &self.pre {
            s.push_str(&format!("    PRE: {p};\n"));
        }
        for p in &self.post {
            s.push_str(&format!("    POST: {p};\n"));
        }
        for (lbl, body) in &self.invariants {
            s.push_str(&format!("    INVARIANT @{lbl}(k, acc): {body};\n"));
        }
        s.push_str("  }\n");
        s.push_str("  BODY {\n");
        for l in &self.lines {
            s.push_str(l);
            s.push('\n');
        }
        s.push_str(&format!("    RETURN({});\n", self.ret_exprs.join(", ")));
        s.push_str("  }\n");
        s.push_str("}\n");
        s
    }
}

struct LiftCtx<'a> {
    module: &'a Module,
    func: &'a Function,
    param_kinds: &'a [ParamKind],
    ptr_info: &'a BTreeMap<String, (PtrBase, Option<i64>)>,
    intervals: &'a BTreeMap<String, Interval>,
    loops: &'a [NaturalLoop],
    mem_specs: BTreeMap<MemSlot, MemSpec>,
    lifted_params: Vec<LiftedParam>,
    param_to_vg_name: BTreeMap<usize, String>,
    param_to_vg_type: BTreeMap<usize, String>,
    bb_map: BTreeMap<String, &'a BasicBlock>,
    emitted_nodes: Vec<NodeBuilder>,
    seq: usize,
    has_dynamic_ub: bool,
    ub_found: Vec<UbFinding>,
    ok_ret_ty: String,
    global_consts_used: BTreeMap<String, (u32, Vec<i128>)>,
    inv_seq: usize,
    root_label: String,
}

impl<'a> LiftCtx<'a> {
    fn fresh_node_name(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("{prefix}_{}", self.seq)
    }

    fn fresh_inv_label(&mut self) -> (String, String) {
        let id = self.inv_seq;
        self.inv_seq += 1;
        // Distinct tautology per invariant so vg-core never merges invariants across different acc types (VG-C0101).
        (format!("inv_{id}"), format!("k + {id} >= {id}"))
    }
}

/// Lift a function from C/Rust source file through LLVM IR into verified VG-Sym / VG-IR.
pub fn lift_source(
    path: &Path,
    function: &str,
    language: Language,
    extra_args: &[String],
) -> Result<Lifted, Diagnostic> {
    let ingested = ingest_to_llvm_ir(path, function, language, extra_args)?;
    let mut lifted = lift_llvm_ir(&ingested.llvm_ir, function, ingested.provenance)?;
    lifted.original_llvm_ir = ingested.llvm_ir;
    crate::verify::verify_lifted(&mut lifted, 8, crate::verify::default_fuzz_cases())?;
    Ok(lifted)
}

/// Lift directly from normalized LLVM IR text (useful for unit tests and internal callers).
pub fn lift_llvm_ir(
    llvm_ir: &str,
    function: &str,
    provenance: Provenance,
) -> Result<Lifted, Diagnostic> {
    let module = parse_module_reachable(llvm_ir, function)?;
    let (func, had_null_check) = inline_and_simplify(&module, function)?;
    let (allocas, param_kinds, ptr_info) = analyze_memory_and_params(&module, &func)?;
    let intervals = compute_intervals(&func, &param_kinds);
    let loops = find_natural_loops(&func)?;

    // Determine if function has any dynamic UB sites requiring Result<T, u32>.
    let mut initial_ub = Vec::new();
    if had_null_check {
        let null_witness = build_witness_inputs(&func, &param_kinds, None, true);
        initial_ub.push(UbFinding {
            kind: "null_pointer".to_string(),
            input: null_witness,
        });
    }
    let has_dynamic_ub = detect_dynamic_ub(&func, &param_kinds, &intervals, &mut initial_ub);

    let mut mem_specs = BTreeMap::new();
    for a in &allocas {
        let (elem_bits, len, vg_type) = if a.word_array {
            let n = (a.byte_size / 4).max(1);
            (32, n, format!("Array<u32, {n}>"))
        } else {
            let n = a.byte_size.max(1);
            (8, n, format!("Array<u8, {n}>"))
        };
        mem_specs.insert(
            MemSlot::Alloca(a.id),
            MemSpec {
                elem_bits,
                len,
                vg_type,
            },
        );
    }

    let mut lifted_params = Vec::new();
    let mut param_to_vg_name = BTreeMap::new();
    let mut param_to_vg_type = BTreeMap::new();
    let mut root_sig_params: Vec<(String, String)> = Vec::new();
    let mut used_names = BTreeSet::new();

    for (i, (p, pk)) in func.params.iter().zip(&param_kinds).enumerate() {
        if matches!(pk, ParamKind::SliceLen { .. }) {
            continue;
        }
        let clean = sanitize_ident(&p.name, i, &mut used_names);
        match pk {
            ParamKind::Scalar { bits, signed } => {
                let ty = vg_int_type(*bits, *signed);
                root_sig_params.push((clean.clone(), ty.clone()));
                param_to_vg_name.insert(i, clean.clone());
                param_to_vg_type.insert(i, ty);
                lifted_params.push(LiftedParam::Scalar {
                    name: clean,
                    bits: *bits,
                    signed: *signed,
                    param_idx: i,
                });
            }
            ParamKind::Slice { len_param_idx } => {
                root_sig_params.push((clean.clone(), "Slice<u8>".to_string()));
                param_to_vg_name.insert(i, clean.clone());
                param_to_vg_type.insert(i, "Slice<u8>".to_string());
                lifted_params.push(LiftedParam::Slice {
                    name: clean,
                    elem_bits: 8,
                    len_param_idx: *len_param_idx,
                    ptr_param_idx: i,
                });
            }
            ParamKind::FixedArray {
                elem_bits,
                len,
                writable,
            } => {
                let ty = format!("Array<u{elem_bits}, {len}>");
                root_sig_params.push((clean.clone(), ty.clone()));
                param_to_vg_name.insert(i, clean.clone());
                param_to_vg_type.insert(i, ty.clone());
                if *writable {
                    mem_specs.insert(
                        MemSlot::WritableParam(i),
                        MemSpec {
                            elem_bits: *elem_bits,
                            len: *len,
                            vg_type: ty,
                        },
                    );
                }
                lifted_params.push(LiftedParam::Array {
                    name: clean,
                    elem_bits: *elem_bits,
                    len: *len,
                    writable: *writable,
                    ptr_param_idx: i,
                });
            }
            ParamKind::SliceLen { .. } => unreachable!(),
        }
    }

    // Determine return type:
    // If func.ret_ty is non-void: that scalar type; else if writable fixed array param(s) exist: those arrays!
    let writable_params: Vec<MemSlot> = mem_specs
        .keys()
        .filter(|s| matches!(s, MemSlot::WritableParam(_)))
        .cloned()
        .collect();
    let mut root_results = Vec::new();
    match &func.ret_ty {
        LlvmType::Void => {
            for slot in &writable_params {
                root_results.push(mem_specs[slot].vg_type.clone());
            }
            if root_results.is_empty() {
                return Err(err("void function with no writable output"));
            }
        }
        LlvmType::Int(bits) => {
            let signed = has_signed_usage(&func);
            root_results.push(vg_int_type(*bits, signed));
        }
        other => return Err(err(format!("unsupported return type {other:?}"))),
    }

    let ok_ret_ty = if root_results.len() == 1 {
        root_results[0].clone()
    } else {
        format!("Tuple<{}>", root_results.join(", "))
    };
    let final_results = if has_dynamic_ub {
        if root_results.len() != 1 {
            return Err(err(
                "Result wrapping for multi-result functions not supported",
            ));
        }
        vec![format!("Result<{}, u32>", root_results[0])]
    } else {
        root_results.clone()
    };

    let bb_map = func.blocks.iter().map(|b| (b.label.clone(), b)).collect();

    let root_label = sanitize_ident(function, 999, &mut BTreeSet::new());

    let mut ctx = LiftCtx {
        module: &module,
        func: &func,
        param_kinds: &param_kinds,
        ptr_info: &ptr_info,
        intervals: &intervals,
        loops: &loops,
        mem_specs,
        lifted_params,
        param_to_vg_name,
        param_to_vg_type,
        bb_map,
        emitted_nodes: Vec::new(),
        seq: 0,
        has_dynamic_ub,
        ub_found: initial_ub,
        ok_ret_ty,
        global_consts_used: BTreeMap::new(),
        inv_seq: 0,
        root_label: root_label.clone(),
    };

    let mut root_nb = NodeBuilder::new(root_label.clone());
    root_nb.params = root_sig_params;
    root_nb.results = final_results;

    // Initialize SSA env and memory slots in root node.
    let mut env: BTreeMap<String, VgVal> = BTreeMap::new();
    let mut mem_ports: BTreeMap<MemSlot, String> = BTreeMap::new();

    for (i, (p, pk)) in func.params.iter().zip(&param_kinds).enumerate() {
        match pk {
            ParamKind::Scalar { bits, .. } => {
                let vg_name = ctx.param_to_vg_name[&i].clone();
                let vg_ty = ctx.param_to_vg_type[&i].clone();
                env.insert(
                    p.name.clone(),
                    VgVal::Scalar {
                        port: vg_name,
                        ty: vg_ty,
                        bits: *bits,
                    },
                );
            }
            ParamKind::Slice { .. } => {
                env.insert(
                    p.name.clone(),
                    VgVal::Ptr {
                        base: PtrBase::Param(i),
                        off_port: "0:u64".to_string(),
                    },
                );
            }
            ParamKind::FixedArray { writable, .. } => {
                let vg_name = ctx.param_to_vg_name[&i].clone();
                env.insert(
                    p.name.clone(),
                    VgVal::Ptr {
                        base: PtrBase::Param(i),
                        off_port: "0:u64".to_string(),
                    },
                );
                if *writable {
                    mem_ports.insert(MemSlot::WritableParam(i), vg_name);
                }
            }
            ParamKind::SliceLen { slice_param_idx } => {
                let slice_name = ctx.param_to_vg_name[slice_param_idx].clone();
                let len_port = root_nb.emit(format!("LEN({slice_name})"));
                let bits = p.ty.bit_width().unwrap_or(64);
                let final_len_port = if bits < 64 {
                    cast_to_unsigned(&mut root_nb, &len_port, 64, bits)
                } else {
                    len_port
                };
                env.insert(
                    p.name.clone(),
                    VgVal::Scalar {
                        port: final_len_port,
                        ty: format!("u{bits}"),
                        bits,
                    },
                );
            }
        }
    }

    for a in &allocas {
        let spec = &ctx.mem_specs[&MemSlot::Alloca(a.id)];
        let zeros = vec!["0"; spec.len as usize].join(", ");
        let arr_port = root_nb.emit(format!("IDENTITY([{zeros}]:{})", spec.vg_type));
        mem_ports.insert(MemSlot::Alloca(a.id), arr_port.clone());
        env.insert(
            a.res_name.clone(),
            VgVal::Ptr {
                base: PtrBase::Alloca(a.id),
                off_port: "0:u64".to_string(),
            },
        );
    }

    let mut global_ports = BTreeMap::new();
    // Collect any global constants referenced anywhere in func.
    collect_used_globals(ctx.module, &func, &mut ctx.global_consts_used)?;
    for (gname, (bits, vals)) in &ctx.global_consts_used {
        let elems: Vec<String> = vals
            .iter()
            .map(|v| mask_to_u128(*v, *bits).to_string())
            .collect();
        let gty = format!("Array<u{bits}, {}>", vals.len());
        let gport = root_nb.emit(format!("IDENTITY([{}]:{gty})", elems.join(", ")));
        global_ports.insert(gname.clone(), (gport, gty));
    }

    let entry_lbl = func.blocks[0].label.clone();
    let ret_vals = lower_region(
        &mut ctx,
        &mut root_nb,
        &entry_lbl,
        None,
        &mut env,
        &mut mem_ports,
        &global_ports,
        &writable_params,
    )?;
    root_nb.ret_exprs = ret_vals;
    ctx.emitted_nodes.push(root_nb);

    // Build module VG-Sym text.
    let mut vgsym = format!("module {root_label}_lifted;\n\n");
    for n in &ctx.emitted_nodes {
        vgsym.push_str(&n.render());
        vgsym.push('\n');
    }

    let assembled = vg_core::asm_local(&vgsym).map_err(|d| {
        let bad_line = vgsym
            .lines()
            .nth(d.span.line.saturating_sub(1) as usize)
            .unwrap_or("");
        err(format!(
            "internal VG-Sym assembly error ({}): {} at line {}: `{}`",
            d.code,
            d.message,
            d.span.line,
            bad_line.trim()
        ))
    })?;
    vg_types::check_source(&vgsym).map_err(|ds| {
        let msg = ds
            .iter()
            .map(|d| format!("{}: {}", d.code, d.message))
            .collect::<Vec<_>>()
            .join("; ");
        err(format!("lifted VG-Sym failed type check: {msg}"))
    })?;

    let root_asm = assembled
        .last()
        .ok_or_else(|| err("assembled module is empty"))?;
    let node_hash = root_asm.hashes.node;
    let return_type = root_asm
        .node
        .results
        .first()
        .cloned()
        .ok_or_else(|| err("root node has no result"))?;

    Ok(Lifted {
        vgsym,
        nodes: assembled,
        root_label,
        node_hash,
        provenance,
        original_llvm_ir: llvm_ir.to_string(),
        entry_fn_name: function.to_string(),
        params: ctx.lifted_params,
        returns_result: has_dynamic_ub,
        return_type,
        ub_found: ctx.ub_found,
        bounded_check: "not run".to_string(),
        unroll: 0,
        fuzz_cases: 0,
    })
}

fn sanitize_ident(raw: &str, idx: usize, used: &mut BTreeSet<String>) -> String {
    let mut s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() || s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s = format!("p{idx}_{s}");
    }
    if s.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        s = format!("v_{s}");
    }
    // Avoid VG-Sym keywords.
    const RESERVED: &[&str] = &[
        "module",
        "true",
        "false",
        "result",
        "in",
        "pre",
        "post",
        "spec",
        "fold",
        "len",
        "effects",
        "intent",
        "export",
        "inline",
        "u8",
        "u16",
        "u32",
        "u64",
        "i8",
        "i16",
        "i32",
        "i64",
        "f16",
        "bf16",
        "f32",
        "f64",
        "init",
        "invariant",
        "else",
        "abs",
        "min",
        "max",
        "same",
        "ok",
        "err",
    ];
    if RESERVED.contains(&s.as_str()) {
        s = format!("v_{s}");
    }
    while !used.insert(s.clone()) {
        s.push('_');
    }
    s
}

fn vg_int_type(bits: u32, signed: bool) -> String {
    match bits {
        1 => "Bool".to_string(),
        8 | 16 | 32 | 64 => {
            if signed {
                format!("i{bits}")
            } else {
                format!("u{bits}")
            }
        }
        _ => "u64".to_string(),
    }
}

fn has_signed_usage(func: &Function) -> bool {
    // Only use signed return type if the returned value/ops are explicitly `sdiv`/`srem`/`ashr` or `add/sub/mul nsw` on <= 32-bit signed params.
    for p in &func.params {
        if p.ty.bit_width().is_some_and(|b| b <= 32) {
            for bb in &func.blocks {
                for inst in &bb.insts {
                    if let InstKind::BinOp {
                        op: BinOpKind::SDiv | BinOpKind::SRem | BinOpKind::AShr,
                        ..
                    } = &inst.kind
                    {
                        return true;
                    }
                    if let InstKind::BinOp {
                        op: BinOpKind::Add | BinOpKind::Sub | BinOpKind::Mul,
                        nsw: true,
                        ty: LlvmType::Int(32),
                        lhs,
                        rhs,
                        ..
                    } = &inst.kind
                        && (*lhs == ValueRef::Local(p.name.clone())
                            || *rhs == ValueRef::Local(p.name.clone()))
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn mask_to_u128(v: i128, bits: u32) -> u128 {
    if bits >= 128 {
        v as u128
    } else {
        (v as u128) & ((1u128 << bits) - 1)
    }
}

fn collect_used_globals(
    module: &Module,
    func: &Function,
    out: &mut BTreeMap<String, (u32, Vec<i128>)>,
) -> Result<(), Diagnostic> {
    let mut names = BTreeSet::new();
    for bb in &func.blocks {
        for inst in &bb.insts {
            match &inst.kind {
                InstKind::Gep {
                    ptr: ValueRef::Global(g),
                    ..
                }
                | InstKind::Load {
                    ptr: ValueRef::Global(g),
                    ..
                } => {
                    names.insert(g.clone());
                }
                _ => {}
            }
        }
    }
    for gname in names {
        let g = module
            .globals
            .get(&gname)
            .ok_or_else(|| err(format!("unknown global @{gname}")))?;
        match &g.init {
            GlobalInit::IntArray { elem_bits, values } => {
                out.insert(gname, (*elem_bits, values.clone()));
            }
            GlobalInit::ZeroInit => {
                if let LlvmType::Array(n, elem) = &g.ty
                    && let Some(bits) = elem.bit_width()
                {
                    out.insert(gname, (bits, vec![0; *n as usize]));
                }
            }
            _ => return Err(err(format!("unsupported global @{gname}"))),
        }
    }
    Ok(())
}

fn detect_dynamic_ub(
    func: &Function,
    param_kinds: &[ParamKind],
    intervals: &BTreeMap<String, Interval>,
    ub_found: &mut Vec<UbFinding>,
) -> bool {
    let mut found_dynamic = false;
    let mut seen_kinds = BTreeSet::new();

    for bb in &func.blocks {
        for inst in &bb.insts {
            match &inst.kind {
                InstKind::BinOp {
                    op: op @ (BinOpKind::Add | BinOpKind::Sub | BinOpKind::Mul),
                    nsw: true,
                    ty,
                    lhs,
                    rhs,
                    ..
                } => {
                    let bits = ty.bit_width().unwrap_or(64);
                    if !is_nsw_proven_safe(func, &bb.label, *op, bits, lhs, rhs, intervals) {
                        found_dynamic = true;
                        if seen_kinds.insert("signed_overflow") {
                            ub_found.push(UbFinding {
                                kind: "signed_overflow".to_string(),
                                input: build_witness_inputs(
                                    func,
                                    param_kinds,
                                    Some(("signed_overflow", bits)),
                                    false,
                                ),
                            });
                        }
                    }
                }
                InstKind::BinOp {
                    op: BinOpKind::UDiv | BinOpKind::URem | BinOpKind::SDiv | BinOpKind::SRem,
                    ty,
                    rhs,
                    ..
                } => {
                    let bits = ty.bit_width().unwrap_or(64);
                    let safe_nonzero = match rhs {
                        ValueRef::ConstInt { value, .. } => *value != 0,
                        ValueRef::Local(n) => intervals.get(n).is_some_and(|iv| iv.umin > 0),
                        _ => false,
                    };
                    if !safe_nonzero {
                        found_dynamic = true;
                        if seen_kinds.insert("div_by_zero") {
                            ub_found.push(UbFinding {
                                kind: "div_by_zero".to_string(),
                                input: build_witness_inputs(
                                    func,
                                    param_kinds,
                                    Some(("div_by_zero", bits)),
                                    false,
                                ),
                            });
                        }
                    }
                    if matches!(
                        inst.kind,
                        InstKind::BinOp {
                            op: BinOpKind::SDiv | BinOpKind::SRem,
                            ..
                        }
                    ) && !matches!(rhs, ValueRef::ConstInt { value, .. } if *value != -1)
                    {
                        found_dynamic = true;
                        if seen_kinds.insert("signed_div_overflow") {
                            ub_found.push(UbFinding {
                                kind: "signed_div_overflow".to_string(),
                                input: build_witness_inputs(
                                    func,
                                    param_kinds,
                                    Some(("signed_div_overflow", bits)),
                                    false,
                                ),
                            });
                        }
                    }
                }
                InstKind::BinOp {
                    op: BinOpKind::Shl | BinOpKind::LShr | BinOpKind::AShr,
                    ty,
                    rhs,
                    ..
                } => {
                    let bits = ty.bit_width().unwrap_or(64);
                    let safe_shift = match rhs {
                        ValueRef::ConstInt { value, .. } => {
                            *value >= 0 && (*value as u64) < u64::from(bits)
                        }
                        ValueRef::Local(n) => {
                            intervals.get(n).is_some_and(|iv| iv.umax < u64::from(bits))
                        }
                        _ => false,
                    };
                    if !safe_shift {
                        found_dynamic = true;
                        if seen_kinds.insert("shift_overflow") {
                            ub_found.push(UbFinding {
                                kind: "shift_overflow".to_string(),
                                input: build_witness_inputs(
                                    func,
                                    param_kinds,
                                    Some(("shift_overflow", bits)),
                                    false,
                                ),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Check unguarded slice indexing: if a Slice param has no paired length param (raw ptr + index),
    // any load through a non-loop-bounded offset can go out of bounds.
    for (pi, pk) in param_kinds.iter().enumerate() {
        if let ParamKind::Slice {
            len_param_idx: None,
        } = pk
        {
            let _ = pi;
            found_dynamic = true;
            if seen_kinds.insert("out_of_bounds") {
                ub_found.push(UbFinding {
                    kind: "out_of_bounds".to_string(),
                    input: build_witness_inputs(
                        func,
                        param_kinds,
                        Some(("out_of_bounds", 64)),
                        false,
                    ),
                });
            }
        }
    }
    found_dynamic
}

fn build_witness_inputs(
    func: &Function,
    param_kinds: &[ParamKind],
    ub: Option<(&str, u32)>,
    empty_slice: bool,
) -> Vec<Value> {
    let mut out = Vec::new();
    let mut scalar_pos = 0usize;
    for (p, pk) in func.params.iter().zip(param_kinds) {
        let _ = p;
        match pk {
            ParamKind::Slice { .. } => {
                if empty_slice {
                    out.push(Value::Seq(vec![]));
                } else {
                    out.push(Value::Seq(vec![Value::Int(0); 4]));
                }
            }
            ParamKind::FixedArray { elem_bits, len, .. } => {
                let _ = elem_bits;
                out.push(Value::Seq(vec![Value::Int(0); *len as usize]));
            }
            ParamKind::Scalar { bits, signed } => {
                let val = match ub {
                    Some(("signed_overflow", b)) => {
                        let smax = (1i128 << (b - 1)) - 1;
                        if scalar_pos == 0 { smax } else { 1 }
                    }
                    Some(("div_by_zero", _)) => {
                        if scalar_pos == 0 {
                            1
                        } else {
                            0
                        }
                    }
                    Some(("signed_div_overflow", b)) => {
                        let smin = -(1i128 << (b - 1));
                        if scalar_pos == 0 { smin } else { -1 }
                    }
                    Some(("shift_overflow", b)) => {
                        if scalar_pos == 0 {
                            1
                        } else {
                            i128::from(b)
                        }
                    }
                    Some(("out_of_bounds", _)) => 1024,
                    _ => {
                        let _ = (bits, signed);
                        0
                    }
                };
                scalar_pos += 1;
                out.push(Value::Int(val));
            }
            ParamKind::SliceLen { .. } => {}
        }
    }
    out
}

/// Find immediate merge block (first common post-dominator reachable from both branches)
/// inside the current acyclic region up to `stop_bb`.
fn find_merge_block(
    ctx: &LiftCtx<'_>,
    then_bb: &str,
    else_bb: &str,
    stop_bb: Option<&str>,
) -> Option<String> {
    let reach = |start: &str| -> BTreeMap<String, usize> {
        let mut dist = BTreeMap::new();
        let mut queue = std::collections::VecDeque::new();
        dist.insert(start.to_string(), 0usize);
        queue.push_back(start.to_string());
        while let Some(cur) = queue.pop_front() {
            if stop_bb == Some(cur.as_str()) {
                continue;
            }
            let d = dist[&cur];
            // Skip through loops directly to their exit block.
            if let Some(lp) = ctx.loops.iter().rev().find(|l| l.header == cur) {
                if !dist.contains_key(&lp.exit_block) {
                    dist.insert(lp.exit_block.clone(), d + 1);
                    queue.push_back(lp.exit_block.clone());
                }
                continue;
            }
            let bb = ctx.bb_map[cur.as_str()];
            for s in term_succs(&bb.term) {
                if !dist.contains_key(&s) {
                    dist.insert(s.clone(), d + 1);
                    queue.push_back(s);
                }
            }
        }
        dist
    };

    let d1 = reach(then_bb);
    let d2 = reach(else_bb);
    let mut candidates: Vec<(usize, String)> = d1
        .iter()
        .filter_map(|(k, &v1)| d2.get(k).map(|&v2| (v1 + v2, k.clone())))
        .collect();
    candidates.sort();
    candidates.into_iter().map(|(_, k)| k).next()
}

fn cast_to_unsigned(nb: &mut NodeBuilder, port: &str, from_bits: u32, to_bits: u32) -> String {
    if from_bits == to_bits {
        return port.to_string();
    }
    if to_bits > from_bits {
        return nb.emit(format!("CAST.u{to_bits}({port})"));
    }
    let mask = (1u64 << to_bits) - 1;
    let masked = nb.emit(format!("AND({port}, {mask}:u{from_bits})"));
    nb.emit(format!("CAST.u{to_bits}({masked})"))
}

fn emit_scalar_const(v: i128, bits: u32, ty: &str) -> String {
    if bits == 1 {
        if v != 0 {
            "true".to_string()
        } else {
            "false".to_string()
        }
    } else if ty.starts_with('i') {
        let shift = 128 - bits;
        let sv = ((mask_to_u128(v, bits) << shift) as i128) >> shift;
        format!("{sv}:{ty}")
    } else {
        format!("{}:{ty}", mask_to_u128(v, bits))
    }
}

fn coerce_to_unsigned(nb: &mut NodeBuilder, val: &VgVal) -> (String, u32) {
    match val {
        VgVal::Scalar { port, ty, bits } => {
            if *bits == 1 {
                (nb.emit(format!("CAST.u8({port})")), 8)
            } else if ty.starts_with('i') {
                (nb.emit(format!("CAST.u{bits}({port})")), *bits)
            } else {
                (port.clone(), *bits)
            }
        }
        VgVal::Ptr { off_port, .. } => (off_port.clone(), 64),
    }
}

fn coerce_to_type(nb: &mut NodeBuilder, val: &VgVal, target_ty: &str, target_bits: u32) -> String {
    match val {
        VgVal::Scalar { port, ty, bits } => {
            if ty == target_ty {
                return port.clone();
            }
            if target_ty == "Bool" {
                return nb.emit(format!("NE({port}, 0:{ty})"));
            }
            if *bits == 1 {
                return nb.emit(format!("CAST.{target_ty}({port})"));
            }
            if *bits == target_bits {
                // Signed <-> unsigned bitcast via CAST (wait: vg-types requires CAST.in_range for i32 <-> u32? Let's check!).
                // In docs/14, CAST i32 <-> u32 adds CAST.in_range! Instead of signed types internally,
                // keep all intermediate arithmetic in uN and CAST to iN only if needed!
                return nb.emit(format!("CAST.{target_ty}({port})"));
            }
            if target_bits > *bits {
                nb.emit(format!("CAST.{target_ty}({port})"))
            } else {
                let u = coerce_to_unsigned(nb, val).0;
                let mask = (1u64 << target_bits) - 1;
                let masked = nb.emit(format!("AND({u}, {mask}:u{bits})"));
                nb.emit(format!("CAST.{target_ty}({masked})"))
            }
        }
        VgVal::Ptr { off_port, .. } => off_port.clone(),
    }
}

/// Lower a region `[cur_bb .. stop_bb)` into `nb`, returning the output port expressions when reaching
/// `stop_bb` (if `stop_bb` is `Some`, returning `[]` and populating `phi`s? Better: return values at `stop_bb`, or final node results if `stop_bb` is `None`).
#[allow(clippy::too_many_arguments)]
fn lower_region(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    start_bb: &str,
    stop_bb: Option<&str>,
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
    writable_params: &[MemSlot],
) -> Result<Vec<String>, Diagnostic> {
    lower_region_impl(
        ctx,
        nb,
        start_bb,
        None,
        false,
        stop_bb,
        env,
        mem_ports,
        global_ports,
        writable_params,
    )
}

#[allow(clippy::too_many_arguments)]
fn lower_region_impl(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    start_bb: &str,
    init_prev: Option<&str>,
    mut skip_first_phis: bool,
    stop_bb: Option<&str>,
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
    writable_params: &[MemSlot],
) -> Result<Vec<String>, Diagnostic> {
    let mut cur = start_bb.to_string();
    let mut prev: Option<String> = init_prev.map(str::to_string);

    loop {
        if stop_bb == Some(cur.as_str()) {
            return Ok(Vec::new());
        }

        // Check if `cur` is the header of a natural loop whose body is inside this region.
        if !skip_first_phis && let Some(loop_idx) = ctx.loops.iter().rposition(|l| l.header == cur)
        {
            let exit_bb = ctx.loops[loop_idx].exit_block.clone();
            lower_natural_loop(
                ctx,
                nb,
                loop_idx,
                prev.as_deref(),
                env,
                mem_ports,
                global_ports,
            )?;
            prev = Some(ctx.loops[loop_idx].latch.clone());
            cur = exit_bb;
            continue;
        }

        let bb = ctx.bb_map[cur.as_str()].clone();
        if !skip_first_phis {
            for inst in &bb.insts {
                if let InstKind::Phi { ty, incoming } = &inst.kind {
                    let res = inst.result.as_ref().unwrap();
                    let chosen = if let Some(p) = &prev {
                        incoming
                            .iter()
                            .find(|(_, lbl)| lbl == p)
                            .or_else(|| incoming.first())
                            .map(|(v, _)| v)
                            .unwrap()
                    } else {
                        &incoming[0].0
                    };
                    let val = resolve_value(ctx, nb, chosen, ty, env, global_ports)?;
                    env.insert(res.clone(), val);
                }
            }
        }
        skip_first_phis = false;

        // Lower non-Phi instructions in `bb`.
        for (inst_idx, inst) in bb.insts.iter().enumerate() {
            if matches!(inst.kind, InstKind::Phi { .. }) {
                continue;
            }
            if ctx.has_dynamic_ub
                && let Some((cond_port, err_code)) =
                    emit_ub_guard_cond(ctx, nb, &bb.label, inst, env)?
            {
                let remaining_insts = &bb.insts[inst_idx..];
                return lower_guarded_continuation(
                    ctx,
                    nb,
                    &cond_port,
                    err_code,
                    &bb.label,
                    remaining_insts,
                    &bb.term,
                    stop_bb,
                    env,
                    mem_ports,
                    global_ports,
                    writable_params,
                );
            }
            lower_instruction(ctx, nb, inst, env, mem_ports, global_ports)?;
        }

        // Handle terminator.
        match &bb.term {
            Terminator::Br { target } => {
                prev = Some(cur);
                cur = target.clone();
            }
            Terminator::CondBr {
                cond,
                then_bb,
                else_bb,
            } => {
                let cond_val = resolve_value(ctx, nb, cond, &LlvmType::Int(1), env, global_ports)?;
                let VgVal::Scalar {
                    port: cond_port, ..
                } = cond_val
                else {
                    return Err(err("branch condition must be Bool"));
                };
                let merge = find_merge_block(ctx, then_bb, else_bb, stop_bb);

                if let Some(ref merge_lbl) = merge {
                    let merge_bb = ctx.bb_map[merge_lbl.as_str()].clone();
                    let mut merge_phis: Vec<(String, LlvmType)> = Vec::new();
                    for inst in &merge_bb.insts {
                        if let InstKind::Phi { ty, .. } = &inst.kind {
                            merge_phis.push((inst.result.clone().unwrap(), ty.clone()));
                        }
                    }
                    let arm_defs = collect_defs_between(ctx, then_bb, else_bb, merge_lbl);
                    let post_uses = collect_uses_from(ctx, merge_lbl, stop_bb);
                    let extra_live: Vec<(String, String, u32)> = arm_defs
                        .into_iter()
                        .filter(|(n, _)| {
                            post_uses.contains(n) && !merge_phis.iter().any(|(pn, _)| pn == n)
                        })
                        .map(|(n, bits)| (n, format!("u{bits}"), bits))
                        .collect();

                    let mem_keys: Vec<MemSlot> = mem_ports.keys().cloned().collect();
                    let mut out_types = Vec::new();
                    for (_, ty) in &merge_phis {
                        let tstr = match ty {
                            LlvmType::Ptr => "u64".to_string(),
                            LlvmType::Int(1) => "Bool".to_string(),
                            LlvmType::Int(b) => format!("u{b}"),
                            _ => return Err(err(format!("unsupported phi type {ty:?}"))),
                        };
                        out_types.push(tstr);
                    }
                    for (_, tstr, _) in &extra_live {
                        out_types.push(tstr.clone());
                    }
                    for slot in &mem_keys {
                        out_types.push(ctx.mem_specs[slot].vg_type.clone());
                    }

                    let dummy_out = out_types.is_empty();
                    if dummy_out {
                        out_types.push("Bool".to_string());
                    }

                    let (branch_params, branch_args) =
                        collect_live_inputs(ctx, env, mem_ports, global_ports);

                    let then_name = ctx.fresh_node_name("then");
                    let else_name = ctx.fresh_node_name("else");

                    let then_nb = build_merge_branch_node(
                        ctx,
                        &then_name,
                        &cur,
                        then_bb,
                        merge_lbl,
                        &branch_params,
                        &out_types,
                        &merge_phis,
                        &extra_live,
                        &mem_keys,
                        dummy_out,
                        env,
                        mem_ports,
                        global_ports,
                        writable_params,
                    )?;
                    let else_nb = build_merge_branch_node(
                        ctx,
                        &else_name,
                        &cur,
                        else_bb,
                        merge_lbl,
                        &branch_params,
                        &out_types,
                        &merge_phis,
                        &extra_live,
                        &mem_keys,
                        dummy_out,
                        env,
                        mem_ports,
                        global_ports,
                        writable_params,
                    )?;

                    nb.effects.extend(then_nb.effects.iter().cloned());
                    nb.effects.extend(else_nb.effects.iter().cloned());
                    ctx.emitted_nodes.push(then_nb);
                    ctx.emitted_nodes.push(else_nb);

                    let arg_list = branch_args.join(", ");
                    let call_expr = if arg_list.is_empty() {
                        format!("ASSERT_GUARD.branch({cond_port}, @{then_name}, else=@{else_name})")
                    } else {
                        format!(
                            "ASSERT_GUARD.branch({cond_port}, @{then_name}, {arg_list}, else=@{else_name})"
                        )
                    };
                    let res_ports = nb.emit_multi(out_types.len(), call_expr);

                    let mut pos = 0;
                    for (pname, pty) in &merge_phis {
                        let port = res_ports[pos].clone();
                        pos += 1;
                        match pty {
                            LlvmType::Ptr => {
                                let base = resolve_phi_ptr_base(&merge_bb, pname, env)?;
                                env.insert(
                                    pname.clone(),
                                    VgVal::Ptr {
                                        base,
                                        off_port: port,
                                    },
                                );
                            }
                            LlvmType::Int(b) => {
                                env.insert(
                                    pname.clone(),
                                    VgVal::Scalar {
                                        port,
                                        ty: if *b == 1 {
                                            "Bool".to_string()
                                        } else {
                                            format!("u{b}")
                                        },
                                        bits: *b,
                                    },
                                );
                            }
                            _ => {}
                        }
                    }
                    for (ename, tstr, bits) in &extra_live {
                        let port = res_ports[pos].clone();
                        pos += 1;
                        env.insert(
                            ename.clone(),
                            VgVal::Scalar {
                                port,
                                ty: tstr.clone(),
                                bits: *bits,
                            },
                        );
                    }
                    for slot in &mem_keys {
                        mem_ports.insert(slot.clone(), res_ports[pos].clone());
                        pos += 1;
                    }

                    if stop_bb == Some(merge_lbl.as_str()) {
                        return Ok(Vec::new());
                    }
                    cur = merge_lbl.clone();
                    prev = None;
                    skip_first_phis = true;
                    continue;
                } else {
                    let (branch_params, branch_args) =
                        collect_live_inputs(ctx, env, mem_ports, global_ports);
                    let res_tys = nb.results.clone();
                    let then_name = ctx.fresh_node_name("then");
                    let else_name = ctx.fresh_node_name("else");

                    let then_nb = build_tail_branch_node(
                        ctx,
                        &then_name,
                        &cur,
                        then_bb,
                        stop_bb,
                        &branch_params,
                        &res_tys,
                        env,
                        mem_ports,
                        global_ports,
                        writable_params,
                    )?;
                    let else_nb = build_tail_branch_node(
                        ctx,
                        &else_name,
                        &cur,
                        else_bb,
                        stop_bb,
                        &branch_params,
                        &res_tys,
                        env,
                        mem_ports,
                        global_ports,
                        writable_params,
                    )?;
                    nb.effects.extend(then_nb.effects.iter().cloned());
                    nb.effects.extend(else_nb.effects.iter().cloned());
                    ctx.emitted_nodes.push(then_nb);
                    ctx.emitted_nodes.push(else_nb);

                    let arg_list = branch_args.join(", ");
                    let call_expr = if arg_list.is_empty() {
                        format!("ASSERT_GUARD.branch({cond_port}, @{then_name}, else=@{else_name})")
                    } else {
                        format!(
                            "ASSERT_GUARD.branch({cond_port}, @{then_name}, {arg_list}, else=@{else_name})"
                        )
                    };
                    return Ok(nb.emit_multi(res_tys.len(), call_expr));
                }
            }
            Terminator::Ret(opt_val) => {
                let mut raw_rets = Vec::new();
                if let Some((rty, rval)) = opt_val {
                    let v = resolve_value(ctx, nb, rval, rty, env, global_ports)?;
                    let target_ty = if ctx.has_dynamic_ub {
                        ctx.ok_ret_ty.clone()
                    } else {
                        nb.results[0].clone()
                    };
                    let bits = rty.bit_width().unwrap_or(64);
                    let p = coerce_to_type(nb, &v, &target_ty, bits);
                    raw_rets.push(p);
                } else {
                    for slot in writable_params {
                        raw_rets.push(mem_ports[slot].clone());
                    }
                }
                if ctx.has_dynamic_ub && stop_bb.is_none() {
                    let res_ty = nb.results[0].clone();
                    let ok_p = nb.emit_annotated(&res_ty, format!("OK({})", raw_rets[0]));
                    return Ok(vec![ok_p]);
                }
                return Ok(raw_rets);
            }
            Terminator::Unreachable => {
                return Err(err("reachable `unreachable` terminator in target"));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_from_non_phi(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    bb: &BasicBlock,
    stop_bb: Option<&str>,
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
    writable_params: &[MemSlot],
) -> Result<Vec<String>, Diagnostic> {
    for (inst_idx, inst) in bb.insts.iter().enumerate() {
        if matches!(inst.kind, InstKind::Phi { .. }) {
            continue;
        }
        if ctx.has_dynamic_ub
            && let Some((cond_port, err_code)) = emit_ub_guard_cond(ctx, nb, &bb.label, inst, env)?
        {
            let remaining_insts = &bb.insts[inst_idx..];
            return lower_guarded_continuation(
                ctx,
                nb,
                &cond_port,
                err_code,
                &bb.label,
                remaining_insts,
                &bb.term,
                stop_bb,
                env,
                mem_ports,
                global_ports,
                writable_params,
            );
        }
        lower_instruction(ctx, nb, inst, env, mem_ports, global_ports)?;
    }
    match &bb.term {
        Terminator::Br { target } => lower_region(
            ctx,
            nb,
            target,
            stop_bb,
            env,
            mem_ports,
            global_ports,
            writable_params,
        ),
        Terminator::Ret(opt_val) => {
            let mut raw_rets = Vec::new();
            if let Some((rty, rval)) = opt_val {
                let v = resolve_value(ctx, nb, rval, rty, env, global_ports)?;
                let target_ty = if ctx.has_dynamic_ub {
                    ctx.ok_ret_ty.clone()
                } else {
                    nb.results[0].clone()
                };
                let bits = rty.bit_width().unwrap_or(64);
                raw_rets.push(coerce_to_type(nb, &v, &target_ty, bits));
            } else {
                for slot in writable_params {
                    raw_rets.push(mem_ports[slot].clone());
                }
            }
            if ctx.has_dynamic_ub && stop_bb.is_none() {
                let res_ty = nb.results[0].clone();
                let ok_p = nb.emit_annotated(&res_ty, format!("OK({})", raw_rets[0]));
                return Ok(vec![ok_p]);
            }
            Ok(raw_rets)
        }
        Terminator::CondBr { .. } => {
            // Unreachable when using lower_region_impl directly; kept only for lower_guarded_continuation.
            lower_region_impl(
                ctx,
                nb,
                &bb.label,
                None,
                true,
                stop_bb,
                env,
                mem_ports,
                global_ports,
                writable_params,
            )
        }
        Terminator::Unreachable => Err(err("reachable `unreachable` terminator")),
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_guarded_continuation(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    cond_port: &str,
    err_code: u32,
    cur_bb_label: &str,
    remaining_insts: &[Inst],
    term: &Terminator,
    stop_bb: Option<&str>,
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
    writable_params: &[MemSlot],
) -> Result<Vec<String>, Diagnostic> {
    let (branch_params, branch_args) = collect_live_inputs(ctx, env, mem_ports, global_ports);
    let res_ty = format!("Result<{}, u32>", ctx.ok_ret_ty);

    let err_name = ctx.fresh_node_name("ub_err");
    let mut err_nb = NodeBuilder::new(err_name.clone());
    err_nb.params = branch_params
        .iter()
        .map(|(_, pname, ty)| (pname.clone(), ty.clone()))
        .collect();
    err_nb.results = vec![res_ty.clone()];
    let err_val = err_nb.emit_annotated(&res_ty, format!("ERR({err_code}:u32)"));
    err_nb.ret_exprs = vec![err_val];

    let ok_name = ctx.fresh_node_name("ub_ok");
    let mut ok_nb = NodeBuilder::new(ok_name.clone());
    ok_nb.params = branch_params
        .iter()
        .map(|(_, pname, ty)| (pname.clone(), ty.clone()))
        .collect();
    ok_nb.results = vec![res_ty.clone()];

    let mut sub_env = BTreeMap::new();
    let mut sub_mem = BTreeMap::new();
    let mut sub_glob = BTreeMap::new();
    bind_branch_params(
        &branch_params,
        env,
        mem_ports,
        global_ports,
        &mut sub_env,
        &mut sub_mem,
        &mut sub_glob,
    );

    // Lower the guarded instruction WITHOUT re-checking its guard, then continue the block!
    lower_instruction(
        ctx,
        &mut ok_nb,
        &remaining_insts[0],
        &mut sub_env,
        &mut sub_mem,
        &sub_glob,
    )?;
    let synthetic_bb = BasicBlock {
        label: cur_bb_label.to_string(),
        insts: remaining_insts[1..].to_vec(),
        term: term.clone(),
    };
    let ok_rets = lower_from_non_phi(
        ctx,
        &mut ok_nb,
        &synthetic_bb,
        stop_bb,
        &mut sub_env,
        &mut sub_mem,
        &sub_glob,
        writable_params,
    )?;
    ok_nb.ret_exprs = ok_rets;

    nb.effects.extend(ok_nb.effects.iter().cloned());
    ctx.emitted_nodes.push(err_nb);
    ctx.emitted_nodes.push(ok_nb);

    let arg_list = branch_args.join(", ");
    let call_expr = if arg_list.is_empty() {
        format!("ASSERT_GUARD({cond_port}, @{ok_name}, else=@{err_name})")
    } else {
        format!("ASSERT_GUARD({cond_port}, @{ok_name}, {arg_list}, else=@{err_name})")
    };
    Ok(nb.emit_multi(1, call_expr))
}

fn emit_ub_guard_cond(
    ctx: &LiftCtx<'_>,
    nb: &mut NodeBuilder,
    bb_label: &str,
    inst: &Inst,
    env: &BTreeMap<String, VgVal>,
) -> Result<Option<(String, u32)>, Diagnostic> {
    match &inst.kind {
        InstKind::BinOp {
            op: op @ (BinOpKind::Add | BinOpKind::Sub | BinOpKind::Mul),
            nsw: true,
            ty,
            lhs,
            rhs,
            ..
        } => {
            let bits = ty.bit_width().unwrap_or(64);
            if is_nsw_proven_safe(ctx.func, bb_label, *op, bits, lhs, rhs, ctx.intervals) {
                return Ok(None);
            }
            let lval = resolve_value(ctx, nb, lhs, ty, env, &BTreeMap::new())?;
            let rval = resolve_value(ctx, nb, rhs, ty, env, &BTreeMap::new())?;
            let (lu, _) = coerce_to_unsigned(nb, &lval);
            let (ru, _) = coerce_to_unsigned(nb, &rval);
            let uty = format!("u{bits}");
            let sign_mask = 1u64 << (bits - 1);
            let cond = match op {
                BinOpKind::Add => {
                    let sum = nb.emit(format!("ADD.wrap({lu}, {ru})"));
                    let diff_signs = nb.emit(format!("XOR({lu}, {ru})"));
                    let diff_sign_bit = nb.emit(format!("AND({diff_signs}, {sign_mask}:{uty})"));
                    let has_diff_signs = nb.emit(format!("NE({diff_sign_bit}, 0:{uty})"));
                    let sum_xor_l = nb.emit(format!("XOR({sum}, {lu})"));
                    let ovf_bit = nb.emit(format!("AND({sum_xor_l}, {sign_mask}:{uty})"));
                    let same_sign_ok = nb.emit(format!("EQ({ovf_bit}, 0:{uty})"));
                    nb.emit(format!("BOR({has_diff_signs}, {same_sign_ok})"))
                }
                BinOpKind::Sub => {
                    let diff = nb.emit(format!("SUB.wrap({lu}, {ru})"));
                    let same_signs = nb.emit(format!("XOR({lu}, {ru})"));
                    let same_sign_bit = nb.emit(format!("AND({same_signs}, {sign_mask}:{uty})"));
                    let has_same_signs = nb.emit(format!("EQ({same_sign_bit}, 0:{uty})"));
                    let diff_xor_l = nb.emit(format!("XOR({diff}, {lu})"));
                    let ovf_bit = nb.emit(format!("AND({diff_xor_l}, {sign_mask}:{uty})"));
                    let diff_sign_ok = nb.emit(format!("EQ({ovf_bit}, 0:{uty})"));
                    nb.emit(format!("BOR({has_same_signs}, {diff_sign_ok})"))
                }
                BinOpKind::Mul => {
                    if bits <= 32 {
                        let ls = emit_sign_extend(nb, &lu, bits, 64);
                        let rs = emit_sign_extend(nb, &ru, bits, 64);
                        let prod = nb.emit(format!("MUL.wrap({ls}, {rs})"));
                        let lo_mask = (1u64 << bits) - 1;
                        let lo = nb.emit(format!("AND({prod}, {lo_mask}:u64)"));
                        let trunc = nb.emit(format!("CAST.{uty}({lo})"));
                        let reext = emit_sign_extend(nb, &trunc, bits, 64);
                        nb.emit(format!("EQ({prod}, {reext})"))
                    } else {
                        // Signed 64-bit mul overflow guard: exact check using high-level sign/bounds or zero.
                        let is_zero = nb.emit(format!("EQ({ru}, 0:u64)"));
                        let not_min_neg1 = {
                            let is_min = nb.emit(format!("EQ({lu}, {}:u64)", i64::MIN as u64));
                            let is_neg1 = nb.emit(format!("EQ({ru}, {}:u64)", (-1i64) as u64));
                            let both = nb.emit(format!("BAND({is_min}, {is_neg1})"));
                            nb.emit(format!("BNOT({both})"))
                        };
                        let prod = nb.emit(format!("MUL.wrap({lu}, {ru})"));
                        let back = emit_signed_div(nb, &prod, &ru, 64);
                        let eq = nb.emit(format!("EQ({back}, {lu})"));
                        let nz_ok = nb.emit(format!("BAND({not_min_neg1}, {eq})"));
                        nb.emit(format!("BOR({is_zero}, {nz_ok})"))
                    }
                }
                _ => unreachable!(),
            };
            Ok(Some((cond, 1)))
        }
        InstKind::BinOp {
            op: op @ (BinOpKind::UDiv | BinOpKind::URem | BinOpKind::SDiv | BinOpKind::SRem),
            ty,
            lhs,
            rhs,
            ..
        } => {
            let bits = ty.bit_width().unwrap_or(64);
            let rval = resolve_value(ctx, nb, rhs, ty, env, &BTreeMap::new())?;
            let (ru, _) = coerce_to_unsigned(nb, &rval);
            let uty = format!("u{bits}");
            let safe_nonzero = match rhs {
                ValueRef::ConstInt { value, .. } => *value != 0,
                ValueRef::Local(n) => ctx.intervals.get(n).is_some_and(|iv| iv.umin > 0),
                _ => false,
            };
            let is_signed = matches!(op, BinOpKind::SDiv | BinOpKind::SRem);
            let need_min_neg1 =
                is_signed && !matches!(rhs, ValueRef::ConstInt { value, .. } if *value != -1);
            if safe_nonzero && !need_min_neg1 {
                return Ok(None);
            }
            let mut cond = nb.emit(format!("NE({ru}, 0:{uty})"));
            if need_min_neg1 {
                let lval = resolve_value(ctx, nb, lhs, ty, env, &BTreeMap::new())?;
                let (lu, _) = coerce_to_unsigned(nb, &lval);
                let min_u = 1u64 << (bits - 1);
                let neg1_u = if bits >= 64 {
                    u64::MAX
                } else {
                    (1u64 << bits) - 1
                };
                let is_min = nb.emit(format!("EQ({lu}, {min_u}:{uty})"));
                let is_neg1 = nb.emit(format!("EQ({ru}, {neg1_u}:{uty})"));
                let both = nb.emit(format!("BAND({is_min}, {is_neg1})"));
                let ok2 = nb.emit(format!("BNOT({both})"));
                cond = nb.emit(format!("BAND({cond}, {ok2})"));
            }
            Ok(Some((cond, 2)))
        }
        InstKind::BinOp {
            op: BinOpKind::Shl | BinOpKind::LShr | BinOpKind::AShr,
            ty,
            rhs,
            ..
        } => {
            let bits = ty.bit_width().unwrap_or(64);
            let safe_shift = match rhs {
                ValueRef::ConstInt { value, .. } => {
                    *value >= 0 && (*value as u64) < u64::from(bits)
                }
                ValueRef::Local(n) => ctx
                    .intervals
                    .get(n)
                    .is_some_and(|iv| iv.umax < u64::from(bits)),
                _ => false,
            };
            if safe_shift {
                return Ok(None);
            }
            let rval = resolve_value(ctx, nb, rhs, ty, env, &BTreeMap::new())?;
            let (ru, _) = coerce_to_unsigned(nb, &rval);
            let uty = format!("u{bits}");
            let cond = nb.emit(format!("LT.u({ru}, {bits}:{uty})"));
            Ok(Some((cond, 3)))
        }
        InstKind::Load { ty, ptr, .. } => {
            if let Some(VgVal::Ptr {
                base: PtrBase::Param(pi),
                off_port,
            }) = resolve_ptr_val(ptr, env)
                && matches!(
                    ctx.param_kinds.get(pi),
                    Some(ParamKind::Slice {
                        len_param_idx: None
                    })
                )
            {
                let slice_name = &ctx.param_to_vg_name[&pi];
                let len_p = nb.emit(format!("LEN({slice_name})"));
                let access_bytes = u64::from(ty.bit_width().unwrap_or(8).div_ceil(8));
                let cond = if access_bytes == 1 {
                    nb.emit(format!("LT.u({off_port}, {len_p})"))
                } else {
                    let end = nb.emit(format!("ADD.wrap({off_port}, {access_bytes}:u64)"));
                    let ok1 = nb.emit(format!("LE.u({end}, {len_p})"));
                    let ok2 = nb.emit(format!("GE.u({end}, {off_port})"));
                    nb.emit(format!("BAND({ok1}, {ok2})"))
                };
                return Ok(Some((cond, 4)));
            }
            Ok(None)
        }
        _ => Ok(None),
    }
}

#[derive(Clone)]
struct BranchParam {
    key: BranchKey,
    param_name: String,
    bits: u32,
}

#[derive(Clone, PartialEq, Eq)]
enum BranchKey {
    SsaScalar(String),
    SsaPtr { name: String, base: PtrBase },
    Mem(MemSlot),
    Global(String),
}

fn collect_live_inputs(
    ctx: &LiftCtx<'_>,
    env: &BTreeMap<String, VgVal>,
    mem_ports: &BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> (Vec<(BranchParam, String, String)>, Vec<String>) {
    let mut params = Vec::new();
    let mut args = Vec::new();
    let mut used = BTreeSet::new();

    for (name, val) in env {
        match val {
            VgVal::Scalar { port, ty, bits } => {
                let pname = sanitize_ident(name, params.len(), &mut used);
                params.push((
                    BranchParam {
                        key: BranchKey::SsaScalar(name.clone()),
                        param_name: pname.clone(),
                        bits: *bits,
                    },
                    pname,
                    ty.clone(),
                ));
                args.push(port.clone());
            }
            VgVal::Ptr { base, off_port } => {
                if let PtrBase::Param(pi) = base {
                    match &ctx.param_kinds[*pi] {
                        ParamKind::Slice { .. }
                            if !params.iter().any(|(bp, _, _): &(BranchParam, _, _)| {
                                bp.param_name == ctx.param_to_vg_name[pi]
                            }) =>
                        {
                            let sname = ctx.param_to_vg_name[pi].clone();
                            if used.insert(sname.clone()) {
                                params.push((
                                    BranchParam {
                                        key: BranchKey::SsaScalar(format!("__slice_{pi}")),
                                        param_name: sname.clone(),
                                        bits: 64,
                                    },
                                    sname.clone(),
                                    "Slice<u8>".to_string(),
                                ));
                                args.push(sname);
                            }
                        }
                        ParamKind::FixedArray {
                            elem_bits,
                            len,
                            writable: false,
                        } if !params.iter().any(|(bp, _, _): &(BranchParam, _, _)| {
                            bp.param_name == ctx.param_to_vg_name[pi]
                        }) =>
                        {
                            let aname = ctx.param_to_vg_name[pi].clone();
                            if used.insert(aname.clone()) {
                                let aty = format!("Array<u{elem_bits}, {len}>");
                                params.push((
                                    BranchParam {
                                        key: BranchKey::SsaScalar(format!("__ro_arr_{pi}")),
                                        param_name: aname.clone(),
                                        bits: *elem_bits,
                                    },
                                    aname.clone(),
                                    aty,
                                ));
                                args.push(aname);
                            }
                        }
                        _ => {}
                    }
                }
                let p_off = sanitize_ident(&format!("off_{name}"), params.len(), &mut used);
                params.push((
                    BranchParam {
                        key: BranchKey::SsaPtr {
                            name: name.clone(),
                            base: base.clone(),
                        },
                        param_name: p_off.clone(),
                        bits: 64,
                    },
                    p_off,
                    "u64".to_string(),
                ));
                args.push(off_port.clone());
            }
        }
    }

    for (slot, port) in mem_ports {
        let spec = &ctx.mem_specs[slot];
        let base_name = match slot {
            MemSlot::Alloca(id) => format!("mem_a{id}"),
            MemSlot::WritableParam(pi) => ctx.param_to_vg_name[pi].clone(),
        };
        let pname = sanitize_ident(&base_name, params.len(), &mut used);
        params.push((
            BranchParam {
                key: BranchKey::Mem(slot.clone()),
                param_name: pname.clone(),
                bits: spec.elem_bits,
            },
            pname,
            spec.vg_type.clone(),
        ));
        args.push(port.clone());
    }

    for (gname, (port, gty)) in global_ports {
        let pname = sanitize_ident(&format!("g_{gname}"), params.len(), &mut used);
        params.push((
            BranchParam {
                key: BranchKey::Global(gname.clone()),
                param_name: pname.clone(),
                bits: 32,
            },
            pname,
            gty.clone(),
        ));
        args.push(port.clone());
    }

    (params, args)
}

fn bind_branch_params(
    params: &[(BranchParam, String, String)],
    _orig_env: &BTreeMap<String, VgVal>,
    _orig_mem: &BTreeMap<MemSlot, String>,
    _orig_glob: &BTreeMap<String, (String, String)>,
    sub_env: &mut BTreeMap<String, VgVal>,
    sub_mem: &mut BTreeMap<MemSlot, String>,
    sub_glob: &mut BTreeMap<String, (String, String)>,
) {
    for (bp, pname, ty) in params {
        match &bp.key {
            BranchKey::SsaScalar(name) => {
                if !name.starts_with("__slice_") && !name.starts_with("__ro_arr_") {
                    sub_env.insert(
                        name.clone(),
                        VgVal::Scalar {
                            port: pname.clone(),
                            ty: ty.clone(),
                            bits: bp.bits,
                        },
                    );
                }
            }
            BranchKey::SsaPtr { name, base } => {
                sub_env.insert(
                    name.clone(),
                    VgVal::Ptr {
                        base: base.clone(),
                        off_port: pname.clone(),
                    },
                );
            }
            BranchKey::Mem(slot) => {
                sub_mem.insert(slot.clone(), pname.clone());
            }
            BranchKey::Global(gname) => {
                sub_glob.insert(gname.clone(), (pname.clone(), ty.clone()));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_merge_branch_node(
    ctx: &mut LiftCtx<'_>,
    name: &str,
    from_bb: &str,
    entry_bb: &str,
    merge_lbl: &str,
    branch_params: &[(BranchParam, String, String)],
    out_tys: &[String],
    merge_phis: &[(String, LlvmType)],
    extra_live: &[(String, String, u32)],
    mem_keys: &[MemSlot],
    dummy_out: bool,
    orig_env: &BTreeMap<String, VgVal>,
    orig_mem: &BTreeMap<MemSlot, String>,
    orig_glob: &BTreeMap<String, (String, String)>,
    writable_params: &[MemSlot],
) -> Result<NodeBuilder, Diagnostic> {
    let mut bnb = NodeBuilder::new(name.to_string());
    bnb.params = branch_params
        .iter()
        .map(|(_, pn, ty)| (pn.clone(), ty.clone()))
        .collect();
    bnb.results = out_tys.to_vec();

    let mut sub_env = BTreeMap::new();
    let mut sub_mem = BTreeMap::new();
    let mut sub_glob = BTreeMap::new();
    bind_branch_params(
        branch_params,
        orig_env,
        orig_mem,
        orig_glob,
        &mut sub_env,
        &mut sub_mem,
        &mut sub_glob,
    );

    if entry_bb != merge_lbl {
        lower_region_with_prev(
            ctx,
            &mut bnb,
            entry_bb,
            Some(from_bb),
            Some(merge_lbl),
            &mut sub_env,
            &mut sub_mem,
            &sub_glob,
            writable_params,
        )?;
    }

    // Find predecessor of `merge_lbl` from this arm:
    let pred_at_merge = if entry_bb == merge_lbl {
        from_bb.to_string()
    } else {
        find_predecessor_reaching_merge(ctx, entry_bb, merge_lbl)
            .unwrap_or_else(|| entry_bb.to_string())
    };

    let merge_bb = ctx.bb_map[merge_lbl];
    let mut rets = Vec::new();
    for (pname, pty) in merge_phis {
        let val = if let Some(already_merged) = sub_env.get(pname)
            && !orig_env.contains_key(pname)
        {
            already_merged.clone()
        } else {
            let phi_inst = merge_bb
                .insts
                .iter()
                .find(|inst| inst.result.as_deref() == Some(pname.as_str()))
                .unwrap();
            let InstKind::Phi { incoming, .. } = &phi_inst.kind else {
                unreachable!()
            };
            let chosen = incoming
                .iter()
                .find(|(_, lbl)| *lbl == pred_at_merge)
                .map(|(v, _)| v)
                .ok_or_else(|| {
                    err(format!(
                        "phi %{pname} missing incoming edge from %{pred_at_merge}"
                    ))
                })?;
            resolve_value(ctx, &mut bnb, chosen, pty, &sub_env, &sub_glob)?
        };
        let port = match pty {
            LlvmType::Ptr => coerce_to_unsigned(&mut bnb, &val).0,
            LlvmType::Int(b) => {
                let target_ty = if *b == 1 {
                    "Bool".to_string()
                } else {
                    format!("u{b}")
                };
                coerce_to_type(&mut bnb, &val, &target_ty, *b)
            }
            _ => return Err(err(format!("unsupported merge phi type {pty:?}"))),
        };
        rets.push(port);
    }
    for (ename, tstr, bits) in extra_live {
        if let Some(v) = sub_env.get(ename) {
            rets.push(coerce_to_type(&mut bnb, v, tstr, *bits));
        } else {
            rets.push(emit_scalar_const(0, *bits, tstr));
        }
    }
    for slot in mem_keys {
        rets.push(sub_mem[slot].clone());
    }
    if dummy_out {
        rets.push("true".to_string());
    }
    bnb.ret_exprs = rets;
    Ok(bnb)
}

#[allow(clippy::too_many_arguments)]
fn build_tail_branch_node(
    ctx: &mut LiftCtx<'_>,
    name: &str,
    from_bb: &str,
    entry_bb: &str,
    stop_bb: Option<&str>,
    branch_params: &[(BranchParam, String, String)],
    res_tys: &[String],
    orig_env: &BTreeMap<String, VgVal>,
    orig_mem: &BTreeMap<MemSlot, String>,
    orig_glob: &BTreeMap<String, (String, String)>,
    writable_params: &[MemSlot],
) -> Result<NodeBuilder, Diagnostic> {
    let mut bnb = NodeBuilder::new(name.to_string());
    bnb.params = branch_params
        .iter()
        .map(|(_, pn, ty)| (pn.clone(), ty.clone()))
        .collect();
    bnb.results = res_tys.to_vec();

    let mut sub_env = BTreeMap::new();
    let mut sub_mem = BTreeMap::new();
    let mut sub_glob = BTreeMap::new();
    bind_branch_params(
        branch_params,
        orig_env,
        orig_mem,
        orig_glob,
        &mut sub_env,
        &mut sub_mem,
        &mut sub_glob,
    );

    let rets = lower_region_with_prev(
        ctx,
        &mut bnb,
        entry_bb,
        Some(from_bb),
        stop_bb,
        &mut sub_env,
        &mut sub_mem,
        &sub_glob,
        writable_params,
    )?;
    bnb.ret_exprs = rets;
    Ok(bnb)
}

#[allow(clippy::too_many_arguments)]
fn lower_region_with_prev(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    start_bb: &str,
    init_prev: Option<&str>,
    stop_bb: Option<&str>,
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
    writable_params: &[MemSlot],
) -> Result<Vec<String>, Diagnostic> {
    if let Some(p) = init_prev {
        let bb = ctx.bb_map[start_bb].clone();
        for inst in &bb.insts {
            if let InstKind::Phi { ty, incoming } = &inst.kind {
                let res = inst.result.as_ref().unwrap();
                let chosen = incoming
                    .iter()
                    .find(|(_, lbl)| lbl == p)
                    .or_else(|| incoming.first())
                    .map(|(v, _)| v)
                    .unwrap();
                let val = resolve_value(ctx, nb, chosen, ty, env, global_ports)?;
                env.insert(res.clone(), val);
            }
        }
        return lower_from_non_phi(
            ctx,
            nb,
            &bb,
            stop_bb,
            env,
            mem_ports,
            global_ports,
            writable_params,
        );
    }
    lower_region(
        ctx,
        nb,
        start_bb,
        stop_bb,
        env,
        mem_ports,
        global_ports,
        writable_params,
    )
}

fn find_predecessor_reaching_merge(
    ctx: &LiftCtx<'_>,
    start_bb: &str,
    merge_lbl: &str,
) -> Option<String> {
    let mut visited = BTreeSet::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(start_bb.to_string());
    while let Some(cur) = queue.pop_front() {
        if !visited.insert(cur.clone()) {
            continue;
        }
        if let Some(lp) = ctx.loops.iter().rev().find(|l| l.header == cur) {
            if lp.exit_block == merge_lbl {
                return Some(lp.latch.clone());
            }
            queue.push_back(lp.exit_block.clone());
            continue;
        }
        let bb = ctx.bb_map[cur.as_str()];
        for s in term_succs(&bb.term) {
            if s == merge_lbl {
                return Some(cur);
            }
            queue.push_back(s);
        }
    }
    None
}

fn collect_defs_between(
    ctx: &LiftCtx<'_>,
    then_bb: &str,
    else_bb: &str,
    merge_lbl: &str,
) -> BTreeMap<String, u32> {
    let mut defs = BTreeMap::new();
    let mut visited = BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([then_bb.to_string(), else_bb.to_string()]);
    while let Some(cur) = queue.pop_front() {
        if cur == merge_lbl || !visited.insert(cur.clone()) {
            continue;
        }
        let bb = ctx.bb_map[cur.as_str()];
        for inst in &bb.insts {
            if let Some(res) = &inst.result {
                let bits = match &inst.kind {
                    InstKind::BinOp { ty, .. }
                    | InstKind::Load { ty, .. }
                    | InstKind::Select { ty, .. }
                    | InstKind::Phi { ty, .. } => ty.bit_width().unwrap_or(64),
                    InstKind::Cast { to_ty, .. } => to_ty.bit_width().unwrap_or(64),
                    InstKind::ICmp { .. } => 1,
                    InstKind::Call { ret_ty, .. } => ret_ty.bit_width().unwrap_or(64),
                    _ => 64,
                };
                defs.insert(res.clone(), bits);
            }
        }
        for s in term_succs(&bb.term) {
            queue.push_back(s);
        }
    }
    defs
}

fn collect_uses_from(
    ctx: &LiftCtx<'_>,
    start_lbl: &str,
    stop_bb: Option<&str>,
) -> BTreeSet<String> {
    let mut uses = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut queue = std::collections::VecDeque::from([start_lbl.to_string()]);
    while let Some(cur) = queue.pop_front() {
        if stop_bb == Some(cur.as_str()) || !visited.insert(cur.clone()) {
            continue;
        }
        let bb = ctx.bb_map[cur.as_str()];
        for inst in &bb.insts {
            collect_inst_uses(inst, &mut uses);
        }
        match &bb.term {
            Terminator::CondBr {
                cond: ValueRef::Local(n),
                ..
            }
            | Terminator::Ret(Some((_, ValueRef::Local(n)))) => {
                uses.insert(n.clone());
            }
            _ => {}
        }
        for s in term_succs(&bb.term) {
            queue.push_back(s);
        }
    }
    uses
}

fn collect_inst_uses(inst: &Inst, out: &mut BTreeSet<String>) {
    let mut add = |v: &ValueRef| {
        if let ValueRef::Local(n) = v {
            out.insert(n.clone());
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

fn resolve_phi_ptr_base(
    bb: &BasicBlock,
    phi_name: &str,
    env: &BTreeMap<String, VgVal>,
) -> Result<PtrBase, Diagnostic> {
    let phi_inst = bb
        .insts
        .iter()
        .find(|i| i.result.as_deref() == Some(phi_name))
        .ok_or_else(|| err(format!("missing phi %{phi_name}")))?;
    let InstKind::Phi { incoming, .. } = &phi_inst.kind else {
        return Err(err(format!("expected phi %{phi_name}")));
    };
    for (v, _) in incoming {
        if let ValueRef::Local(n) = v
            && let Some(VgVal::Ptr { base, .. }) = env.get(n)
        {
            return Ok(base.clone());
        }
    }
    Err(err(format!(
        "cannot resolve pointer base for phi %{phi_name}"
    )))
}

fn resolve_ptr_val(v: &ValueRef, env: &BTreeMap<String, VgVal>) -> Option<VgVal> {
    match v {
        ValueRef::Local(n) => env.get(n).cloned(),
        ValueRef::Global(g) => Some(VgVal::Ptr {
            base: PtrBase::Global(g.clone()),
            off_port: "0:u64".to_string(),
        }),
        _ => None,
    }
}

fn resolve_value(
    _ctx: &LiftCtx<'_>,
    nb: &mut NodeBuilder,
    v: &ValueRef,
    ty: &LlvmType,
    env: &BTreeMap<String, VgVal>,
    _global_ports: &BTreeMap<String, (String, String)>,
) -> Result<VgVal, Diagnostic> {
    match v {
        ValueRef::Local(n) => env
            .get(n)
            .cloned()
            .ok_or_else(|| err(format!("use of undefined SSA value %{n}"))),
        ValueRef::Global(g) => Ok(VgVal::Ptr {
            base: PtrBase::Global(g.clone()),
            off_port: "0:u64".to_string(),
        }),
        ValueRef::ConstInt { bits, value } => {
            let b = ty.bit_width().unwrap_or(*bits);
            let vg_ty = if b == 1 {
                "Bool".to_string()
            } else {
                format!("u{b}")
            };
            Ok(VgVal::Scalar {
                port: emit_scalar_const(*value, b, &vg_ty),
                ty: vg_ty,
                bits: b,
            })
        }
        ValueRef::ConstBool(b) => Ok(VgVal::Scalar {
            port: if *b {
                "true".to_string()
            } else {
                "false".to_string()
            },
            ty: "Bool".to_string(),
            bits: 1,
        }),
        ValueRef::Undef => {
            let b = ty.bit_width().unwrap_or(64);
            let vg_ty = if b == 1 {
                "Bool".to_string()
            } else {
                format!("u{b}")
            };
            let _ = nb;
            Ok(VgVal::Scalar {
                port: emit_scalar_const(0, b, &vg_ty),
                ty: vg_ty,
                bits: b,
            })
        }
        ValueRef::ConstNull => Ok(VgVal::Ptr {
            base: PtrBase::Null,
            off_port: "0:u64".to_string(),
        }),
    }
}

fn lower_instruction(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    inst: &Inst,
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> Result<(), Diagnostic> {
    match &inst.kind {
        InstKind::Phi { .. } | InstKind::Alloca { .. } => {}
        InstKind::BinOp {
            op, ty, lhs, rhs, ..
        } => {
            let bits = ty
                .bit_width()
                .ok_or_else(|| err(format!("unsupported binop type {ty:?}")))?;
            let res_name = inst.result.as_ref().unwrap().clone();
            let lv = resolve_value(ctx, nb, lhs, ty, env, global_ports)?;
            let rv = resolve_value(ctx, nb, rhs, ty, env, global_ports)?;
            let (lu, _) = coerce_to_unsigned(nb, &lv);
            let (ru, _) = coerce_to_unsigned(nb, &rv);
            if bits == 1 {
                let lp = match &lv {
                    VgVal::Scalar { port, .. } => port.clone(),
                    _ => return Err(err("i1 binop on ptr")),
                };
                let rp = match &rv {
                    VgVal::Scalar { port, .. } => port.clone(),
                    _ => return Err(err("i1 binop on ptr")),
                };
                let port = match op {
                    BinOpKind::And => nb.emit(format!("BAND({lp}, {rp})")),
                    BinOpKind::Or => nb.emit(format!("BOR({lp}, {rp})")),
                    BinOpKind::Xor => nb.emit(format!("NE({lp}, {rp})")),
                    _ => return Err(err(format!("unsupported i1 binop {op:?}"))),
                };
                env.insert(
                    res_name,
                    VgVal::Scalar {
                        port,
                        ty: "Bool".to_string(),
                        bits: 1,
                    },
                );
                return Ok(());
            }

            let uty = format!("u{bits}");
            let port = match op {
                BinOpKind::Add => nb.emit(format!("ADD.wrap({lu}, {ru})")),
                BinOpKind::Sub => nb.emit(format!("SUB.wrap({lu}, {ru})")),
                BinOpKind::Mul => nb.emit(format!("MUL.wrap({lu}, {ru})")),
                BinOpKind::UDiv => nb.emit(format!("DIV.wrap({lu}, {ru})")),
                BinOpKind::URem => nb.emit(format!("MOD.wrap({lu}, {ru})")),
                BinOpKind::SDiv => emit_signed_div(nb, &lu, &ru, bits),
                BinOpKind::SRem => emit_signed_rem(nb, &lu, &ru, bits),
                BinOpKind::And => nb.emit(format!("AND({lu}, {ru})")),
                BinOpKind::Or => nb.emit(format!("OR({lu}, {ru})")),
                BinOpKind::Xor => nb.emit(format!("XOR({lu}, {ru})")),
                BinOpKind::Shl => {
                    let sh = cast_to_unsigned(nb, &ru, bits, 32);
                    nb.emit(format!("SHL.wrap({lu}, {sh})"))
                }
                BinOpKind::LShr => {
                    let sh = cast_to_unsigned(nb, &ru, bits, 32);
                    nb.emit(format!("SHR.wrap({lu}, {sh})"))
                }
                BinOpKind::AShr => emit_ashr(nb, &lu, &ru, bits),
            };
            env.insert(
                res_name,
                VgVal::Scalar {
                    port,
                    ty: uty,
                    bits,
                },
            );
        }
        InstKind::Cast {
            op,
            from_ty,
            val,
            to_ty,
            ..
        } => {
            let fbits = from_ty
                .bit_width()
                .ok_or_else(|| err(format!("unsupported cast source {from_ty:?}")))?;
            let tbits = to_ty
                .bit_width()
                .ok_or_else(|| err(format!("unsupported cast target {to_ty:?}")))?;
            let res_name = inst.result.as_ref().unwrap().clone();
            let v = resolve_value(ctx, nb, val, from_ty, env, global_ports)?;
            let port = match op {
                CastKind::ZExt => {
                    if fbits == 1 {
                        let VgVal::Scalar { port: bp, .. } = v else {
                            return Err(err("zext non-bool"));
                        };
                        nb.emit(format!("CAST.u{tbits}({bp})"))
                    } else {
                        let (u, _) = coerce_to_unsigned(nb, &v);
                        nb.emit(format!("CAST.u{tbits}({u})"))
                    }
                }
                CastKind::Trunc => {
                    let (u, _) = coerce_to_unsigned(nb, &v);
                    if tbits == 1 {
                        let bit0 = nb.emit(format!("AND({u}, 1:u{fbits})"));
                        nb.emit(format!("NE({bit0}, 0:u{fbits})"))
                    } else {
                        cast_to_unsigned(nb, &u, fbits, tbits)
                    }
                }
                CastKind::SExt => {
                    let (u, _) = coerce_to_unsigned(nb, &v);
                    emit_sign_extend(nb, &u, fbits, tbits)
                }
            };
            let vg_ty = if tbits == 1 {
                "Bool".to_string()
            } else {
                format!("u{tbits}")
            };
            env.insert(
                res_name,
                VgVal::Scalar {
                    port,
                    ty: vg_ty,
                    bits: tbits,
                },
            );
        }
        InstKind::ICmp {
            pred, ty, lhs, rhs, ..
        } => {
            let res_name = inst.result.as_ref().unwrap().clone();
            let port = if *ty == LlvmType::Ptr {
                let lv = resolve_value(ctx, nb, lhs, ty, env, global_ports)?;
                let rv = resolve_value(ctx, nb, rhs, ty, env, global_ports)?;
                match (lv, rv) {
                    (
                        VgVal::Ptr {
                            base: b1,
                            off_port: o1,
                        },
                        VgVal::Ptr {
                            base: b2,
                            off_port: o2,
                        },
                    ) => {
                        if b1 == b2 {
                            emit_icmp_u(nb, *pred, &o1, &o2, 64)
                        } else {
                            match pred {
                                ICmpPred::Eq => "false".to_string(),
                                ICmpPred::Ne => "true".to_string(),
                                _ => return Err(err("ordered icmp across distinct pointers")),
                            }
                        }
                    }
                    _ => return Err(err("ptr icmp on non-pointer")),
                }
            } else {
                let bits = ty.bit_width().unwrap_or(64);
                let lv = resolve_value(ctx, nb, lhs, ty, env, global_ports)?;
                let rv = resolve_value(ctx, nb, rhs, ty, env, global_ports)?;
                if bits == 1 {
                    let lp = coerce_to_type(nb, &lv, "Bool", 1);
                    let rp = coerce_to_type(nb, &rv, "Bool", 1);
                    match pred {
                        ICmpPred::Eq => nb.emit(format!("EQ({lp}, {rp})")),
                        ICmpPred::Ne => nb.emit(format!("NE({lp}, {rp})")),
                        _ => return Err(err(format!("unsupported i1 icmp {pred:?}"))),
                    }
                } else {
                    let (lu, _) = coerce_to_unsigned(nb, &lv);
                    let (ru, _) = coerce_to_unsigned(nb, &rv);
                    emit_icmp_u(nb, *pred, &lu, &ru, bits)
                }
            };
            env.insert(
                res_name,
                VgVal::Scalar {
                    port,
                    ty: "Bool".to_string(),
                    bits: 1,
                },
            );
        }
        InstKind::Select {
            cond,
            ty,
            then_val,
            else_val,
        } => {
            let res_name = inst.result.as_ref().unwrap().clone();
            let cv = resolve_value(ctx, nb, cond, &LlvmType::Int(1), env, global_ports)?;
            let cp = coerce_to_type(nb, &cv, "Bool", 1);
            let tv = resolve_value(ctx, nb, then_val, ty, env, global_ports)?;
            let ev = resolve_value(ctx, nb, else_val, ty, env, global_ports)?;
            match (tv, ev) {
                (
                    VgVal::Ptr {
                        base: b1,
                        off_port: o1,
                    },
                    VgVal::Ptr {
                        base: b2,
                        off_port: o2,
                    },
                ) => {
                    if b1 != b2 {
                        return Err(err("select across distinct pointer bases"));
                    }
                    let port = nb.emit(format!("SELECT({cp}, {o1}, {o2})"));
                    env.insert(
                        res_name,
                        VgVal::Ptr {
                            base: b1,
                            off_port: port,
                        },
                    );
                }
                (
                    VgVal::Scalar {
                        port: p1,
                        ty: t1,
                        bits: b1,
                    },
                    VgVal::Scalar { port: p2, .. },
                ) => {
                    let port = nb.emit(format!("SELECT({cp}, {p1}, {p2})"));
                    env.insert(
                        res_name,
                        VgVal::Scalar {
                            port,
                            ty: t1,
                            bits: b1,
                        },
                    );
                }
                _ => return Err(err("mismatched select arms")),
            }
        }
        InstKind::Gep {
            base_ty,
            ptr,
            indices,
            ..
        } => {
            let res_name = inst.result.as_ref().unwrap().clone();
            let pv = resolve_value(ctx, nb, ptr, &LlvmType::Ptr, env, global_ports)?;
            let VgVal::Ptr { base, mut off_port } = pv else {
                return Err(err("GEP on non-pointer"));
            };
            let delta = emit_gep_offset_expr(ctx, nb, base_ty, indices, env, global_ports)?;
            if delta != "0:u64" {
                off_port = if off_port == "0:u64" {
                    delta
                } else {
                    nb.emit(format!("ADD.wrap({off_port}, {delta})"))
                };
            }
            env.insert(res_name, VgVal::Ptr { base, off_port });
        }
        InstKind::Load { ty, ptr, .. } => {
            let res_name = inst.result.as_ref().unwrap().clone();
            let bits = ty.bit_width().unwrap_or(8);
            let pv = resolve_value(ctx, nb, ptr, &LlvmType::Ptr, env, global_ports)?;
            let VgVal::Ptr { base, off_port } = pv else {
                return Err(err("load from non-pointer"));
            };
            let loaded_port =
                emit_load_from_base(ctx, nb, &base, &off_port, bits, mem_ports, global_ports)?;
            env.insert(
                res_name,
                VgVal::Scalar {
                    port: loaded_port,
                    ty: format!("u{bits}"),
                    bits,
                },
            );
        }
        InstKind::Store { ty, val, ptr, .. } => {
            let bits = ty.bit_width().unwrap_or(8);
            let v = resolve_value(ctx, nb, val, ty, env, global_ports)?;
            let (uport, _) = coerce_to_unsigned(nb, &v);
            let pv = resolve_value(ctx, nb, ptr, &LlvmType::Ptr, env, global_ports)?;
            let VgVal::Ptr { base, off_port } = pv else {
                return Err(err("store to non-pointer"));
            };
            emit_store_to_base(ctx, nb, &base, &off_port, bits, &uport, mem_ports)?;
        }
        InstKind::Call {
            callee,
            args,
            ret_ty,
            ..
        } => {
            if let Some(res_name) = &inst.result {
                if callee.starts_with("llvm.fshl.") {
                    let bits = ret_ty.bit_width().unwrap_or(32);
                    let v0 = resolve_value(ctx, nb, &args[0].1, &args[0].0, env, global_ports)?;
                    let v1 = resolve_value(ctx, nb, &args[1].1, &args[1].0, env, global_ports)?;
                    let s = resolve_value(ctx, nb, &args[2].1, &args[2].0, env, global_ports)?;
                    let (p0, _) = coerce_to_unsigned(nb, &v0);
                    let (p1, _) = coerce_to_unsigned(nb, &v1);
                    let (ps, sbits) = coerce_to_unsigned(nb, &s);
                    let sh32 = cast_to_unsigned(nb, &ps, sbits, 32);
                    let port = if p0 == p1 {
                        nb.emit(format!("ROTL({p0}, {sh32})"))
                    } else {
                        // General funnel shift left: (p0 << (s%W)) | (p1 >> (W - s%W)).
                        let sh_m = nb.emit(format!("MOD.wrap({sh32}, {bits}:u32)"));
                        let left = nb.emit(format!("SHL.wrap({p0}, {sh_m})"));
                        let rem = nb.emit(format!("SUB.wrap({bits}:u32, {sh_m})"));
                        let right = nb.emit(format!("SHR.wrap({p1}, {rem})"));
                        let comb = nb.emit(format!("OR({left}, {right})"));
                        let is0 = nb.emit(format!("EQ({sh_m}, 0:u32)"));
                        nb.emit(format!("SELECT({is0}, {p0}, {comb})"))
                    };
                    env.insert(
                        res_name.clone(),
                        VgVal::Scalar {
                            port,
                            ty: format!("u{bits}"),
                            bits,
                        },
                    );
                } else if callee.starts_with("llvm.fshr.") {
                    let bits = ret_ty.bit_width().unwrap_or(32);
                    let v0 = resolve_value(ctx, nb, &args[0].1, &args[0].0, env, global_ports)?;
                    let v1 = resolve_value(ctx, nb, &args[1].1, &args[1].0, env, global_ports)?;
                    let s = resolve_value(ctx, nb, &args[2].1, &args[2].0, env, global_ports)?;
                    let (p0, _) = coerce_to_unsigned(nb, &v0);
                    let (p1, _) = coerce_to_unsigned(nb, &v1);
                    let (ps, sbits) = coerce_to_unsigned(nb, &s);
                    let sh32 = cast_to_unsigned(nb, &ps, sbits, 32);
                    let port = if p0 == p1 {
                        nb.emit(format!("ROTR({p0}, {sh32})"))
                    } else {
                        let sh_m = nb.emit(format!("MOD.wrap({sh32}, {bits}:u32)"));
                        let right = nb.emit(format!("SHR.wrap({p1}, {sh_m})"));
                        let rem = nb.emit(format!("SUB.wrap({bits}:u32, {sh_m})"));
                        let left = nb.emit(format!("SHL.wrap({p0}, {rem})"));
                        let comb = nb.emit(format!("OR({left}, {right})"));
                        let is0 = nb.emit(format!("EQ({sh_m}, 0:u32)"));
                        nb.emit(format!("SELECT({is0}, {p1}, {comb})"))
                    };
                    env.insert(
                        res_name.clone(),
                        VgVal::Scalar {
                            port,
                            ty: format!("u{bits}"),
                            bits,
                        },
                    );
                } else if callee.starts_with("llvm.bswap.i32") {
                    let v = resolve_value(ctx, nb, &args[0].1, &args[0].0, env, global_ports)?;
                    let (p, _) = coerce_to_unsigned(nb, &v);
                    let b0 = nb.emit(format!("SHL.wrap({p}, 24:u32)"));
                    let t1 = nb.emit(format!("AND({p}, 65280:u32)"));
                    let b1 = nb.emit(format!("SHL.wrap({t1}, 8:u32)"));
                    let t2 = nb.emit(format!("SHR.wrap({p}, 8:u32)"));
                    let b2 = nb.emit(format!("AND({t2}, 65280:u32)"));
                    let b3 = nb.emit(format!("SHR.wrap({p}, 24:u32)"));
                    let o1 = nb.emit(format!("OR({b0}, {b1})"));
                    let o2 = nb.emit(format!("OR({o1}, {b2})"));
                    let port = nb.emit(format!("OR({o2}, {b3})"));
                    env.insert(
                        res_name.clone(),
                        VgVal::Scalar {
                            port,
                            ty: "u32".to_string(),
                            bits: 32,
                        },
                    );
                } else if callee.starts_with("llvm.umin.") || callee.starts_with("llvm.umax.") {
                    let bits = ret_ty.bit_width().unwrap_or(64);
                    let v0 = resolve_value(ctx, nb, &args[0].1, &args[0].0, env, global_ports)?;
                    let v1 = resolve_value(ctx, nb, &args[1].1, &args[1].0, env, global_ports)?;
                    let (p0, _) = coerce_to_unsigned(nb, &v0);
                    let (p1, _) = coerce_to_unsigned(nb, &v1);
                    let cmp = if callee.starts_with("llvm.umin.") {
                        "LT.u"
                    } else {
                        "GT.u"
                    };
                    let c = nb.emit(format!("{cmp}({p0}, {p1})"));
                    let port = nb.emit(format!("SELECT({c}, {p0}, {p1})"));
                    env.insert(
                        res_name.clone(),
                        VgVal::Scalar {
                            port,
                            ty: format!("u{bits}"),
                            bits,
                        },
                    );
                } else {
                    return Err(err(format!("unsupported intrinsic call @{callee}")));
                }
            } else if callee.starts_with("llvm.memcpy.") {
                let dst = resolve_value(ctx, nb, &args[0].1, &LlvmType::Ptr, env, global_ports)?;
                let src = resolve_value(ctx, nb, &args[1].1, &LlvmType::Ptr, env, global_ports)?;
                let ValueRef::ConstInt { value: nb_val, .. } = args[2].1 else {
                    return Err(err("dynamic-length memcpy not supported"));
                };
                let (
                    VgVal::Ptr {
                        base: db,
                        off_port: doff,
                    },
                    VgVal::Ptr {
                        base: sb,
                        off_port: soff,
                    },
                ) = (dst, src)
                else {
                    return Err(err("memcpy on non-pointer"));
                };
                let nbytes = nb_val as u64;
                if nbytes.is_multiple_of(4) {
                    for w in 0..(nbytes / 4) {
                        let byte_delta = w * 4;
                        let s_addr = if byte_delta == 0 {
                            soff.clone()
                        } else {
                            nb.emit(format!("ADD.wrap({soff}, {byte_delta}:u64)"))
                        };
                        let d_addr = if byte_delta == 0 {
                            doff.clone()
                        } else {
                            nb.emit(format!("ADD.wrap({doff}, {byte_delta}:u64)"))
                        };
                        let word = emit_load_from_base(
                            ctx,
                            nb,
                            &sb,
                            &s_addr,
                            32,
                            mem_ports,
                            global_ports,
                        )?;
                        emit_store_to_base(ctx, nb, &db, &d_addr, 32, &word, mem_ports)?;
                    }
                } else {
                    for b in 0..nbytes {
                        let s_addr = if b == 0 {
                            soff.clone()
                        } else {
                            nb.emit(format!("ADD.wrap({soff}, {b}:u64)"))
                        };
                        let d_addr = if b == 0 {
                            doff.clone()
                        } else {
                            nb.emit(format!("ADD.wrap({doff}, {b}:u64)"))
                        };
                        let byte =
                            emit_load_from_base(ctx, nb, &sb, &s_addr, 8, mem_ports, global_ports)?;
                        emit_store_to_base(ctx, nb, &db, &d_addr, 8, &byte, mem_ports)?;
                    }
                }
            } else if callee.starts_with("llvm.memset.") {
                let dst = resolve_value(ctx, nb, &args[0].1, &LlvmType::Ptr, env, global_ports)?;
                let ValueRef::ConstInt { value: nb_val, .. } = args[2].1 else {
                    return Err(err("dynamic-length memset not supported"));
                };
                let VgVal::Ptr {
                    base: db,
                    off_port: doff,
                } = dst
                else {
                    return Err(err("memset on non-pointer"));
                };
                let nbytes = nb_val as u64;
                if nbytes.is_multiple_of(4) {
                    for w in 0..(nbytes / 4) {
                        let d_addr = if w == 0 {
                            doff.clone()
                        } else {
                            nb.emit(format!("ADD.wrap({doff}, {}:u64)", w * 4))
                        };
                        emit_store_to_base(ctx, nb, &db, &d_addr, 32, "0:u32", mem_ports)?;
                    }
                } else {
                    for b in 0..nbytes {
                        let d_addr = if b == 0 {
                            doff.clone()
                        } else {
                            nb.emit(format!("ADD.wrap({doff}, {b}:u64)"))
                        };
                        emit_store_to_base(ctx, nb, &db, &d_addr, 8, "0:u8", mem_ports)?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn emit_sign_extend(nb: &mut NodeBuilder, port: &str, from_bits: u32, to_bits: u32) -> String {
    if from_bits == to_bits {
        return port.to_string();
    }
    if from_bits == 1 {
        let one_u = nb.emit(format!("CAST.u{to_bits}({port})"));
        return nb.emit(format!("NEG.wrap({one_u})"));
    }
    let wide = nb.emit(format!("CAST.u{to_bits}({port})"));
    let sign_bit_mask = 1u64 << (from_bits - 1);
    let masked = nb.emit(format!("AND({port}, {sign_bit_mask}:u{from_bits})"));
    let is_neg = nb.emit(format!("NE({masked}, 0:u{from_bits})"));
    let high_mask = (!0u64) << from_bits;
    let high_mask_masked = if to_bits < 64 {
        high_mask & ((1u64 << to_bits) - 1)
    } else {
        high_mask
    };
    let sign_ext_val = nb.emit(format!("OR({wide}, {high_mask_masked}:u{to_bits})"));
    nb.emit(format!("SELECT({is_neg}, {sign_ext_val}, {wide})"))
}

fn emit_ashr(nb: &mut NodeBuilder, val: &str, shift: &str, bits: u32) -> String {
    let uty = format!("u{bits}");
    let sh32 = cast_to_unsigned(nb, shift, bits, 32);
    let sh_mod = nb.emit(format!("MOD.wrap({sh32}, {bits}:u32)"));
    let lshr = nb.emit(format!("SHR.wrap({val}, {sh_mod})"));
    let sign_bit = 1u64 << (bits - 1);
    let sb = nb.emit(format!("AND({val}, {sign_bit}:{uty})"));
    let is_neg = nb.emit(format!("NE({sb}, 0:{uty})"));
    let all_ones = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let shift_val = nb.emit(format!("SUB.wrap({bits}:u32, {sh_mod})"));
    let fill = nb.emit(format!("SHL.wrap({all_ones}:{uty}, {shift_val})"));
    let is_zero_sh = nb.emit(format!("EQ({sh_mod}, 0:u32)"));
    let fill_or_zero = nb.emit(format!("SELECT({is_zero_sh}, 0:{uty}, {fill})"));
    let neg_res = nb.emit(format!("OR({lshr}, {fill_or_zero})"));
    nb.emit(format!("SELECT({is_neg}, {neg_res}, {lshr})"))
}

fn emit_signed_div(nb: &mut NodeBuilder, a: &str, b: &str, bits: u32) -> String {
    let uty = format!("u{bits}");
    let sign_bit = 1u64 << (bits - 1);
    let a_sb = nb.emit(format!("AND({a}, {sign_bit}:{uty})"));
    let b_sb = nb.emit(format!("AND({b}, {sign_bit}:{uty})"));
    let a_neg = nb.emit(format!("NE({a_sb}, 0:{uty})"));
    let b_neg = nb.emit(format!("NE({b_sb}, 0:{uty})"));
    let neg_a = nb.emit(format!("NEG.wrap({a})"));
    let neg_b = nb.emit(format!("NEG.wrap({b})"));
    let abs_a = nb.emit(format!("SELECT({a_neg}, {neg_a}, {a})"));
    let abs_b = nb.emit(format!("SELECT({b_neg}, {neg_b}, {b})"));
    let q = nb.emit(format!("DIV.wrap({abs_a}, {abs_b})"));
    let neg_q = nb.emit(format!("NEG.wrap({q})"));
    let res_neg = nb.emit(format!("NE({a_neg}, {b_neg})"));
    nb.emit(format!("SELECT({res_neg}, {neg_q}, {q})"))
}

fn emit_signed_rem(nb: &mut NodeBuilder, a: &str, b: &str, bits: u32) -> String {
    let uty = format!("u{bits}");
    let sign_bit = 1u64 << (bits - 1);
    let a_sb = nb.emit(format!("AND({a}, {sign_bit}:{uty})"));
    let b_sb = nb.emit(format!("AND({b}, {sign_bit}:{uty})"));
    let a_neg = nb.emit(format!("NE({a_sb}, 0:{uty})"));
    let b_neg = nb.emit(format!("NE({b_sb}, 0:{uty})"));
    let neg_a = nb.emit(format!("NEG.wrap({a})"));
    let neg_b = nb.emit(format!("NEG.wrap({b})"));
    let abs_a = nb.emit(format!("SELECT({a_neg}, {neg_a}, {a})"));
    let abs_b = nb.emit(format!("SELECT({b_neg}, {neg_b}, {b})"));
    let r = nb.emit(format!("MOD.wrap({abs_a}, {abs_b})"));
    let neg_r = nb.emit(format!("NEG.wrap({r})"));
    nb.emit(format!("SELECT({a_neg}, {neg_r}, {r})"))
}

fn emit_icmp_u(nb: &mut NodeBuilder, pred: ICmpPred, lhs: &str, rhs: &str, bits: u32) -> String {
    match pred {
        ICmpPred::Eq => nb.emit(format!("EQ({lhs}, {rhs})")),
        ICmpPred::Ne => nb.emit(format!("NE({lhs}, {rhs})")),
        ICmpPred::UGt => nb.emit(format!("GT.u({lhs}, {rhs})")),
        ICmpPred::UGe => nb.emit(format!("GE.u({lhs}, {rhs})")),
        ICmpPred::ULt => nb.emit(format!("LT.u({lhs}, {rhs})")),
        ICmpPred::ULe => nb.emit(format!("LE.u({lhs}, {rhs})")),
        ICmpPred::SGt | ICmpPred::SGe | ICmpPred::SLt | ICmpPred::SLe => {
            // Flip sign bit to compare signed two's-complement via unsigned comparison (total!).
            let sign_mask = 1u64 << (bits - 1);
            let lx = nb.emit(format!("XOR({lhs}, {sign_mask}:u{bits})"));
            let rx = nb.emit(format!("XOR({rhs}, {sign_mask}:u{bits})"));
            let cmp = match pred {
                ICmpPred::SGt => "GT.u",
                ICmpPred::SGe => "GE.u",
                ICmpPred::SLt => "LT.u",
                ICmpPred::SLe => "LE.u",
                _ => unreachable!(),
            };
            nb.emit(format!("{cmp}({lx}, {rx})"))
        }
    }
}

fn emit_gep_offset_expr(
    ctx: &LiftCtx<'_>,
    nb: &mut NodeBuilder,
    base_ty: &LlvmType,
    indices: &[(LlvmType, ValueRef)],
    env: &BTreeMap<String, VgVal>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> Result<String, Diagnostic> {
    if indices.is_empty() {
        return Ok("0:u64".to_string());
    }
    let mut acc: Option<String> = None;
    let add_term = |nb: &mut NodeBuilder, acc: &mut Option<String>, term: String| {
        if term == "0:u64" {
            return;
        }
        *acc = Some(match acc.take() {
            None => term,
            Some(prev) => nb.emit(format!("ADD.wrap({prev}, {term})")),
        });
    };

    let stride0 = ctx.module.sizeof(base_ty)?;
    let (ity0, ival0) = &indices[0];
    let term0 = scale_gep_index(ctx, nb, ity0, ival0, stride0, env, global_ports)?;
    add_term(nb, &mut acc, term0);

    let mut cur_ty = base_ty.clone();
    for (ity, ival) in &indices[1..] {
        match cur_ty {
            LlvmType::Array(_, elem) => {
                let esz = ctx.module.sizeof(&elem)?;
                let t = scale_gep_index(ctx, nb, ity, ival, esz, env, global_ports)?;
                add_term(nb, &mut acc, t);
                cur_ty = *elem;
            }
            LlvmType::Struct { .. } | LlvmType::Named(_) => {
                let ValueRef::ConstInt { value: fidx, .. } = ival else {
                    return Err(err("non-constant struct GEP index"));
                };
                let (foff, fty) = ctx.module.struct_field(&cur_ty, *fidx as usize)?;
                if foff != 0 {
                    add_term(nb, &mut acc, format!("{foff}:u64"));
                }
                cur_ty = fty;
            }
            _ => return Err(err(format!("unsupported GEP step through {cur_ty:?}"))),
        }
    }
    Ok(acc.unwrap_or_else(|| "0:u64".to_string()))
}

fn scale_gep_index(
    ctx: &LiftCtx<'_>,
    nb: &mut NodeBuilder,
    ity: &LlvmType,
    ival: &ValueRef,
    stride: u64,
    env: &BTreeMap<String, VgVal>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> Result<String, Diagnostic> {
    if stride == 0 {
        return Ok("0:u64".to_string());
    }
    if let ValueRef::ConstInt { value, .. } = ival {
        let bits = ity.bit_width().unwrap_or(64);
        let shift = 128 - bits;
        let sext = ((mask_to_u128(*value, bits) << shift) as i128) >> shift;
        let off = (sext as i64).wrapping_mul(stride as i64) as u64;
        return Ok(format!("{off}:u64"));
    }
    let v = resolve_value(ctx, nb, ival, ity, env, global_ports)?;
    let (u, bits) = coerce_to_unsigned(nb, &v);
    let idx64 = emit_sign_extend(nb, &u, bits, 64);
    if stride == 1 {
        Ok(idx64)
    } else {
        Ok(nb.emit(format!("MUL.wrap({idx64}, {stride}:u64)")))
    }
}

fn emit_umin_u64(nb: &mut NodeBuilder, a: &str, b: &str) -> String {
    let cmp = nb.emit(format!("LT.u({a}, {b})"));
    nb.emit(format!("SELECT({cmp}, {a}, {b})"))
}

fn emit_load_from_base(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    base: &PtrBase,
    off_port: &str,
    bits: u32,
    mem_ports: &BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> Result<String, Diagnostic> {
    match base {
        PtrBase::Global(gname) => {
            let (gport, _) = global_ports
                .get(gname)
                .ok_or_else(|| err(format!("global @{gname} not in scope")))?;
            let (ebits, ref gvals) = ctx.global_consts_used[gname];
            let glen = gvals.len() as u64;
            if bits == ebits {
                let is_const_off = off_port.ends_with(":u64") || nb.label == ctx.root_label;
                let ebytes = u64::from(ebits.div_ceil(8));
                let raw_idx = if ebytes == 1 {
                    off_port.to_string()
                } else {
                    nb.emit(format!("DIV.wrap({off_port}, {ebytes}:u64)"))
                };
                let idx = clamp_array_idx(nb, raw_idx, glen, is_const_off);
                Ok(nb.emit(format!("INDEX({gport}, {idx})")))
            } else {
                Err(err(format!(
                    "mismatched load width {bits} from @{gname} (elem {ebits})"
                )))
            }
        }
        PtrBase::Param(pi) => match &ctx.param_kinds[*pi] {
            ParamKind::Slice { .. } => {
                let sname = ctx.param_to_vg_name[pi].clone();
                if bits == 8 {
                    Ok(emit_safe_slice_byte_load(ctx, nb, &sname, off_port))
                } else if bits == 32 {
                    // Little-endian 4-byte load from Slice<u8>.
                    let mut acc: Option<String> = None;
                    for b in 0..4u64 {
                        let a = if b == 0 {
                            off_port.to_string()
                        } else {
                            nb.emit(format!("ADD.wrap({off_port}, {b}:u64)"))
                        };
                        let byte = emit_safe_slice_byte_load(ctx, nb, &sname, &a);
                        let w = nb.emit(format!("CAST.u32({byte})"));
                        let sh = if b == 0 {
                            w
                        } else {
                            nb.emit(format!("SHL.wrap({w}, {}:u32)", b * 8))
                        };
                        acc = Some(match acc {
                            None => sh,
                            Some(prev) => nb.emit(format!("OR({prev}, {sh})")),
                        });
                    }
                    Ok(acc.unwrap())
                } else {
                    Err(err(format!("unsupported {bits}-bit load from Slice<u8>")))
                }
            }
            ParamKind::FixedArray {
                elem_bits,
                len,
                writable,
                ..
            } => {
                let arr_port = if *writable {
                    mem_ports[&MemSlot::WritableParam(*pi)].clone()
                } else {
                    ctx.param_to_vg_name[pi].clone()
                };
                let clamp_len_opt = (nb.label != ctx.root_label).then_some(*len);
                emit_load_from_array(
                    nb,
                    &arr_port,
                    *elem_bits,
                    clamp_len_opt.unwrap_or(0),
                    off_port,
                    bits,
                )
            }
            ParamKind::Scalar { .. } | ParamKind::SliceLen { .. } => {
                Err(err("load from scalar param"))
            }
        },
        PtrBase::Alloca(aid) => {
            let slot = MemSlot::Alloca(*aid);
            let spec = &ctx.mem_specs[&slot];
            let arr_port = &mem_ports[&slot];
            let clamp_len = if nb.label != ctx.root_label {
                spec.len
            } else {
                0
            };
            emit_load_from_array(nb, arr_port, spec.elem_bits, clamp_len, off_port, bits)
        }
        PtrBase::Null => Err(err("null pointer load")),
    }
}

fn emit_safe_slice_byte_load(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    sname: &str,
    off_port: &str,
) -> String {
    let read_name = ctx.fresh_node_name("read_slice_byte");
    let mut read_nb = NodeBuilder::new(&read_name);
    read_nb.params = vec![
        ("s".into(), "Slice<u8>".into()),
        ("idx".into(), "u64".into()),
    ];
    read_nb.results = vec!["u8".into()];
    read_nb.pre.push("idx < len(s)".into());
    let b = read_nb.emit("INDEX(s, idx)");
    read_nb.ret_exprs = vec![b];
    ctx.emitted_nodes.push(read_nb);

    let zero_name = ctx.fresh_node_name("zero_slice_byte");
    let mut zero_nb = NodeBuilder::new(&zero_name);
    zero_nb.params = vec![
        ("s".into(), "Slice<u8>".into()),
        ("idx".into(), "u64".into()),
    ];
    zero_nb.results = vec!["u8".into()];
    zero_nb.ret_exprs = vec!["0:u8".into()];
    ctx.emitted_nodes.push(zero_nb);

    let slen = nb.emit(format!("LEN({sname})"));
    let ok = nb.emit(format!("LT.u({off_port}, {slen})"));
    nb.emit(format!(
        "ASSERT_GUARD.branch({ok}, @{read_name}, {sname}, {off_port}, else=@{zero_name})"
    ))
}

fn clamp_array_idx(nb: &mut NodeBuilder, idx: String, arr_len: u64, is_const_off: bool) -> String {
    if arr_len == 0 || is_const_off || idx.ends_with(":u64") {
        idx
    } else {
        emit_umin_u64(nb, &idx, &format!("{}:u64", arr_len - 1))
    }
}

fn emit_load_from_array(
    nb: &mut NodeBuilder,
    arr_port: &str,
    elem_bits: u32,
    arr_len: u64,
    off_port: &str,
    load_bits: u32,
) -> Result<String, Diagnostic> {
    let is_const_off = off_port.ends_with(":u64");
    if elem_bits == load_bits {
        let ebytes = u64::from(elem_bits.div_ceil(8));
        let raw_idx = if ebytes == 1 {
            off_port.to_string()
        } else {
            nb.emit(format!("DIV.wrap({off_port}, {ebytes}:u64)"))
        };
        let idx = clamp_array_idx(nb, raw_idx, arr_len, is_const_off);
        Ok(nb.emit(format!("INDEX({arr_port}, {idx})")))
    } else if elem_bits == 8 && load_bits == 32 {
        let base_off = if arr_len >= 4 && !is_const_off {
            emit_umin_u64(nb, off_port, &format!("{}:u64", arr_len - 4))
        } else {
            off_port.to_string()
        };
        let mut acc: Option<String> = None;
        for b in 0..4u64 {
            let a = if b == 0 {
                base_off.clone()
            } else {
                nb.emit(format!("ADD.wrap({base_off}, {b}:u64)"))
            };
            let byte = nb.emit(format!("INDEX({arr_port}, {a})"));
            let w = nb.emit(format!("CAST.u32({byte})"));
            let sh = if b == 0 {
                w
            } else {
                nb.emit(format!("SHL.wrap({w}, {}:u32)", b * 8))
            };
            acc = Some(match acc {
                None => sh,
                Some(prev) => nb.emit(format!("OR({prev}, {sh})")),
            });
        }
        Ok(acc.unwrap())
    } else if elem_bits == 32 && load_bits == 8 {
        let raw_widx = nb.emit(format!("SHR.wrap({off_port}, 2:u32)"));
        let widx = clamp_array_idx(nb, raw_widx, arr_len, is_const_off);
        let word = nb.emit(format!("INDEX({arr_port}, {widx})"));
        let sub = nb.emit(format!("AND({off_port}, 3:u64)"));
        let sh = nb.emit(format!("MUL.wrap({sub}, 8:u64)"));
        let sh32 = nb.emit(format!("CAST.u32({sh})"));
        let shifted = nb.emit(format!("SHR.wrap({word}, {sh32})"));
        let masked = nb.emit(format!("AND({shifted}, 255:u32)"));
        Ok(nb.emit(format!("CAST.u8({masked})")))
    } else {
        Err(err(format!(
            "unsupported {load_bits}-bit load from Array<u{elem_bits}>"
        )))
    }
}

fn emit_store_to_base(
    ctx: &LiftCtx<'_>,
    nb: &mut NodeBuilder,
    base: &PtrBase,
    off_port: &str,
    store_bits: u32,
    val_port: &str,
    mem_ports: &mut BTreeMap<MemSlot, String>,
) -> Result<(), Diagnostic> {
    let is_const_off = off_port.ends_with(":u64") || nb.label == ctx.root_label;
    let slot = match base {
        PtrBase::Alloca(aid) => MemSlot::Alloca(*aid),
        PtrBase::Param(pi) => MemSlot::WritableParam(*pi),
        _ => return Err(err("store to non-writable memory base")),
    };
    let spec = &ctx.mem_specs[&slot];
    let cur_arr = mem_ports[&slot].clone();
    let updated = if spec.elem_bits == store_bits {
        let ebytes = u64::from(spec.elem_bits.div_ceil(8));
        let raw_idx = if ebytes == 1 {
            off_port.to_string()
        } else {
            nb.emit(format!("DIV.wrap({off_port}, {ebytes}:u64)"))
        };
        let idx = clamp_array_idx(nb, raw_idx, spec.len, is_const_off);
        nb.emit(format!("UPDATE({cur_arr}, {idx}, {val_port})"))
    } else if spec.elem_bits == 8 && store_bits == 32 {
        let base_off = if spec.len >= 4 && !is_const_off {
            emit_umin_u64(nb, off_port, &format!("{}:u64", spec.len - 4))
        } else {
            off_port.to_string()
        };
        let mut arr = cur_arr;
        for b in 0..4u64 {
            let a = if b == 0 {
                base_off.clone()
            } else {
                nb.emit(format!("ADD.wrap({base_off}, {b}:u64)"))
            };
            let sh = if b == 0 {
                val_port.to_string()
            } else {
                nb.emit(format!("SHR.wrap({val_port}, {}:u32)", b * 8))
            };
            let masked = nb.emit(format!("AND({sh}, 255:u32)"));
            let byte = nb.emit(format!("CAST.u8({masked})"));
            arr = nb.emit(format!("UPDATE({arr}, {a}, {byte})"));
        }
        arr
    } else {
        return Err(err(format!(
            "unsupported {store_bits}-bit store into Array<u{}>",
            spec.elem_bits
        )));
    };
    mem_ports.insert(slot, updated);
    Ok(())
}

// ============================================================================
// Counted Loop Extraction & Lowering to FOLD_BOUNDED
// ============================================================================

#[derive(Clone, Debug)]
struct LoopCarriedPhi {
    name: String,
    ty: LlvmType,
    init_val: ValueRef,
    next_val: ValueRef,
}

#[derive(Clone, Debug)]
enum IvKind {
    /// `iv_next = add iv, step`; exit when `iv_next == bound` (or `iv_next < bound` continues).
    Up {
        iv_name: String,
        init: ValueRef,
        bound: ValueRef,
        step: u64,
        bits: u32,
    },
    /// `iv_next = add iv, -step`; continue while `iv_next > thresh` or exit when `iv_next == 0`.
    Down {
        iv_name: String,
        init: ValueRef,
        step: u64,
        bits: u32,
    },
}

#[derive(Clone, Debug)]
struct SliceAdvance {
    phi_name: String,
    indexed_gep_name: Option<String>,
    slice_param_idx: usize,
    init_ptr: ValueRef,
    byte_stride: u64,
}

fn analyze_single_block_iv(
    ctx: &LiftCtx<'_>,
    lp: &NaturalLoop,
    phis: &[LoopCarriedPhi],
) -> Result<(IvKind, Option<SliceAdvance>), Diagnostic> {
    let latch_bb = ctx.bb_map[lp.latch.as_str()];
    let Terminator::CondBr {
        cond: ValueRef::Local(cond_name),
        then_bb,
        else_bb,
    } = &latch_bb.term
    else {
        return Err(err(format!(
            "loop at %{} has non-conditional latch",
            lp.header
        )));
    };

    let cond_inst = latch_bb
        .insts
        .iter()
        .find(|i| i.result.as_deref() == Some(cond_name.as_str()))
        .ok_or_else(|| err(format!("loop condition %{cond_name} not in latch")))?;

    let InstKind::ICmp {
        pred,
        ty,
        lhs: ValueRef::Local(cmp_lhs),
        rhs: cmp_rhs,
        ..
    } = &cond_inst.kind
    else {
        return Err(err(format!(
            "loop at %{} exit condition is not icmp",
            lp.header
        )));
    };

    let continue_on_then = lp.body.contains(then_bb) && !lp.body.contains(else_bb);
    let continue_on_else = lp.body.contains(else_bb) && !lp.body.contains(then_bb);
    if !continue_on_then && !continue_on_else {
        return Err(err(format!("loop at %{} latch does not exit", lp.header)));
    }

    // Look for induction variable updated as `cmp_lhs = add phi, step`.
    let mut found_iv = None;
    for phi in phis {
        if phi.ty == LlvmType::Ptr {
            continue;
        }
        let ValueRef::Local(next_name) = &phi.next_val else {
            continue;
        };
        if next_name != cmp_lhs {
            continue;
        }
        let next_inst = latch_bb
            .insts
            .iter()
            .find(|i| i.result.as_deref() == Some(next_name.as_str()));
        if let Some(Inst {
            kind:
                InstKind::BinOp {
                    op: BinOpKind::Add,
                    lhs: ValueRef::Local(add_l),
                    rhs:
                        ValueRef::ConstInt {
                            value: step_val, ..
                        },
                    ..
                },
            ..
        }) = next_inst
            && add_l == &phi.name
        {
            let bits = ty.bit_width().unwrap_or(64);
            if *step_val > 0 {
                let valid = matches!(
                    (pred, continue_on_then),
                    (ICmpPred::Eq, false)
                        | (ICmpPred::Ne, true)
                        | (ICmpPred::ULt, true)
                        | (ICmpPred::SLt, true)
                );
                if valid {
                    found_iv = Some(IvKind::Up {
                        iv_name: phi.name.clone(),
                        init: phi.init_val.clone(),
                        bound: cmp_rhs.clone(),
                        step: *step_val as u64,
                        bits,
                    });
                    break;
                }
            } else if *step_val < 0 {
                let step = (-*step_val) as u64;
                let valid = match (pred, continue_on_then, cmp_rhs) {
                    (ICmpPred::Eq, false, ValueRef::ConstInt { value: 0, .. }) => step == 1,
                    (ICmpPred::UGt, true, ValueRef::ConstInt { value: thresh, .. }) => {
                        (*thresh as u64) + 1 == step
                    }
                    _ => false,
                };
                if valid {
                    found_iv = Some(IvKind::Down {
                        iv_name: phi.name.clone(),
                        init: phi.init_val.clone(),
                        step,
                        bits,
                    });
                    break;
                }
            }
        }
    }

    let iv = found_iv.ok_or_else(|| {
        err(format!(
            "loop at %{} has uncomputable or non-counted trip count",
            lp.header
        ))
    })?;

    // Check if any Ptr phi advances by a constant byte stride over a Slice<u8> param!
    let mut slice_adv = None;
    for phi in phis {
        if phi.ty != LlvmType::Ptr {
            continue;
        }
        let ValueRef::Local(next_name) = &phi.next_val else {
            continue;
        };
        if let Some(gep_inst) = find_inst_in_loop(ctx, lp, next_name)
            && let InstKind::Gep {
                base_ty,
                ptr: ValueRef::Local(pbase),
                indices,
                ..
            } = &gep_inst.kind
            && pbase == &phi.name
            && let Ok(Some(stride)) =
                crate::analysis::eval_const_gep_offset(ctx.module, base_ty, indices)
            && stride > 0
            && let Some(slice_pi) = find_slice_param_of_ptr(ctx, lp, &phi.init_val)
        {
            slice_adv = Some(SliceAdvance {
                phi_name: phi.name.clone(),
                indexed_gep_name: None,
                slice_param_idx: slice_pi,
                init_ptr: phi.init_val.clone(),
                byte_stride: stride as u64,
            });
            break;
        }
    }

    // Also handle `gep i8, ptr %slice, i64 %iv` where `iv` is an Up counter with step == 1.
    if slice_adv.is_none()
        && let IvKind::Up {
            iv_name,
            init: _,
            step: 1,
            ..
        } = &iv
    {
        for b in &lp.body {
            for inst in &ctx.bb_map[b.as_str()].insts {
                if let InstKind::Gep {
                    base_ty: LlvmType::Int(8),
                    ptr,
                    indices,
                    ..
                } = &inst.kind
                    && indices.len() == 1
                    && indices[0].1 == ValueRef::Local(iv_name.clone())
                    && let Some(slice_pi) = find_slice_param_of_ptr(ctx, lp, ptr)
                    && let Some(gep_res) = &inst.result
                {
                    let init_ptr = ptr.clone();
                    slice_adv = Some(SliceAdvance {
                        phi_name: iv_name.clone(),
                        indexed_gep_name: Some(gep_res.clone()),
                        slice_param_idx: slice_pi,
                        init_ptr,
                        byte_stride: 1,
                    });
                    break;
                }
            }
            if slice_adv.is_some() {
                break;
            }
        }
    }

    Ok((iv, slice_adv))
}

fn find_inst_in_loop<'a>(ctx: &'a LiftCtx<'a>, lp: &NaturalLoop, name: &str) -> Option<&'a Inst> {
    for b in &lp.body {
        for inst in &ctx.bb_map[b.as_str()].insts {
            if inst.result.as_deref() == Some(name) {
                return Some(inst);
            }
        }
    }
    None
}

fn find_slice_param_of_ptr(
    ctx: &LiftCtx<'_>,
    lp: &NaturalLoop,
    init_ptr: &ValueRef,
) -> Option<usize> {
    let mut visited = BTreeSet::new();
    find_slice_param_of_ptr_rec(ctx, lp, init_ptr, &mut visited)
}

fn find_slice_param_of_ptr_rec(
    ctx: &LiftCtx<'_>,
    lp: &NaturalLoop,
    init_ptr: &ValueRef,
    visited: &mut BTreeSet<String>,
) -> Option<usize> {
    let _ = lp;
    let ValueRef::Local(n) = init_ptr else {
        return None;
    };
    if !visited.insert(n.clone()) {
        return None;
    }
    for (i, p) in ctx.func.params.iter().enumerate() {
        if p.name == *n && matches!(ctx.param_kinds[i], ParamKind::Slice { .. }) {
            return Some(i);
        }
    }
    for bb in &ctx.func.blocks {
        for inst in &bb.insts {
            if inst.result.as_deref() == Some(n.as_str()) {
                match &inst.kind {
                    InstKind::Gep { ptr, .. } => {
                        return find_slice_param_of_ptr_rec(ctx, lp, ptr, visited);
                    }
                    InstKind::Phi { incoming, .. } => {
                        for (v, _) in incoming {
                            if let Some(pi) = find_slice_param_of_ptr_rec(ctx, lp, v, visited) {
                                return Some(pi);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    None
}

fn collect_loop_phis(
    ctx: &LiftCtx<'_>,
    lp: &NaturalLoop,
) -> Result<Vec<LoopCarriedPhi>, Diagnostic> {
    let hdr_bb = ctx.bb_map[lp.header.as_str()];
    let mut phis = Vec::new();
    for inst in &hdr_bb.insts {
        if let InstKind::Phi { ty, incoming } = &inst.kind {
            let name = inst.result.clone().unwrap();
            let init_val = incoming
                .iter()
                .find(|(_, p)| p == &lp.preheader)
                .map(|(v, _)| v.clone())
                .ok_or_else(|| err(format!("phi %{name} missing preheader edge")))?;
            let next_val = incoming
                .iter()
                .find(|(_, p)| p == &lp.latch)
                .map(|(v, _)| v.clone())
                .ok_or_else(|| err(format!("phi %{name} missing latch edge")))?;
            phis.push(LoopCarriedPhi {
                name,
                ty: ty.clone(),
                init_val,
                next_val,
            });
        }
    }
    Ok(phis)
}

fn collect_modified_mem_slots_in_loop(
    ctx: &LiftCtx<'_>,
    lp: &NaturalLoop,
    env: &BTreeMap<String, VgVal>,
) -> Vec<MemSlot> {
    let find_base = |v: &ValueRef| -> Option<PtrBase> {
        if let ValueRef::Local(n) = v
            && let Some((b, _)) = ctx.ptr_info.get(n)
        {
            return Some(b.clone());
        }
        resolve_ptr_val(v, env).and_then(|pv| match pv {
            VgVal::Ptr { base, .. } => Some(base),
            _ => None,
        })
    };
    let mut slots = BTreeSet::new();
    for b in &lp.body {
        for inst in &ctx.bb_map[b.as_str()].insts {
            match &inst.kind {
                InstKind::Store { ptr, .. } => {
                    if let Some(base) = find_base(ptr) {
                        match base {
                            PtrBase::Alloca(aid) => {
                                slots.insert(MemSlot::Alloca(aid));
                            }
                            PtrBase::Param(pi)
                                if matches!(
                                    ctx.param_kinds[pi],
                                    ParamKind::FixedArray { writable: true, .. }
                                ) =>
                            {
                                slots.insert(MemSlot::WritableParam(pi));
                            }
                            _ => {}
                        }
                    }
                }
                InstKind::Call { callee, args, .. }
                    if callee.starts_with("llvm.memcpy.") || callee.starts_with("llvm.memset.") =>
                {
                    if let Some(base) = find_base(&args[0].1) {
                        match base {
                            PtrBase::Alloca(aid) => {
                                slots.insert(MemSlot::Alloca(aid));
                            }
                            PtrBase::Param(pi)
                                if matches!(
                                    ctx.param_kinds[pi],
                                    ParamKind::FixedArray { writable: true, .. }
                                ) =>
                            {
                                slots.insert(MemSlot::WritableParam(pi));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
    slots.into_iter().collect()
}

fn lower_natural_loop(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    loop_idx: usize,
    _prev: Option<&str>,
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> Result<(), Diagnostic> {
    let lp = ctx.loops[loop_idx].clone();

    // Handle nested loop (outer loop contains exactly 1 inner loop, e.g., adler32_z NMAX=5552):
    if !lp.children.is_empty() {
        if lp.children.len() != 1 {
            return Err(err(format!("loop at %{} has >1 child loops", lp.header)));
        }
        return lower_nested_slice_loop(
            ctx,
            nb,
            loop_idx,
            lp.children[0],
            env,
            mem_ports,
            global_ports,
        );
    }

    let phis = collect_loop_phis(ctx, &lp)?;
    let (iv, slice_adv) = analyze_single_block_iv(ctx, &lp, &phis)?;
    let mod_mem = collect_modified_mem_slots_in_loop(ctx, &lp, env);

    // Check if constant-trip loop (no slice advance needed; iterate [0..N-1]):
    let const_trip = match &iv {
        IvKind::Up {
            init: ValueRef::ConstInt { value: i0, .. },
            bound: ValueRef::ConstInt { value: b0, .. },
            step,
            ..
        } if *b0 >= *i0 && *step > 0 => Some(((*b0 - *i0) as u64).div_ceil(*step)),
        IvKind::Down {
            init: ValueRef::ConstInt { value: i0, .. },
            step,
            ..
        } if *i0 >= 0 && *step > 0 => Some((*i0 as u64) / *step),
        _ => None,
    };

    if let Some(n) = const_trip
        && slice_adv.is_none()
    {
        return emit_const_trip_fold(
            ctx,
            nb,
            &lp,
            &phis,
            &iv,
            n,
            &mod_mem,
            env,
            mem_ports,
            global_ports,
        );
    }

    if let Some(sa) = slice_adv {
        return emit_slice_fold(
            ctx,
            nb,
            &lp,
            &phis,
            &iv,
            &sa,
            &mod_mem,
            env,
            mem_ports,
            global_ports,
        );
    }

    // Dynamic counted loop without a Slice pointer: use GENERATE(@idx_gen, trip) (adds ARENA effect).
    emit_dynamic_index_fold(
        ctx,
        nb,
        &lp,
        &phis,
        &iv,
        &mod_mem,
        env,
        mem_ports,
        global_ports,
    )
}

#[allow(clippy::too_many_arguments)]
fn emit_const_trip_fold(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    lp: &NaturalLoop,
    phis: &[LoopCarriedPhi],
    iv: &IvKind,
    n: u64,
    mod_mem: &[MemSlot],
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> Result<(), Diagnostic> {
    let init_val_port_of = |ctx: &LiftCtx<'_>,
                            nb: &mut NodeBuilder,
                            v: &ValueRef,
                            ty: &LlvmType,
                            env: &BTreeMap<String, VgVal>|
     -> Result<String, Diagnostic> {
        let rv = resolve_value(ctx, nb, v, ty, env, global_ports)?;
        Ok(match ty {
            LlvmType::Ptr => coerce_to_unsigned(nb, &rv).0,
            LlvmType::Int(1) => coerce_to_type(nb, &rv, "Bool", 1),
            LlvmType::Int(b) => coerce_to_type(nb, &rv, &format!("u{b}"), *b),
            _ => return Err(err(format!("unsupported phi type {ty:?}"))),
        })
    };

    // Build initial state elements: all loop PHIs + modified memory arrays.
    let mut state_ports = Vec::new();
    let mut state_tys = Vec::new();
    for phi in phis {
        state_ports.push(init_val_port_of(ctx, nb, &phi.init_val, &phi.ty, env)?);
        let t = match &phi.ty {
            LlvmType::Ptr => "u64".to_string(),
            LlvmType::Int(1) => "Bool".to_string(),
            LlvmType::Int(b) => format!("u{b}"),
            _ => return Err(err("unsupported phi type")),
        };
        state_tys.push(t);
    }
    for slot in mod_mem {
        state_ports.push(mem_ports[slot].clone());
        state_tys.push(ctx.mem_specs[slot].vg_type.clone());
    }

    let (init_acc, acc_ty) = if state_ports.len() == 1 {
        (state_ports[0].clone(), state_tys[0].clone())
    } else {
        let ty = format!("Tuple<{}>", state_tys.join(", "));
        let p = nb.emit(format!("TUPLE({})", state_ports.join(", ")));
        (p, ty)
    };

    let indices_str: Vec<String> = (0..n).map(|k| k.to_string()).collect();
    let xs_port = nb.emit(format!(
        "IDENTITY([{}]:Array<u64, {n}>)",
        indices_str.join(", ")
    ));

    // Collect fixed-size broadcasts: all env scalars/ptrs (not inside loop phis), non-modified mems, globals, and FixedArray params!
    let (b_params, b_args) =
        collect_fold_broadcasts(ctx, lp, phis, mod_mem, env, mem_ports, global_ports);

    let step_name = ctx.fresh_node_name("fold_step");
    let mut step_nb = NodeBuilder::new(step_name.clone());
    step_nb.params.push(("acc".to_string(), acc_ty.clone()));
    step_nb
        .params
        .push(("iter_idx".to_string(), "u64".to_string()));
    for (_, pname, pty) in &b_params {
        step_nb.params.push((pname.clone(), pty.clone()));
    }
    step_nb.results = vec![acc_ty.clone()];

    let mut k_env = BTreeMap::new();
    let mut k_mem = BTreeMap::new();
    let mut k_glob = BTreeMap::new();
    bind_fold_broadcasts(&b_params, env, &mut k_env, &mut k_mem, &mut k_glob);

    // Unpack `acc` into phis and mem slots.
    let unpacked: Vec<String> = if state_ports.len() == 1 {
        vec!["acc".to_string()]
    } else {
        (0..state_ports.len())
            .map(|j| step_nb.emit(format!("PROJ(acc, {j}:u32)")))
            .collect()
    };
    let mut pos = 0;
    for phi in phis {
        let p = unpacked[pos].clone();
        pos += 1;
        match &phi.ty {
            LlvmType::Ptr => {
                let base = resolve_ptr_val(&phi.init_val, env)
                    .and_then(|v| {
                        if let VgVal::Ptr { base, .. } = v {
                            Some(base)
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| err("unknown ptr phi base"))?;
                k_env.insert(phi.name.clone(), VgVal::Ptr { base, off_port: p });
            }
            LlvmType::Int(b) => {
                k_env.insert(
                    phi.name.clone(),
                    VgVal::Scalar {
                        port: p,
                        ty: if *b == 1 {
                            "Bool".to_string()
                        } else {
                            format!("u{b}")
                        },
                        bits: *b,
                    },
                );
            }
            _ => {}
        }
    }
    for slot in mod_mem {
        k_mem.insert(slot.clone(), unpacked[pos].clone());
        pos += 1;
    }

    // Lower the loop body inside `step_nb`!
    // If single-block loop (`lp.body.len() == 1`): lower non-Phi instructions in `lp.header` directly!
    // If multi-block loop (`lp.body.len() > 1`, e.g., mbedtls 0..64 with `if (i < 16)`):
    // lower starting after Phis in `lp.header`, stopping at `lp.latch`, then lower `lp.latch`!
    let next_state_ports = lower_loop_body_to_next_state(
        ctx,
        &mut step_nb,
        lp,
        phis,
        mod_mem,
        &mut k_env,
        &mut k_mem,
        &k_glob,
    )?;

    let out_acc = if next_state_ports.len() == 1 {
        next_state_ports[0].clone()
    } else {
        step_nb.emit(format!("TUPLE({})", next_state_ports.join(", ")))
    };
    step_nb.ret_exprs = vec![out_acc];
    ctx.emitted_nodes.push(step_nb);

    let (inv_lbl, inv_body) = ctx.fresh_inv_label();
    nb.invariants.push((inv_lbl.clone(), inv_body));
    let fold_call = if b_args.is_empty() {
        format!("FOLD_BOUNDED(@{step_name}, {xs_port}, init={init_acc}, invariant=@{inv_lbl})")
    } else {
        format!(
            "FOLD_BOUNDED(@{step_name}, {xs_port}, {}, init={init_acc}, invariant=@{inv_lbl})",
            b_args.join(", ")
        )
    };
    let final_acc = nb.emit(fold_call);
    unpack_loop_exit_state(
        ctx,
        nb,
        lp,
        phis,
        mod_mem,
        &final_acc,
        state_ports.len(),
        iv,
        env,
        mem_ports,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_slice_fold(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    lp: &NaturalLoop,
    phis: &[LoopCarriedPhi],
    iv: &IvKind,
    sa: &SliceAdvance,
    mod_mem: &[MemSlot],
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> Result<(), Diagnostic> {
    let slice_name = ctx.param_to_vg_name[&sa.slice_param_idx].clone();
    let start_ptr_val = resolve_value(ctx, nb, &sa.init_ptr, &LlvmType::Ptr, env, global_ports)?;
    let VgVal::Ptr {
        off_port: start_off_port,
        ..
    } = start_ptr_val
    else {
        return Err(err("slice advance init is not a ptr"));
    };

    let trip_port = compute_trip_count_port(ctx, nb, iv, env, global_ports)?;
    let byte_len_port = if sa.byte_stride == 1 {
        trip_port.clone()
    } else {
        nb.emit(format!("MUL.wrap({trip_port}, {}:u64)", sa.byte_stride))
    };
    let end_off_port = if start_off_port == "0:u64" {
        byte_len_port.clone()
    } else {
        nb.emit(format!("ADD.wrap({start_off_port}, {byte_len_port})"))
    };
    let slice_len = nb.emit(format!("LEN({slice_name})"));
    let clamped_end = emit_umin_u64(nb, &end_off_port, &slice_len);
    let clamped_start = if start_off_port == "0:u64" {
        "0:u64".to_string()
    } else {
        emit_umin_u64(nb, &start_off_port, &clamped_end)
    };
    let slice_view = nb.emit(format!(
        "SLICE({slice_name}, {clamped_start}, {clamped_end})"
    ));

    // State excludes `sa.phi_name` when it is a Ptr phi (handled by slice iteration).
    // When `sa.indexed_gep_name.is_some()`, `sa.phi_name` is an integer IV that may still be used in the body.
    let active_phis: Vec<&LoopCarriedPhi> = phis
        .iter()
        .filter(|p| sa.indexed_gep_name.is_some() || p.name != sa.phi_name)
        .collect();

    let mut state_ports = Vec::new();
    let mut state_tys = Vec::new();
    for phi in &active_phis {
        let rv = resolve_value(ctx, nb, &phi.init_val, &phi.ty, env, global_ports)?;
        let p = match &phi.ty {
            LlvmType::Ptr => coerce_to_unsigned(nb, &rv).0,
            LlvmType::Int(1) => coerce_to_type(nb, &rv, "Bool", 1),
            LlvmType::Int(b) => coerce_to_type(nb, &rv, &format!("u{b}"), *b),
            _ => return Err(err("unsupported phi type")),
        };
        state_ports.push(p);
        state_tys.push(match &phi.ty {
            LlvmType::Ptr => "u64".to_string(),
            LlvmType::Int(1) => "Bool".to_string(),
            LlvmType::Int(b) => format!("u{b}"),
            _ => unreachable!(),
        });
    }
    for slot in mod_mem {
        state_ports.push(mem_ports[slot].clone());
        state_tys.push(ctx.mem_specs[slot].vg_type.clone());
    }

    let (b_params, b_args) =
        collect_fold_broadcasts(ctx, lp, phis, mod_mem, env, mem_ports, global_ports);

    if sa.byte_stride == 1 {
        // Direct byte-by-byte fold!
        let (init_acc, acc_ty) = if state_ports.len() == 1 {
            (state_ports[0].clone(), state_tys[0].clone())
        } else {
            let ty = format!("Tuple<{}>", state_tys.join(", "));
            let p = nb.emit(format!("TUPLE({})", state_ports.join(", ")));
            (p, ty)
        };

        let step_name = ctx.fresh_node_name("slice_step");
        let mut step_nb = NodeBuilder::new(step_name.clone());
        step_nb.params.push(("acc".to_string(), acc_ty.clone()));
        step_nb
            .params
            .push(("cur_byte".to_string(), "u8".to_string()));
        for (_, pname, pty) in &b_params {
            step_nb.params.push((pname.clone(), pty.clone()));
        }
        step_nb.results = vec![acc_ty.clone()];

        let mut k_env = BTreeMap::new();
        let mut k_mem = BTreeMap::new();
        let mut k_glob = BTreeMap::new();
        bind_fold_broadcasts(&b_params, env, &mut k_env, &mut k_mem, &mut k_glob);

        let unpacked: Vec<String> = if state_ports.len() == 1 {
            vec!["acc".to_string()]
        } else {
            (0..state_ports.len())
                .map(|j| step_nb.emit(format!("PROJ(acc, {j}:u32)")))
                .collect()
        };
        for (idx, phi) in active_phis.iter().enumerate() {
            let p = unpacked[idx].clone();
            match &phi.ty {
                LlvmType::Int(b) => {
                    k_env.insert(
                        phi.name.clone(),
                        VgVal::Scalar {
                            port: p,
                            ty: if *b == 1 {
                                "Bool".to_string()
                            } else {
                                format!("u{b}")
                            },
                            bits: *b,
                        },
                    );
                }
                _ => return Err(err("non-int phi in slice fold")),
            }
        }
        for (idx, slot) in mod_mem.iter().enumerate() {
            k_mem.insert(slot.clone(), unpacked[active_phis.len() + idx].clone());
        }

        // Bind `sa.phi_name` (and `sa.indexed_gep_name` if GEP-indexed) to a virtual 1-byte chunk `Array<u8, 1>` holding `[cur_byte]`!
        let arr1 = step_nb.emit("IDENTITY([0]:Array<u8, 1>)");
        let chunk1 = step_nb.emit(format!("UPDATE({arr1}, 0:u64, cur_byte)"));
        let ptr_alias = sa.indexed_gep_name.as_deref().unwrap_or(&sa.phi_name);
        let next_state = lower_chunked_body_with_virtual_slice(
            ctx,
            &mut step_nb,
            lp,
            &active_phis,
            mod_mem,
            ptr_alias,
            &chunk1,
            1,
            &mut k_env,
            &mut k_mem,
            &k_glob,
        )?;
        let out_acc = if next_state.len() == 1 {
            next_state[0].clone()
        } else {
            step_nb.emit(format!("TUPLE({})", next_state.join(", ")))
        };
        step_nb.ret_exprs = vec![out_acc];
        ctx.emitted_nodes.push(step_nb);

        let (inv_lbl, inv_body) = ctx.fresh_inv_label();
        nb.invariants.push((inv_lbl.clone(), inv_body));
        let fold_call = if b_args.is_empty() {
            format!(
                "FOLD_BOUNDED(@{step_name}, {slice_view}, init={init_acc}, invariant=@{inv_lbl})"
            )
        } else {
            format!(
                "FOLD_BOUNDED(@{step_name}, {slice_view}, {}, init={init_acc}, invariant=@{inv_lbl})",
                b_args.join(", ")
            )
        };
        let final_acc = nb.emit(fold_call);
        unpack_slice_exit_state(
            ctx,
            nb,
            lp,
            &active_phis,
            mod_mem,
            &final_acc,
            state_ports.len(),
            iv,
            sa,
            &end_off_port,
            env,
            mem_ports,
        );
        return Ok(());
    }

    // Multi-byte stride `S = sa.byte_stride > 1` (e.g. crc32_z S=8, adler32_z S=16):
    // Accumulator is Tuple<state..., chunk: Array<u8, S>, pos: u64>.
    let s_stride = sa.byte_stride;
    let zeros = vec!["0"; s_stride as usize].join(", ");
    let zero_chunk = nb.emit(format!("IDENTITY([{zeros}]:Array<u8, {s_stride}>)"));
    let mut full_ports = state_ports.clone();
    let mut full_tys = state_tys.clone();
    full_ports.push(zero_chunk);
    full_tys.push(format!("Array<u8, {s_stride}>"));
    full_ports.push("0:u64".to_string());
    full_tys.push("u64".to_string());
    let full_acc_ty = format!("Tuple<{}>", full_tys.join(", "));
    let init_full_acc = nb.emit(format!("TUPLE({})", full_ports.join(", ")));

    // Emit `@run_body` helper node that runs on `(state..., chunk: Array<u8, S>, b...) -> (Tuple<state..., Array<u8, S>, u64>)`.
    let run_name = ctx.fresh_node_name("chunk_body");
    let mut run_nb = NodeBuilder::new(run_name.clone());
    for (idx, ty) in state_tys.iter().enumerate() {
        run_nb.params.push((format!("st_{idx}"), ty.clone()));
    }
    run_nb
        .params
        .push(("chunk".to_string(), format!("Array<u8, {s_stride}>")));
    for (_, pname, pty) in &b_params {
        run_nb.params.push((pname.clone(), pty.clone()));
    }
    run_nb.results = vec![full_acc_ty.clone()];

    let mut r_env = BTreeMap::new();
    let mut r_mem = BTreeMap::new();
    let mut r_glob = BTreeMap::new();
    bind_fold_broadcasts(&b_params, env, &mut r_env, &mut r_mem, &mut r_glob);
    for (idx, phi) in active_phis.iter().enumerate() {
        let bits = phi.ty.bit_width().unwrap_or(64);
        r_env.insert(
            phi.name.clone(),
            VgVal::Scalar {
                port: format!("st_{idx}"),
                ty: state_tys[idx].clone(),
                bits,
            },
        );
    }
    for (idx, slot) in mod_mem.iter().enumerate() {
        r_mem.insert(slot.clone(), format!("st_{}", active_phis.len() + idx));
    }

    let next_state = lower_chunked_body_with_virtual_slice(
        ctx,
        &mut run_nb,
        lp,
        &active_phis,
        mod_mem,
        &sa.phi_name,
        "chunk",
        s_stride,
        &mut r_env,
        &mut r_mem,
        &r_glob,
    )?;
    let mut run_tuple_parts = next_state;
    run_tuple_parts.push("chunk".to_string());
    run_tuple_parts.push("0:u64".to_string());
    let run_tup = run_nb.emit(format!("TUPLE({})", run_tuple_parts.join(", ")));
    run_nb.ret_exprs = vec![run_tup];
    ctx.emitted_nodes.push(run_nb);

    // Emit `@collect_next` helper node: increments `pos + 1` and keeps `state...`, `chunk`.
    let next_name = ctx.fresh_node_name("chunk_next");
    let mut next_nb = NodeBuilder::new(next_name.clone());
    for (idx, ty) in state_tys.iter().enumerate() {
        next_nb.params.push((format!("st_{idx}"), ty.clone()));
    }
    next_nb
        .params
        .push(("chunk".to_string(), format!("Array<u8, {s_stride}>")));
    for (_, pname, pty) in &b_params {
        next_nb.params.push((pname.clone(), pty.clone()));
    }
    next_nb.params.push(("pos".to_string(), "u64".to_string()));
    next_nb.results = vec![full_acc_ty.clone()];
    let pos_plus_1 = next_nb.emit("ADD.wrap(pos, 1:u64)");
    let mut next_parts: Vec<String> = (0..state_tys.len()).map(|i| format!("st_{i}")).collect();
    next_parts.push("chunk".to_string());
    next_parts.push(pos_plus_1);
    let next_tup = next_nb.emit(format!("TUPLE({})", next_parts.join(", ")));
    next_nb.ret_exprs = vec![next_tup];
    ctx.emitted_nodes.push(next_nb);

    // Emit `@run_adapter` node with same signature as `@next_name` (takes extra `pos: u64` and ignores it).
    let adapter_name = ctx.fresh_node_name("chunk_run_adapt");
    let mut ad_nb = NodeBuilder::new(adapter_name.clone());
    ad_nb.params = ctx.emitted_nodes.last().unwrap().params.clone();
    ad_nb.results = vec![full_acc_ty.clone()];
    let mut call_args: Vec<String> = (0..state_tys.len()).map(|i| format!("st_{i}")).collect();
    call_args.push("chunk".to_string());
    for (_, pname, _) in &b_params {
        call_args.push(pname.clone());
    }
    let called = ad_nb.emit(format!("CALL_HASH(@{run_name}, {})", call_args.join(", ")));
    ad_nb.ret_exprs = vec![called];
    ctx.emitted_nodes.push(ad_nb);

    // Emit main FOLD_BOUNDED step kernel `(acc, byte, b...) -> acc`.
    let step_name = ctx.fresh_node_name("chunk_step");
    let mut step_nb = NodeBuilder::new(step_name.clone());
    step_nb
        .params
        .push(("acc".to_string(), full_acc_ty.clone()));
    step_nb.params.push(("byte".to_string(), "u8".to_string()));
    for (_, pname, pty) in &b_params {
        step_nb.params.push((pname.clone(), pty.clone()));
    }
    step_nb.results = vec![full_acc_ty.clone()];
    let mut st_ports = Vec::new();
    for j in 0..state_tys.len() {
        st_ports.push(step_nb.emit(format!("PROJ(acc, {j}:u32)")));
    }
    let old_chunk = step_nb.emit(format!("PROJ(acc, {}:u32)", state_tys.len()));
    let cur_pos = step_nb.emit(format!("PROJ(acc, {}:u32)", state_tys.len() + 1));
    let safe_pos = step_nb.emit(format!("MOD.wrap({cur_pos}, {s_stride}:u64)"));
    let new_chunk = step_nb.emit(format!("UPDATE({old_chunk}, {safe_pos}, byte)"));
    let is_last = step_nb.emit(format!("EQ({safe_pos}, {}:u64)", s_stride - 1));
    let mut guard_args = st_ports;
    guard_args.push(new_chunk);
    for (_, pname, _) in &b_params {
        guard_args.push(pname.clone());
    }
    guard_args.push(safe_pos);
    let out_acc = step_nb.emit(format!(
        "ASSERT_GUARD.branch({is_last}, @{adapter_name}, {}, else=@{next_name})",
        guard_args.join(", ")
    ));
    step_nb.ret_exprs = vec![out_acc];
    ctx.emitted_nodes.push(step_nb);

    let (inv_lbl, inv_body) = ctx.fresh_inv_label();
    nb.invariants.push((inv_lbl.clone(), inv_body));
    let fold_call = if b_args.is_empty() {
        format!(
            "FOLD_BOUNDED(@{step_name}, {slice_view}, init={init_full_acc}, invariant=@{inv_lbl})"
        )
    } else {
        format!(
            "FOLD_BOUNDED(@{step_name}, {slice_view}, {}, init={init_full_acc}, invariant=@{inv_lbl})",
            b_args.join(", ")
        )
    };
    let final_full_acc = nb.emit(fold_call);
    unpack_slice_exit_state(
        ctx,
        nb,
        lp,
        &active_phis,
        mod_mem,
        &final_full_acc,
        state_ports.len(),
        iv,
        sa,
        &end_off_port,
        env,
        mem_ports,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn lower_nested_slice_loop(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    outer_idx: usize,
    inner_idx: usize,
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> Result<(), Diagnostic> {
    let outer = ctx.loops[outer_idx].clone();
    let inner = ctx.loops[inner_idx].clone();
    if !inner.children.is_empty() {
        return Err(err("3-level nested loops not supported"));
    }

    let outer_phis = collect_loop_phis(ctx, &outer)?;
    let inner_phis = collect_loop_phis(ctx, &inner)?;
    let (outer_iv, outer_sa) = analyze_single_block_iv(ctx, &outer, &outer_phis)?;
    let (inner_iv, inner_sa) = analyze_single_block_iv(ctx, &inner, &inner_phis)?;

    let inner_const_k = match &inner_iv {
        IvKind::Down {
            init: ValueRef::ConstInt { value: k, .. },
            step: 1,
            ..
        } if *k > 0 => *k as u64,
        IvKind::Up {
            init: ValueRef::ConstInt { value: 0, .. },
            bound: ValueRef::ConstInt { value: k, .. },
            step: 1,
            ..
        } if *k > 0 => *k as u64,
        _ => return Err(err("inner nested loop must have constant trip count")),
    };

    let (Some(osa), Some(isa)) = (outer_sa, inner_sa) else {
        return Err(err("nested loop must walk a Slice<u8>"));
    };
    let s_stride = isa.byte_stride;
    if inner_const_k * s_stride != osa.byte_stride {
        return Err(err(format!(
            "nested loop byte stride mismatch: {} * {} != {}",
            inner_const_k, s_stride, osa.byte_stride
        )));
    }

    let slice_name = ctx.param_to_vg_name[&osa.slice_param_idx].clone();
    let start_ptr_val = resolve_value(ctx, nb, &osa.init_ptr, &LlvmType::Ptr, env, global_ports)?;
    let VgVal::Ptr {
        off_port: start_off_port,
        ..
    } = start_ptr_val
    else {
        return Err(err("outer slice init not a ptr"));
    };
    let trip_outer = compute_trip_count_port(ctx, nb, &outer_iv, env, global_ports)?;
    let total_bytes = nb.emit(format!("MUL.wrap({trip_outer}, {}:u64)", osa.byte_stride));
    let end_off_port = if start_off_port == "0:u64" {
        total_bytes.clone()
    } else {
        nb.emit(format!("ADD.wrap({start_off_port}, {total_bytes})"))
    };
    let slice_len = nb.emit(format!("LEN({slice_name})"));
    let clamped_end = emit_umin_u64(nb, &end_off_port, &slice_len);
    let clamped_start = if start_off_port == "0:u64" {
        "0:u64".to_string()
    } else {
        emit_umin_u64(nb, &start_off_port, &clamped_end)
    };
    let slice_view = nb.emit(format!(
        "SLICE({slice_name}, {clamped_start}, {clamped_end})"
    ));

    // State PHIs across outer loop (excluding slice ptr and outer IV):
    let outer_iv_name = match &outer_iv {
        IvKind::Up { iv_name, .. } | IvKind::Down { iv_name, .. } => iv_name.clone(),
    };
    let active_outer_phis: Vec<&LoopCarriedPhi> = outer_phis
        .iter()
        .filter(|p| p.name != osa.phi_name && p.name != outer_iv_name)
        .collect();

    let mut state_ports = Vec::new();
    let mut state_tys = Vec::new();
    for phi in &active_outer_phis {
        let rv = resolve_value(ctx, nb, &phi.init_val, &phi.ty, env, global_ports)?;
        let bits = phi.ty.bit_width().unwrap_or(64);
        let ty = format!("u{bits}");
        state_ports.push(coerce_to_type(nb, &rv, &ty, bits));
        state_tys.push(ty);
    }

    let zeros = vec!["0"; s_stride as usize].join(", ");
    let zero_chunk = nb.emit(format!("IDENTITY([{zeros}]:Array<u8, {s_stride}>)"));
    let mut full_ports = state_ports.clone();
    let mut full_tys = state_tys.clone();
    full_ports.push(zero_chunk);
    full_tys.push(format!("Array<u8, {s_stride}>"));
    full_ports.push("0:u64".to_string());
    full_tys.push("u64".to_string()); // pos within S
    full_ports.push("0:u64".to_string());
    full_tys.push("u64".to_string()); // inner_i within K
    let full_acc_ty = format!("Tuple<{}>", full_tys.join(", "));
    let init_full_acc = nb.emit(format!("TUPLE({})", full_ports.join(", ")));

    let (b_params, b_args) =
        collect_fold_broadcasts(ctx, &outer, &outer_phis, &[], env, mem_ports, global_ports);

    // Map each outer active PHI to the corresponding inner PHI (whose `init_val` names the outer PHI).
    let inner_iv_name = match &inner_iv {
        IvKind::Up { iv_name, .. } | IvKind::Down { iv_name, .. } => iv_name.clone(),
    };
    let active_inner_phis: Vec<&LoopCarriedPhi> = inner_phis
        .iter()
        .filter(|p| p.name != isa.phi_name && p.name != inner_iv_name)
        .collect();

    // 1. Emit `@inner_and_outer_step`: runs inner body on `(state..., chunk)` -> updated state,
    //    then if `inner_i == K - 1` runs outer latch epilogue (`MOD` etc.) and resets `inner_i = 0`.
    let inner_run_name = ctx.fresh_node_name("nested_inner");
    let mut ir_nb = NodeBuilder::new(inner_run_name.clone());
    for (idx, ty) in state_tys.iter().enumerate() {
        ir_nb.params.push((format!("st_{idx}"), ty.clone()));
    }
    ir_nb
        .params
        .push(("chunk".to_string(), format!("Array<u8, {s_stride}>")));
    for (_, pname, pty) in &b_params {
        ir_nb.params.push((pname.clone(), pty.clone()));
    }
    ir_nb.results = state_tys.clone();

    let mut r_env = BTreeMap::new();
    let mut r_mem = BTreeMap::new();
    let mut r_glob = BTreeMap::new();
    bind_fold_broadcasts(&b_params, env, &mut r_env, &mut r_mem, &mut r_glob);
    if !r_env.contains_key(&inner_iv_name) {
        let bits = match inner_iv {
            IvKind::Up { bits, .. } | IvKind::Down { bits, .. } => bits,
        };
        r_env.insert(
            inner_iv_name.clone(),
            VgVal::Scalar {
                port: emit_scalar_const(0, bits, &format!("u{bits}")),
                ty: format!("u{bits}"),
                bits,
            },
        );
    }
    for (idx, ophi) in active_outer_phis.iter().enumerate() {
        let bits = ophi.ty.bit_width().unwrap_or(64);
        r_env.insert(
            ophi.name.clone(),
            VgVal::Scalar {
                port: format!("st_{idx}"),
                ty: state_tys[idx].clone(),
                bits,
            },
        );
        // Also bind the matching inner phi!
        for iphi in &active_inner_phis {
            if iphi.init_val == ValueRef::Local(ophi.name.clone()) {
                r_env.insert(
                    iphi.name.clone(),
                    VgVal::Scalar {
                        port: format!("st_{idx}"),
                        ty: state_tys[idx].clone(),
                        bits,
                    },
                );
            }
        }
    }
    let next_inner_raw = lower_chunked_body_with_virtual_slice(
        ctx,
        &mut ir_nb,
        &inner,
        &active_inner_phis,
        &[],
        &isa.phi_name,
        "chunk",
        s_stride,
        &mut r_env,
        &mut r_mem,
        &r_glob,
    )?;
    // Reorder `next_inner_raw` (which is in `active_inner_phis` order) to match `active_outer_phis` order!
    let mut next_inner = Vec::with_capacity(active_outer_phis.len());
    for ophi in &active_outer_phis {
        let pos = active_inner_phis
            .iter()
            .position(|ip| ip.init_val == ValueRef::Local(ophi.name.clone()))
            .ok_or_else(|| {
                err(format!(
                    "nested loop outer phi %{} has no matching inner phi",
                    ophi.name
                ))
            })?;
        next_inner.push(next_inner_raw[pos].clone());
    }
    ir_nb.ret_exprs = next_inner;
    ctx.emitted_nodes.push(ir_nb);

    // 2. Emit `@outer_epilogue` node: runs outer latch epilogue on `(inner_next_state..., b...) -> Tuple<state..., chunk, 0, 0>`.
    let epi_name = ctx.fresh_node_name("nested_epi");
    let mut epi_nb = NodeBuilder::new(epi_name.clone());
    for (idx, ty) in state_tys.iter().enumerate() {
        epi_nb.params.push((format!("st_{idx}"), ty.clone()));
    }
    epi_nb
        .params
        .push(("chunk".to_string(), format!("Array<u8, {s_stride}>")));
    for (_, pname, pty) in &b_params {
        epi_nb.params.push((pname.clone(), pty.clone()));
    }
    epi_nb.results = vec![full_acc_ty.clone()];

    let mut e_env = BTreeMap::new();
    let mut e_mem = BTreeMap::new();
    let mut e_glob = BTreeMap::new();
    bind_fold_broadcasts(&b_params, env, &mut e_env, &mut e_mem, &mut e_glob);
    e_env.entry(outer_iv_name.clone()).or_insert_with(|| {
        let bits = match outer_iv {
            IvKind::Up { bits, .. } | IvKind::Down { bits, .. } => bits,
        };
        VgVal::Scalar {
            port: emit_scalar_const(0, bits, &format!("u{bits}")),
            ty: format!("u{bits}"),
            bits,
        }
    });
    e_env
        .entry(osa.phi_name.clone())
        .or_insert_with(|| VgVal::Ptr {
            base: PtrBase::Param(osa.slice_param_idx),
            off_port: "0:u64".to_string(),
        });

    // Bind every inner phi's `next_val` directly to the corresponding `st_{outer_idx}` (in case LCSSA phis were folded).
    for (oidx, ophi) in active_outer_phis.iter().enumerate() {
        if let Some(iphi) = active_inner_phis
            .iter()
            .find(|ip| ip.init_val == ValueRef::Local(ophi.name.clone()))
            && let ValueRef::Local(vn) = &iphi.next_val
        {
            let bits = ophi.ty.bit_width().unwrap_or(64);
            e_env.insert(
                vn.clone(),
                VgVal::Scalar {
                    port: format!("st_{oidx}"),
                    ty: state_tys[oidx].clone(),
                    bits,
                },
            );
        }
    }

    let latch_bb = ctx.bb_map[outer.latch.as_str()];
    for inst in &latch_bb.insts {
        if let InstKind::Phi { ty, incoming } = &inst.kind {
            let res = inst.result.as_ref().unwrap();
            let (v, _) = &incoming[0];
            if let ValueRef::Local(vn) = v
                && let Some(bound) = e_env.get(vn).cloned()
            {
                let _ = ty;
                e_env.insert(res.clone(), bound);
            }
        } else {
            lower_instruction(ctx, &mut epi_nb, inst, &mut e_env, &mut e_mem, &e_glob)?;
        }
    }
    let mut epi_parts = Vec::new();
    for (idx, ophi) in active_outer_phis.iter().enumerate() {
        let v = resolve_value(ctx, &mut epi_nb, &ophi.next_val, &ophi.ty, &e_env, &e_glob)?;
        let bits = ophi.ty.bit_width().unwrap_or(64);
        epi_parts.push(coerce_to_type(&mut epi_nb, &v, &state_tys[idx], bits));
    }
    epi_parts.push("chunk".to_string());
    epi_parts.push("0:u64".to_string());
    epi_parts.push("0:u64".to_string());
    let epi_tup = epi_nb.emit(format!("TUPLE({})", epi_parts.join(", ")));
    epi_nb.ret_exprs = vec![epi_tup];
    ctx.emitted_nodes.push(epi_nb);

    // 3. Emit `@no_epi` node: passes `(st..., chunk, b...)` with `pos=0, inner_i + 1`.
    let no_epi_name = ctx.fresh_node_name("nested_no_epi");
    let mut ne_nb = NodeBuilder::new(no_epi_name.clone());
    for (idx, ty) in state_tys.iter().enumerate() {
        ne_nb.params.push((format!("st_{idx}"), ty.clone()));
    }
    ne_nb
        .params
        .push(("chunk".to_string(), format!("Array<u8, {s_stride}>")));
    for (_, pname, pty) in &b_params {
        ne_nb.params.push((pname.clone(), pty.clone()));
    }
    ne_nb
        .params
        .push(("inner_i".to_string(), "u64".to_string()));
    ne_nb.results = vec![full_acc_ty.clone()];
    let inner_plus_1 = ne_nb.emit("ADD.wrap(inner_i, 1:u64)");
    let mut ne_parts: Vec<String> = (0..state_tys.len()).map(|i| format!("st_{i}")).collect();
    ne_parts.push("chunk".to_string());
    ne_parts.push("0:u64".to_string());
    ne_parts.push(inner_plus_1);
    let ne_tup = ne_nb.emit(format!("TUPLE({})", ne_parts.join(", ")));
    ne_nb.ret_exprs = vec![ne_tup];
    ctx.emitted_nodes.push(ne_nb);

    let epi_adapt_name = ctx.fresh_node_name("nested_epi_adapt");
    let mut ea_nb = NodeBuilder::new(epi_adapt_name.clone());
    ea_nb.params = ctx.emitted_nodes.last().unwrap().params.clone();
    ea_nb.results = vec![full_acc_ty.clone()];
    let mut ea_args: Vec<String> = (0..state_tys.len()).map(|i| format!("st_{i}")).collect();
    ea_args.push("chunk".to_string());
    for (_, pname, _) in &b_params {
        ea_args.push(pname.clone());
    }
    let ea_call = ea_nb.emit(format!("CALL_HASH(@{epi_name}, {})", ea_args.join(", ")));
    ea_nb.ret_exprs = vec![ea_call];
    ctx.emitted_nodes.push(ea_nb);

    // 4. Emit `@run_s_chunk`: runs `@inner_and_outer_step`, then dispatches `inner_i == K - 1`.
    let run_s_name = ctx.fresh_node_name("nested_run_s");
    let mut rs_nb = NodeBuilder::new(run_s_name.clone());
    for (idx, ty) in state_tys.iter().enumerate() {
        rs_nb.params.push((format!("st_{idx}"), ty.clone()));
    }
    rs_nb
        .params
        .push(("chunk".to_string(), format!("Array<u8, {s_stride}>")));
    for (_, pname, pty) in &b_params {
        rs_nb.params.push((pname.clone(), pty.clone()));
    }
    rs_nb.params.push(("pos".to_string(), "u64".to_string()));
    rs_nb
        .params
        .push(("inner_i".to_string(), "u64".to_string()));
    rs_nb.results = vec![full_acc_ty.clone()];
    let mut ir_args: Vec<String> = (0..state_tys.len()).map(|i| format!("st_{i}")).collect();
    ir_args.push("chunk".to_string());
    for (_, pname, _) in &b_params {
        ir_args.push(pname.clone());
    }
    let ir_outs = rs_nb.emit_multi(
        state_tys.len(),
        format!("CALL_HASH(@{inner_run_name}, {})", ir_args.join(", ")),
    );
    let is_last_inner = rs_nb.emit(format!("EQ(inner_i, {}:u64)", inner_const_k - 1));
    let mut guard2_args = ir_outs;
    guard2_args.push("chunk".to_string());
    for (_, pname, _) in &b_params {
        guard2_args.push(pname.clone());
    }
    guard2_args.push("inner_i".to_string());
    let rs_out = rs_nb.emit(format!(
        "ASSERT_GUARD.branch({is_last_inner}, @{epi_adapt_name}, {}, else=@{no_epi_name})",
        guard2_args.join(", ")
    ));
    rs_nb.ret_exprs = vec![rs_out];
    ctx.emitted_nodes.push(rs_nb);

    // 5. Emit `@collect_next` (pos < S - 1).
    let coll_name = ctx.fresh_node_name("nested_collect");
    let mut cn_nb = NodeBuilder::new(coll_name.clone());
    cn_nb.params = ctx.emitted_nodes.last().unwrap().params.clone();
    cn_nb.results = vec![full_acc_ty.clone()];
    let pos1 = cn_nb.emit("ADD.wrap(pos, 1:u64)");
    let mut cn_parts: Vec<String> = (0..state_tys.len()).map(|i| format!("st_{i}")).collect();
    cn_parts.push("chunk".to_string());
    cn_parts.push(pos1);
    cn_parts.push("inner_i".to_string());
    let cn_tup = cn_nb.emit(format!("TUPLE({})", cn_parts.join(", ")));
    cn_nb.ret_exprs = vec![cn_tup];
    ctx.emitted_nodes.push(cn_nb);

    // 6. Emit main `FOLD_BOUNDED` byte kernel.
    let step_name = ctx.fresh_node_name("nested_byte");
    let mut step_nb = NodeBuilder::new(step_name.clone());
    step_nb
        .params
        .push(("acc".to_string(), full_acc_ty.clone()));
    step_nb.params.push(("byte".to_string(), "u8".to_string()));
    for (_, pname, pty) in &b_params {
        step_nb.params.push((pname.clone(), pty.clone()));
    }
    step_nb.results = vec![full_acc_ty.clone()];
    let mut st_ports = Vec::new();
    for j in 0..state_tys.len() {
        st_ports.push(step_nb.emit(format!("PROJ(acc, {j}:u32)")));
    }
    let old_chunk = step_nb.emit(format!("PROJ(acc, {}:u32)", state_tys.len()));
    let cur_pos = step_nb.emit(format!("PROJ(acc, {}:u32)", state_tys.len() + 1));
    let cur_inner = step_nb.emit(format!("PROJ(acc, {}:u32)", state_tys.len() + 2));
    let safe_pos = step_nb.emit(format!("MOD.wrap({cur_pos}, {s_stride}:u64)"));
    let new_chunk = step_nb.emit(format!("UPDATE({old_chunk}, {safe_pos}, byte)"));
    let is_last_pos = step_nb.emit(format!("EQ({safe_pos}, {}:u64)", s_stride - 1));
    let mut g_args = st_ports;
    g_args.push(new_chunk);
    for (_, pname, _) in &b_params {
        g_args.push(pname.clone());
    }
    g_args.push(safe_pos);
    g_args.push(cur_inner);
    let out_acc = step_nb.emit(format!(
        "ASSERT_GUARD.branch({is_last_pos}, @{run_s_name}, {}, else=@{coll_name})",
        g_args.join(", ")
    ));
    step_nb.ret_exprs = vec![out_acc];
    ctx.emitted_nodes.push(step_nb);

    let (inv_lbl, inv_body) = ctx.fresh_inv_label();
    nb.invariants.push((inv_lbl.clone(), inv_body));
    let fold_call = if b_args.is_empty() {
        format!(
            "FOLD_BOUNDED(@{step_name}, {slice_view}, init={init_full_acc}, invariant=@{inv_lbl})"
        )
    } else {
        format!(
            "FOLD_BOUNDED(@{step_name}, {slice_view}, {}, init={init_full_acc}, invariant=@{inv_lbl})",
            b_args.join(", ")
        )
    };
    let final_full_acc = nb.emit(fold_call);
    unpack_slice_exit_state(
        ctx,
        nb,
        &outer,
        &active_outer_phis,
        &[],
        &final_full_acc,
        state_ports.len(),
        &outer_iv,
        &osa,
        &end_off_port,
        env,
        mem_ports,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_dynamic_index_fold(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    lp: &NaturalLoop,
    phis: &[LoopCarriedPhi],
    iv: &IvKind,
    mod_mem: &[MemSlot],
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> Result<(), Diagnostic> {
    let trip_port = compute_trip_count_port(ctx, nb, iv, env, global_ports)?;

    // Emit `@gen_idx(i: u64) -> (u64) { RETURN(i); }` + `GENERATE(@gen_idx, trip)` (requires ARENA).
    nb.effects.insert("ARENA".to_string());
    let gen_name = ctx.fresh_node_name("gen_idx");
    let mut gen_nb = NodeBuilder::new(gen_name.clone());
    gen_nb.params.push(("i".to_string(), "u64".to_string()));
    gen_nb.results.push("u64".to_string());
    gen_nb.ret_exprs = vec!["i".to_string()];
    ctx.emitted_nodes.push(gen_nb);

    let xs_port = nb.emit(format!("GENERATE(@{gen_name}, {trip_port})"));

    let mut state_ports = Vec::new();
    let mut state_tys = Vec::new();
    for phi in phis {
        let rv = resolve_value(ctx, nb, &phi.init_val, &phi.ty, env, global_ports)?;
        let bits = phi.ty.bit_width().unwrap_or(64);
        let ty = match &phi.ty {
            LlvmType::Ptr => "u64".to_string(),
            LlvmType::Int(1) => "Bool".to_string(),
            LlvmType::Int(b) => format!("u{b}"),
            _ => return Err(err("unsupported phi type")),
        };
        state_ports.push(coerce_to_type(nb, &rv, &ty, bits));
        state_tys.push(ty);
    }
    for slot in mod_mem {
        state_ports.push(mem_ports[slot].clone());
        state_tys.push(ctx.mem_specs[slot].vg_type.clone());
    }
    let (init_acc, acc_ty) = if state_ports.len() == 1 {
        (state_ports[0].clone(), state_tys[0].clone())
    } else {
        let ty = format!("Tuple<{}>", state_tys.join(", "));
        let p = nb.emit(format!("TUPLE({})", state_ports.join(", ")));
        (p, ty)
    };

    let (b_params, b_args) =
        collect_fold_broadcasts(ctx, lp, phis, mod_mem, env, mem_ports, global_ports);
    let step_name = ctx.fresh_node_name("dyn_step");
    let mut step_nb = NodeBuilder::new(step_name.clone());
    step_nb.params.push(("acc".to_string(), acc_ty.clone()));
    step_nb
        .params
        .push(("iter_idx".to_string(), "u64".to_string()));
    for (_, pname, pty) in &b_params {
        step_nb.params.push((pname.clone(), pty.clone()));
    }
    step_nb.results = vec![acc_ty.clone()];

    let mut k_env = BTreeMap::new();
    let mut k_mem = BTreeMap::new();
    let mut k_glob = BTreeMap::new();
    bind_fold_broadcasts(&b_params, env, &mut k_env, &mut k_mem, &mut k_glob);

    let unpacked: Vec<String> = if state_ports.len() == 1 {
        vec!["acc".to_string()]
    } else {
        (0..state_ports.len())
            .map(|j| step_nb.emit(format!("PROJ(acc, {j}:u32)")))
            .collect()
    };
    for (idx, phi) in phis.iter().enumerate() {
        let p = unpacked[idx].clone();
        let bits = phi.ty.bit_width().unwrap_or(64);
        k_env.insert(
            phi.name.clone(),
            VgVal::Scalar {
                port: p,
                ty: state_tys[idx].clone(),
                bits,
            },
        );
    }
    for (idx, slot) in mod_mem.iter().enumerate() {
        k_mem.insert(slot.clone(), unpacked[phis.len() + idx].clone());
    }

    let next_state = lower_loop_body_to_next_state(
        ctx,
        &mut step_nb,
        lp,
        phis,
        mod_mem,
        &mut k_env,
        &mut k_mem,
        &k_glob,
    )?;
    let out_acc = if next_state.len() == 1 {
        next_state[0].clone()
    } else {
        step_nb.emit(format!("TUPLE({})", next_state.join(", ")))
    };
    step_nb.ret_exprs = vec![out_acc];
    ctx.emitted_nodes.push(step_nb);

    let (inv_lbl, inv_body) = ctx.fresh_inv_label();
    nb.invariants.push((inv_lbl.clone(), inv_body));
    let fold_call = if b_args.is_empty() {
        format!("FOLD_BOUNDED(@{step_name}, {xs_port}, init={init_acc}, invariant=@{inv_lbl})")
    } else {
        format!(
            "FOLD_BOUNDED(@{step_name}, {xs_port}, {}, init={init_acc}, invariant=@{inv_lbl})",
            b_args.join(", ")
        )
    };
    let final_acc = nb.emit(fold_call);
    unpack_loop_exit_state(
        ctx,
        nb,
        lp,
        phis,
        mod_mem,
        &final_acc,
        state_ports.len(),
        iv,
        env,
        mem_ports,
    );
    Ok(())
}

fn compute_trip_count_port(
    ctx: &LiftCtx<'_>,
    nb: &mut NodeBuilder,
    iv: &IvKind,
    env: &BTreeMap<String, VgVal>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> Result<String, Diagnostic> {
    match iv {
        IvKind::Up {
            init,
            bound,
            step,
            bits,
            ..
        } => {
            let iv_v = resolve_value(ctx, nb, init, &LlvmType::Int(*bits), env, global_ports)?;
            let bv_v = resolve_value(ctx, nb, bound, &LlvmType::Int(*bits), env, global_ports)?;
            let iv64 = coerce_to_type(nb, &iv_v, "u64", 64);
            let bv64 = coerce_to_type(nb, &bv_v, "u64", 64);
            let has = nb.emit(format!("GT.u({bv64}, {iv64})"));
            let diff = nb.emit(format!("SUB.wrap({bv64}, {iv64})"));
            let raw = if *step == 1 {
                diff
            } else {
                let adj = nb.emit(format!("ADD.wrap({diff}, {}:u64)", step - 1));
                nb.emit(format!("DIV.wrap({adj}, {step}:u64)"))
            };
            Ok(nb.emit(format!("SELECT({has}, {raw}, 0:u64)")))
        }
        IvKind::Down {
            init, step, bits, ..
        } => {
            let iv_v = resolve_value(ctx, nb, init, &LlvmType::Int(*bits), env, global_ports)?;
            let iv64 = coerce_to_type(nb, &iv_v, "u64", 64);
            if *step == 1 {
                Ok(iv64)
            } else {
                Ok(nb.emit(format!("DIV.wrap({iv64}, {step}:u64)")))
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_loop_body_to_next_state(
    ctx: &mut LiftCtx<'_>,
    step_nb: &mut NodeBuilder,
    lp: &NaturalLoop,
    phis: &[LoopCarriedPhi],
    mod_mem: &[MemSlot],
    k_env: &mut BTreeMap<String, VgVal>,
    k_mem: &mut BTreeMap<MemSlot, String>,
    k_glob: &BTreeMap<String, (String, String)>,
) -> Result<Vec<String>, Diagnostic> {
    if lp.body.len() == 1 {
        let hdr = ctx.bb_map[lp.header.as_str()];
        for inst in &hdr.insts {
            if !matches!(inst.kind, InstKind::Phi { .. }) {
                lower_instruction(ctx, step_nb, inst, k_env, k_mem, k_glob)?;
            }
        }
    } else {
        // Multi-block loop body (e.g. mbedtls `21 -> {24, 45} -> 68`).
        let hdr = ctx.bb_map[lp.header.as_str()].clone();
        for inst in &hdr.insts {
            if !matches!(inst.kind, InstKind::Phi { .. }) {
                lower_instruction(ctx, step_nb, inst, k_env, k_mem, k_glob)?;
            }
        }
        match &hdr.term {
            Terminator::Br { target } => {
                lower_loop_acyclic_body(ctx, step_nb, target, lp, k_env, k_mem, k_glob)?;
            }
            Terminator::CondBr {
                cond,
                then_bb,
                else_bb,
            } => {
                let cv = resolve_value(ctx, step_nb, cond, &LlvmType::Int(1), k_env, k_glob)?;
                let cp = coerce_to_type(step_nb, &cv, "Bool", 1);
                let merge_lbl = find_merge_block(ctx, then_bb, else_bb, Some(&lp.latch))
                    .unwrap_or_else(|| lp.latch.clone());

                let (b_params, b_args) = collect_live_inputs(ctx, k_env, k_mem, k_glob);
                let mem_keys: Vec<MemSlot> = k_mem.keys().cloned().collect();
                let merge_bb = ctx.bb_map[merge_lbl.as_str()].clone();
                let mut merge_phis = Vec::new();
                for inst in &merge_bb.insts {
                    if let InstKind::Phi { ty, .. } = &inst.kind {
                        merge_phis.push((inst.result.clone().unwrap(), ty.clone()));
                    }
                }
                let mut out_tys: Vec<String> = merge_phis
                    .iter()
                    .map(|(_, ty)| match ty {
                        LlvmType::Int(1) => "Bool".to_string(),
                        LlvmType::Int(b) => format!("u{b}"),
                        _ => "u64".to_string(),
                    })
                    .collect();
                for slot in &mem_keys {
                    out_tys.push(ctx.mem_specs[slot].vg_type.clone());
                }
                let dummy = out_tys.is_empty();
                if dummy {
                    out_tys.push("Bool".to_string());
                }

                let then_name = ctx.fresh_node_name("lthen");
                let else_name = ctx.fresh_node_name("lelse");
                let then_nb = build_merge_branch_node(
                    ctx,
                    &then_name,
                    &lp.header,
                    then_bb,
                    &merge_lbl,
                    &b_params,
                    &out_tys,
                    &merge_phis,
                    &[],
                    &mem_keys,
                    dummy,
                    k_env,
                    k_mem,
                    k_glob,
                    &[],
                )?;
                let else_nb = build_merge_branch_node(
                    ctx,
                    &else_name,
                    &lp.header,
                    else_bb,
                    &merge_lbl,
                    &b_params,
                    &out_tys,
                    &merge_phis,
                    &[],
                    &mem_keys,
                    dummy,
                    k_env,
                    k_mem,
                    k_glob,
                    &[],
                )?;
                ctx.emitted_nodes.push(then_nb);
                ctx.emitted_nodes.push(else_nb);
                let res_ports = step_nb.emit_multi(
                    out_tys.len(),
                    format!(
                        "ASSERT_GUARD.branch({cp}, @{then_name}, {}, else=@{else_name})",
                        b_args.join(", ")
                    ),
                );
                let mut pos = 0;
                for (pname, pty) in &merge_phis {
                    let bits = pty.bit_width().unwrap_or(64);
                    k_env.insert(
                        pname.clone(),
                        VgVal::Scalar {
                            port: res_ports[pos].clone(),
                            ty: format!("u{bits}"),
                            bits,
                        },
                    );
                    pos += 1;
                }
                for slot in &mem_keys {
                    k_mem.insert(slot.clone(), res_ports[pos].clone());
                    pos += 1;
                }
                if merge_lbl == lp.latch {
                    for inst in &merge_bb.insts {
                        if !matches!(inst.kind, InstKind::Phi { .. }) {
                            lower_instruction(ctx, step_nb, inst, k_env, k_mem, k_glob)?;
                        }
                    }
                } else {
                    lower_loop_acyclic_body(ctx, step_nb, &merge_lbl, lp, k_env, k_mem, k_glob)?;
                }
            }
            _ => return Err(err("unexpected terminator in multi-block loop header")),
        }
    }

    let mut next_state = Vec::new();
    for phi in phis {
        let nv = resolve_value(ctx, step_nb, &phi.next_val, &phi.ty, k_env, k_glob)?;
        let bits = phi.ty.bit_width().unwrap_or(64);
        let target_ty = match &phi.ty {
            LlvmType::Ptr => "u64".to_string(),
            LlvmType::Int(1) => "Bool".to_string(),
            LlvmType::Int(b) => format!("u{b}"),
            _ => return Err(err("unsupported phi type")),
        };
        next_state.push(coerce_to_type(step_nb, &nv, &target_ty, bits));
    }
    for slot in mod_mem {
        next_state.push(k_mem[slot].clone());
    }
    Ok(next_state)
}

fn lower_loop_acyclic_body(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    start: &str,
    lp: &NaturalLoop,
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> Result<(), Diagnostic> {
    let mut cur = start.to_string();
    loop {
        let bb = ctx.bb_map[cur.as_str()].clone();
        for inst in &bb.insts {
            if !matches!(inst.kind, InstKind::Phi { .. }) {
                lower_instruction(ctx, nb, inst, env, mem_ports, global_ports)?;
            }
        }
        if cur == lp.latch {
            break;
        }
        match &bb.term {
            Terminator::Br { target } => cur = target.clone(),
            _ => return Err(err(format!("unexpected branch in loop body at %{cur}"))),
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn lower_chunked_body_with_virtual_slice(
    ctx: &mut LiftCtx<'_>,
    nb: &mut NodeBuilder,
    lp: &NaturalLoop,
    active_phis: &[&LoopCarriedPhi],
    mod_mem: &[MemSlot],
    slice_phi_name: &str,
    chunk_port: &str,
    chunk_len: u64,
    k_env: &mut BTreeMap<String, VgVal>,
    k_mem: &mut BTreeMap<MemSlot, String>,
    k_glob: &BTreeMap<String, (String, String)>,
) -> Result<Vec<String>, Diagnostic> {
    // Represent `slice_phi_name` temporarily as a special PtrBase `Alloca(usize::MAX)` mapped to `chunk_port`!
    let virt_id = usize::MAX;
    let virt_slot = MemSlot::Alloca(virt_id);
    k_env.insert(
        slice_phi_name.to_string(),
        VgVal::Ptr {
            base: PtrBase::Alloca(virt_id),
            off_port: "0:u64".to_string(),
        },
    );
    k_mem.insert(virt_slot.clone(), chunk_port.to_string());

    let hdr = ctx.bb_map[lp.header.as_str()].clone();
    // If `inst` is the GEP producing `slice_phi_name` (when GEP-indexed), skip re-lowering it!
    for inst in &hdr.insts {
        if matches!(inst.kind, InstKind::Phi { .. }) {
            continue;
        }
        if inst.result.as_deref() == Some(slice_phi_name) {
            continue;
        }
        // Intercept `Load` on `virt_id` directly as `INDEX(chunk_port, off_port)`!
        if let InstKind::Load { ty, ptr, .. } = &inst.kind
            && let Some(VgVal::Ptr {
                base: PtrBase::Alloca(aid),
                off_port,
            }) = resolve_ptr_val(ptr, k_env)
            && aid == virt_id
        {
            let bits = ty.bit_width().unwrap_or(8);
            let loaded = emit_load_from_array(nb, chunk_port, 8, chunk_len, &off_port, bits)?;
            k_env.insert(
                inst.result.clone().unwrap(),
                VgVal::Scalar {
                    port: loaded,
                    ty: format!("u{bits}"),
                    bits,
                },
            );
            continue;
        }
        let _ = chunk_len;
        lower_instruction(ctx, nb, inst, k_env, k_mem, k_glob)?;
    }

    let mut next_state = Vec::new();
    for phi in active_phis {
        let nv = resolve_value(ctx, nb, &phi.next_val, &phi.ty, k_env, k_glob)?;
        let bits = phi.ty.bit_width().unwrap_or(64);
        let ty = match &phi.ty {
            LlvmType::Ptr => "u64".to_string(),
            LlvmType::Int(1) => "Bool".to_string(),
            LlvmType::Int(b) => format!("u{b}"),
            _ => return Err(err("unsupported phi type")),
        };
        next_state.push(coerce_to_type(nb, &nv, &ty, bits));
    }
    for slot in mod_mem {
        next_state.push(k_mem[slot].clone());
    }
    Ok(next_state)
}

fn collect_fold_broadcasts(
    ctx: &LiftCtx<'_>,
    lp: &NaturalLoop,
    phis: &[LoopCarriedPhi],
    mod_mem: &[MemSlot],
    env: &BTreeMap<String, VgVal>,
    mem_ports: &BTreeMap<MemSlot, String>,
    global_ports: &BTreeMap<String, (String, String)>,
) -> (Vec<(BranchParam, String, String)>, Vec<String>) {
    // Collect any value in `env`, `mem_ports` (not in `mod_mem`), `global_ports`, and FixedArray params
    // that is actually referenced by instructions inside `lp.body`.
    let mut referenced = BTreeSet::new();
    for b in &lp.body {
        for inst in &ctx.bb_map[b.as_str()].insts {
            collect_inst_uses(inst, &mut referenced);
        }
    }
    let phi_names: BTreeSet<&str> = phis.iter().map(|p| p.name.as_str()).collect();

    let mut params = Vec::new();
    let mut args = Vec::new();
    let mut used = BTreeSet::new();

    for phi in phis {
        if phi.ty == LlvmType::Ptr
            && let Some(VgVal::Ptr {
                base: PtrBase::Param(pi),
                ..
            }) = resolve_ptr_val(&phi.init_val, env)
            && let ParamKind::FixedArray {
                elem_bits,
                len,
                writable: false,
            } = ctx.param_kinds[pi]
        {
            let arr_name = ctx.param_to_vg_name[&pi].clone();
            if used.insert(arr_name.clone()) {
                let ty = format!("Array<u{elem_bits}, {len}>");
                params.push((
                    BranchParam {
                        key: BranchKey::SsaScalar(format!("__ro_arr_{pi}")),
                        param_name: arr_name.clone(),
                        bits: elem_bits,
                    },
                    arr_name.clone(),
                    ty,
                ));
                args.push(arr_name);
            }
        }
    }

    for (name, val) in env {
        if phi_names.contains(name.as_str()) {
            continue;
        }
        if !referenced.contains(name) {
            continue;
        }
        let pname = sanitize_ident(name, params.len(), &mut used);
        match val {
            VgVal::Scalar { port, ty, bits } => {
                params.push((
                    BranchParam {
                        key: BranchKey::SsaScalar(name.clone()),
                        param_name: pname.clone(),
                        bits: *bits,
                    },
                    pname,
                    ty.clone(),
                ));
                args.push(port.clone());
            }
            VgVal::Ptr { base, off_port } => {
                if let PtrBase::Param(pi) = base
                    && let ParamKind::FixedArray {
                        elem_bits,
                        len,
                        writable: false,
                    } = ctx.param_kinds[*pi]
                {
                    let arr_name = ctx.param_to_vg_name[pi].clone();
                    if used.insert(arr_name.clone()) {
                        let ty = format!("Array<u{elem_bits}, {len}>");
                        params.push((
                            BranchParam {
                                key: BranchKey::SsaScalar(format!("__ro_arr_{pi}")),
                                param_name: arr_name.clone(),
                                bits: elem_bits,
                            },
                            arr_name.clone(),
                            ty,
                        ));
                        args.push(arr_name);
                    }
                }
                let p_off = sanitize_ident(&format!("off_{name}"), params.len(), &mut used);
                params.push((
                    BranchParam {
                        key: BranchKey::SsaPtr {
                            name: name.clone(),
                            base: base.clone(),
                        },
                        param_name: p_off.clone(),
                        bits: 64,
                    },
                    p_off,
                    "u64".to_string(),
                ));
                args.push(off_port.clone());
            }
        }
    }

    for (slot, port) in mem_ports {
        if mod_mem.contains(slot) {
            continue;
        }
        let spec = &ctx.mem_specs[slot];
        let base_name = match slot {
            MemSlot::Alloca(id) => format!("mem_a{id}"),
            MemSlot::WritableParam(pi) => ctx.param_to_vg_name[pi].clone(),
        };
        let pname = sanitize_ident(&base_name, params.len(), &mut used);
        params.push((
            BranchParam {
                key: BranchKey::Mem(slot.clone()),
                param_name: pname.clone(),
                bits: spec.elem_bits,
            },
            pname,
            spec.vg_type.clone(),
        ));
        args.push(port.clone());
    }

    for (gname, (port, gty)) in global_ports {
        let pname = sanitize_ident(&format!("g_{gname}"), params.len(), &mut used);
        params.push((
            BranchParam {
                key: BranchKey::Global(gname.clone()),
                param_name: pname.clone(),
                bits: 32,
            },
            pname,
            gty.clone(),
        ));
        args.push(port.clone());
    }

    (params, args)
}

fn bind_fold_broadcasts(
    b_params: &[(BranchParam, String, String)],
    _orig_env: &BTreeMap<String, VgVal>,
    k_env: &mut BTreeMap<String, VgVal>,
    k_mem: &mut BTreeMap<MemSlot, String>,
    k_glob: &mut BTreeMap<String, (String, String)>,
) {
    for (bp, pname, ty) in b_params {
        match &bp.key {
            BranchKey::SsaScalar(name) => {
                if !name.starts_with("__ro_arr_") {
                    k_env.insert(
                        name.clone(),
                        VgVal::Scalar {
                            port: pname.clone(),
                            ty: ty.clone(),
                            bits: bp.bits,
                        },
                    );
                }
            }
            BranchKey::SsaPtr { name, base } => {
                k_env.insert(
                    name.clone(),
                    VgVal::Ptr {
                        base: base.clone(),
                        off_port: pname.clone(),
                    },
                );
            }
            BranchKey::Mem(slot) => {
                k_mem.insert(slot.clone(), pname.clone());
            }
            BranchKey::Global(gname) => {
                k_glob.insert(gname.clone(), (pname.clone(), ty.clone()));
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn unpack_loop_exit_state(
    ctx: &LiftCtx<'_>,
    nb: &mut NodeBuilder,
    lp: &NaturalLoop,
    phis: &[LoopCarriedPhi],
    mod_mem: &[MemSlot],
    final_acc: &str,
    total_state_len: usize,
    iv: &IvKind,
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
) {
    let unpacked: Vec<String> = if total_state_len == 1 {
        vec![final_acc.to_string()]
    } else {
        (0..total_state_len)
            .map(|j| nb.emit(format!("PROJ({final_acc}, {j}:u32)")))
            .collect()
    };
    let mut pos = 0;
    for phi in phis {
        let p = unpacked[pos].clone();
        pos += 1;
        match &phi.ty {
            LlvmType::Ptr => {
                let base = resolve_ptr_val(&phi.init_val, env)
                    .and_then(|v| {
                        if let VgVal::Ptr { base, .. } = v {
                            Some(base)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(PtrBase::Null);
                env.insert(phi.name.clone(), VgVal::Ptr { base, off_port: p });
            }
            LlvmType::Int(b) => {
                env.insert(
                    phi.name.clone(),
                    VgVal::Scalar {
                        port: p,
                        ty: if *b == 1 {
                            "Bool".to_string()
                        } else {
                            format!("u{b}")
                        },
                        bits: *b,
                    },
                );
            }
            _ => {}
        }
    }
    for slot in mod_mem {
        mem_ports.insert(slot.clone(), unpacked[pos].clone());
        pos += 1;
    }
    bind_lcssa_exit_phis(ctx, nb, lp, phis, iv, None, None, env);
}

#[allow(clippy::too_many_arguments)]
fn unpack_slice_exit_state(
    ctx: &LiftCtx<'_>,
    nb: &mut NodeBuilder,
    lp: &NaturalLoop,
    active_phis: &[&LoopCarriedPhi],
    mod_mem: &[MemSlot],
    final_acc: &str,
    state_len: usize,
    iv: &IvKind,
    sa: &SliceAdvance,
    end_off_port: &str,
    env: &mut BTreeMap<String, VgVal>,
    mem_ports: &mut BTreeMap<MemSlot, String>,
) {
    let unpacked: Vec<String> = if state_len == 1 && sa.byte_stride == 1 {
        vec![final_acc.to_string()]
    } else {
        (0..state_len)
            .map(|j| nb.emit(format!("PROJ({final_acc}, {j}:u32)")))
            .collect()
    };
    for (idx, phi) in active_phis.iter().enumerate() {
        let bits = phi.ty.bit_width().unwrap_or(64);
        env.insert(
            phi.name.clone(),
            VgVal::Scalar {
                port: unpacked[idx].clone(),
                ty: if bits == 1 {
                    "Bool".to_string()
                } else {
                    format!("u{bits}")
                },
                bits,
            },
        );
    }
    for (idx, slot) in mod_mem.iter().enumerate() {
        mem_ports.insert(slot.clone(), unpacked[active_phis.len() + idx].clone());
    }
    if sa.indexed_gep_name.is_none() {
        env.insert(
            sa.phi_name.clone(),
            VgVal::Ptr {
                base: PtrBase::Param(sa.slice_param_idx),
                off_port: end_off_port.to_string(),
            },
        );
    }
    let owned_phis: Vec<LoopCarriedPhi> = active_phis.iter().map(|p| (*p).clone()).collect();
    bind_lcssa_exit_phis(
        ctx,
        nb,
        lp,
        &owned_phis,
        iv,
        Some(sa),
        Some(end_off_port),
        env,
    );
}

#[allow(clippy::too_many_arguments)]
fn bind_lcssa_exit_phis(
    ctx: &LiftCtx<'_>,
    nb: &mut NodeBuilder,
    lp: &NaturalLoop,
    phis: &[LoopCarriedPhi],
    iv: &IvKind,
    sa: Option<&SliceAdvance>,
    end_off_port: Option<&str>,
    env: &mut BTreeMap<String, VgVal>,
) {
    // Map every phi's `next_val` to the final accumulator value so LCSSA phis in `exit_block` resolve.
    for phi in phis {
        if let ValueRef::Local(next_name) = &phi.next_val
            && let Some(final_val) = env.get(&phi.name).cloned()
        {
            env.insert(next_name.clone(), final_val);
        }
    }
    // Also update any LCSSA exit values for IV (`rem_next`, `ptr_next`) in `lp.exit_block`.
    if let (Some(sa), Some(end_off)) = (sa, end_off_port)
        && let Some(latch_bb) = ctx.bb_map.get(lp.latch.as_str())
    {
        for inst in &latch_bb.insts {
            if let InstKind::Gep {
                ptr: ValueRef::Local(p),
                ..
            } = &inst.kind
                && p == &sa.phi_name
                && let Some(r) = &inst.result
            {
                env.insert(
                    r.clone(),
                    VgVal::Ptr {
                        base: PtrBase::Param(sa.slice_param_idx),
                        off_port: end_off.to_string(),
                    },
                );
            }
        }
    }

    match iv {
        IvKind::Down {
            iv_name,
            init,
            step,
            bits,
        } => {
            // Find `iv_next` in latch:
            let latch_bb = ctx.bb_map[lp.latch.as_str()];
            for inst in &latch_bb.insts {
                if let InstKind::BinOp {
                    op: BinOpKind::Add,
                    lhs: ValueRef::Local(l),
                    ..
                } = &inst.kind
                    && l == iv_name
                    && let Some(iv_next) = &inst.result
                {
                    let rem_port = if *step == 1 {
                        emit_scalar_const(0, *bits, &format!("u{bits}"))
                    } else if let Ok(iv_v) =
                        resolve_value(ctx, nb, init, &LlvmType::Int(*bits), env, &BTreeMap::new())
                    {
                        let (p, _) = coerce_to_unsigned(nb, &iv_v);
                        nb.emit(format!("MOD.wrap({p}, {step}:u{bits})"))
                    } else {
                        emit_scalar_const(0, *bits, &format!("u{bits}"))
                    };
                    env.insert(
                        iv_next.clone(),
                        VgVal::Scalar {
                            port: rem_port,
                            ty: format!("u{bits}"),
                            bits: *bits,
                        },
                    );
                }
            }
        }
        IvKind::Up { .. } => {}
    }

    // Bind any 1-pred LCSSA Phis in `lp.exit_block` now while `env` has the exit values!
    let exit_bb = ctx.bb_map[lp.exit_block.as_str()];
    for inst in &exit_bb.insts {
        if let InstKind::Phi { ty, incoming } = &inst.kind
            && incoming.len() == 1
        {
            let res = inst.result.as_ref().unwrap();
            if let Ok(v) = resolve_value(ctx, nb, &incoming[0].0, ty, env, &BTreeMap::new()) {
                env.insert(res.clone(), v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vg_interp::{NoEffectsHost, run};

    fn test_prov(func: &str) -> Provenance {
        Provenance {
            path: "test.c".to_string(),
            function: func.to_string(),
            language: Language::C,
            compiler: "clang".to_string(),
            compiler_args: vec![],
            opt: "opt".to_string(),
            opt_args: vec![],
        }
    }

    #[test]
    fn lifts_straight_line_and_loop_c_snippets() {
        if std::env::var("VG_REQUIRE_ALIVE2").as_deref() != Ok("1")
            && (!crate::verify::has_alive_tv() || !crate::ingest::has_opt())
        {
            eprintln!("SKIP: alive-tv or opt not installed on this host");
            return;
        }
        let dir = std::env::temp_dir().join("vg_omnilift_lift_unit_1");
        let _ = std::fs::create_dir_all(&dir);
        let c_path = dir.join("ops.c");
        std::fs::write(
            &c_path,
            r#"
#include <stdint.h>
#include <stddef.h>

uint32_t bitmix(uint32_t a, uint32_t b) {
    return ((a << 5) ^ (b >> 3)) + (a & 0xffu);
}

uint64_t sum_bytes(const uint8_t *buf, size_t len) {
    uint64_t s = 0;
    for (size_t i = 0; i < len; i++) {
        s += buf[i];
    }
    return s;
}
"#,
        )
        .unwrap();

        let l1 = lift_source(&c_path, "bitmix", Language::C, &[]).unwrap();
        assert!(!l1.returns_result);
        let mut h = NoEffectsHost;
        let r = run(
            &l1.nodes.last().unwrap().node,
            &[Value::Int(0x1234), Value::Int(0x5678)],
            &|_| None,
            &mut h,
        )
        .unwrap();
        let expected = ((0x1234u32 << 5) ^ (0x5678u32 >> 3)).wrapping_add(0x1234u32 & 0xff);
        assert_eq!(r, vec![Value::Int(i128::from(expected))]);

        let l2 = lift_source(&c_path, "sum_bytes", Language::C, &[]).unwrap();
        let map: BTreeMap<Hash, vg_core::ir::Node> = l2
            .nodes
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let data = vec![
            Value::Int(10),
            Value::Int(20),
            Value::Int(30),
            Value::Int(5),
        ];
        let r2 = run(
            &l2.nodes.last().unwrap().node,
            &[Value::Seq(data)],
            &|hh| map.get(hh).cloned(),
            &mut h,
        )
        .unwrap();
        assert_eq!(r2, vec![Value::Int(65)]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cures_signed_overflow_div_zero_shift_overflow_and_oob() {
        if std::env::var("VG_REQUIRE_ALIVE2").as_deref() != Ok("1")
            && (!crate::verify::has_alive_tv() || !crate::ingest::has_opt())
        {
            eprintln!("SKIP: alive-tv or opt not installed on this host");
            return;
        }
        let dir = std::env::temp_dir().join("vg_omnilift_lift_ub");
        let _ = std::fs::create_dir_all(&dir);
        let c_path = dir.join("ub.c");
        std::fs::write(
            &c_path,
            r#"
#include <stdint.h>
#include <stddef.h>

int add_signed(int a, int b) { return a + b; }
unsigned int div_u(unsigned int a, unsigned int b) { return a / b; }
unsigned int shl_u(unsigned int a, unsigned int s) { return a << s; }
uint8_t get_at(const uint8_t *p, size_t idx) { return p[idx]; }
"#,
        )
        .unwrap();

        let mut h = NoEffectsHost;

        let l_add = lift_source(&c_path, "add_signed", Language::C, &[]).unwrap();
        assert!(l_add.returns_result);
        assert!(l_add.ub_found.iter().any(|u| u.kind == "signed_overflow"));
        let m_add: BTreeMap<_, _> = l_add
            .nodes
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let root_add = &l_add.nodes.last().unwrap().node;
        assert_eq!(
            run(
                root_add,
                &[Value::Int(10), Value::Int(20)],
                &|hh| m_add.get(hh).cloned(),
                &mut h
            )
            .unwrap(),
            vec![Value::Ok(Box::new(Value::Int(30)))]
        );
        assert!(matches!(
            run(
                root_add,
                &[Value::Int(i128::from(i32::MAX)), Value::Int(1)],
                &|hh| m_add.get(hh).cloned(),
                &mut h
            )
            .unwrap()[0],
            Value::Err(_)
        ));

        let l_div = lift_source(&c_path, "div_u", Language::C, &[]).unwrap();
        assert!(l_div.returns_result);
        assert!(l_div.ub_found.iter().any(|u| u.kind == "div_by_zero"));
        let m_div: BTreeMap<_, _> = l_div
            .nodes
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let root_div = &l_div.nodes.last().unwrap().node;
        assert_eq!(
            run(
                root_div,
                &[Value::Int(42), Value::Int(6)],
                &|hh| m_div.get(hh).cloned(),
                &mut h
            )
            .unwrap(),
            vec![Value::Ok(Box::new(Value::Int(7)))]
        );
        assert!(matches!(
            run(
                root_div,
                &[Value::Int(42), Value::Int(0)],
                &|hh| m_div.get(hh).cloned(),
                &mut h
            )
            .unwrap()[0],
            Value::Err(_)
        ));

        let l_shl = lift_source(&c_path, "shl_u", Language::C, &[]).unwrap();
        assert!(l_shl.returns_result);
        assert!(l_shl.ub_found.iter().any(|u| u.kind == "shift_overflow"));
        let m_shl: BTreeMap<_, _> = l_shl
            .nodes
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let root_shl = &l_shl.nodes.last().unwrap().node;
        assert_eq!(
            run(
                root_shl,
                &[Value::Int(1), Value::Int(4)],
                &|hh| m_shl.get(hh).cloned(),
                &mut h
            )
            .unwrap(),
            vec![Value::Ok(Box::new(Value::Int(16)))]
        );
        assert!(matches!(
            run(
                root_shl,
                &[Value::Int(1), Value::Int(32)],
                &|hh| m_shl.get(hh).cloned(),
                &mut h
            )
            .unwrap()[0],
            Value::Err(_)
        ));

        let l_oob = lift_source(&c_path, "get_at", Language::C, &[]).unwrap();
        assert!(l_oob.returns_result);
        assert!(l_oob.ub_found.iter().any(|u| u.kind == "out_of_bounds"));
        let m_oob: BTreeMap<_, _> = l_oob
            .nodes
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let root_oob = &l_oob.nodes.last().unwrap().node;
        let slice = Value::Seq(vec![Value::Int(11), Value::Int(22), Value::Int(33)]);
        assert_eq!(
            run(
                root_oob,
                &[slice.clone(), Value::Int(1)],
                &|hh| m_oob.get(hh).cloned(),
                &mut h
            )
            .unwrap(),
            vec![Value::Ok(Box::new(Value::Int(22)))]
        );
        assert!(matches!(
            run(
                root_oob,
                &[slice, Value::Int(5)],
                &|hh| m_oob.get(hh).cloned(),
                &mut h
            )
            .unwrap()[0],
            Value::Err(_)
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_recursion_and_pointer_to_pointer_with_vg_l0001() {
        let rec_ir = r#"
define i32 @fact(i32 %n) {
entry:
  %c = icmp eq i32 %n, 0
  br i1 %c, label %base, label %step
base:
  ret i32 1
step:
  %m = sub i32 %n, 1
  %r = call i32 @fact(i32 %m)
  %p = mul i32 %n, %r
  ret i32 %p
}
"#;
        assert_eq!(
            lift_llvm_ir(rec_ir, "fact", test_prov("fact"))
                .unwrap_err()
                .code,
            "VG-L0001"
        );

        let p2p_ir = r#"
define i32 @deref2(ptr %pp) {
entry:
  %p = load ptr, ptr %pp, align 8
  %v = load i32, ptr %p, align 4
  ret i32 %v
}
"#;
        assert_eq!(
            lift_llvm_ir(p2p_ir, "deref2", test_prov("deref2"))
                .unwrap_err()
                .code,
            "VG-L0001"
        );
    }

    #[test]
    fn smoke_lifts_all_four_targets_and_matches_canonical_vectors() {
        if std::env::var("VG_REQUIRE_ALIVE2").as_deref() != Ok("1")
            && (!crate::verify::has_alive_tv() || !crate::ingest::has_opt())
        {
            eprintln!("SKIP: alive-tv or opt not installed on this host");
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let mut h = NoEffectsHost;

        eprintln!("[smoke] 1. lifting crc32_z...");
        // 1. zlib crc32_z: "123456789" -> 0xcbf43926
        let crc = lift_source(
            &root.join("tests/lift/zlib/crc32.c"),
            "crc32_z",
            Language::C,
            &[],
        )
        .unwrap();
        assert!(!crc.returns_result);
        let crc_map: BTreeMap<_, _> = crc
            .nodes
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let crc_root = &crc.nodes.last().unwrap().node;
        let vec_123 = Value::Seq(
            b"123456789"
                .iter()
                .map(|&b| Value::Int(i128::from(b)))
                .collect(),
        );
        let r_crc = run(
            crc_root,
            &[Value::Int(0), vec_123],
            &|hh| crc_map.get(hh).cloned(),
            &mut h,
        )
        .unwrap();
        assert_eq!(r_crc, vec![Value::Int(0xcbf4_3926)]);
        eprintln!("[smoke] 1. crc32_z OK");

        eprintln!("[smoke] 2. lifting adler32_z...");
        // 2. zlib adler32_z: "Wikipedia" -> 0x11e60398, plus 6000-byte NMAX buffer
        let adler = lift_source(
            &root.join("tests/lift/zlib/adler32.c"),
            "adler32_z",
            Language::C,
            &[],
        )
        .unwrap();
        eprintln!("[smoke] 2. adler32_z lifted OK");
        assert!(!adler.returns_result);
        let adler_map: BTreeMap<_, _> = adler
            .nodes
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let adler_root = &adler.nodes.last().unwrap().node;
        let vec_wiki = Value::Seq(
            b"Wikipedia"
                .iter()
                .map(|&b| Value::Int(i128::from(b)))
                .collect(),
        );
        let r_adler = run(
            adler_root,
            &[Value::Int(1), vec_wiki],
            &|hh| adler_map.get(hh).cloned(),
            &mut h,
        )
        .unwrap();
        assert_eq!(r_adler, vec![Value::Int(0x11e6_0398)]);

        let big_buf = Value::Seq((0..6000).map(|i| Value::Int((i % 251) as i128)).collect());
        let r_big = run(
            adler_root,
            &[Value::Int(1), big_buf],
            &|hh| adler_map.get(hh).cloned(),
            &mut h,
        )
        .unwrap();
        // Verify against plain Rust adler32 calculation:
        let (mut s1, mut s2) = (1u32, 0u32);
        for i in 0..6000 {
            s1 = (s1 + (i % 251) as u32) % 65521;
            s2 = (s2 + s1) % 65521;
        }
        assert_eq!(r_big, vec![Value::Int(i128::from((s2 << 16) | s1))]);

        // 3 & 4. Mbed TLS and Rust sha2 on FIPS 180-4 "abc" padded block
        let init_state_vals: Vec<Value> = [
            0x6a09e667u32,
            0xbb67ae85,
            0x3c6ef372,
            0xa54ff53a,
            0x510e527f,
            0x9b05688c,
            0x1f83d9ab,
            0x5be0cd19,
        ]
        .into_iter()
        .map(|w| Value::Int(i128::from(w)))
        .collect();
        let mut abc_block = [0u8; 64];
        abc_block[0] = b'a';
        abc_block[1] = b'b';
        abc_block[2] = b'c';
        abc_block[3] = 0x80;
        abc_block[63] = 24;
        let block_vals: Vec<Value> = abc_block
            .iter()
            .map(|&b| Value::Int(i128::from(b)))
            .collect();
        let want_abc: Vec<Value> = [
            0xba7816bfu32,
            0x8f01cfea,
            0x414140de,
            0x5dae2223,
            0xb00361a3,
            0x96177a9c,
            0xb410ff61,
            0xf20015ad,
        ]
        .into_iter()
        .map(|w| Value::Int(i128::from(w)))
        .collect();

        eprintln!("[smoke] 3. lifting mbedtls...");
        let mbed = lift_source(
            &root.join("tests/lift/mbedtls/sha256_shim.c"),
            "mbedtls_sha256_compress",
            Language::C,
            &[],
        )
        .unwrap();
        eprintln!("[smoke] 3. mbedtls lifted OK");
        let mbed_map: BTreeMap<_, _> = mbed
            .nodes
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let r_mbed = run(
            &mbed.nodes.last().unwrap().node,
            &[
                Value::Seq(init_state_vals.clone()),
                Value::Seq(block_vals.clone()),
            ],
            &|hh| mbed_map.get(hh).cloned(),
            &mut h,
        )
        .unwrap();
        assert_eq!(r_mbed, vec![Value::Seq(want_abc.clone())]);

        eprintln!("[smoke] 4. lifting rust-sha2...");
        let rsha = lift_source(
            &root.join("tests/lift/rust-sha2/lib.rs"),
            "compress256",
            Language::Rust,
            &[],
        )
        .unwrap();
        eprintln!("[smoke] 4. rust-sha2 lifted OK");
        let rsha_map: BTreeMap<_, _> = rsha
            .nodes
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let r_rust = run(
            &rsha.nodes.last().unwrap().node,
            &[Value::Seq(init_state_vals), Value::Seq(block_vals)],
            &|hh| rsha_map.get(hh).cloned(),
            &mut h,
        )
        .unwrap();
        assert_eq!(r_rust, vec![Value::Seq(want_abc)]);
    }
}
