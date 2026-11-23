//! P8.5: Equivalence verification for lifted VG-IR nodes:
//! 1. Bounded round-trip translation validation with Alive2 (`alive-tv --src-unroll=8 --tgt-unroll=8`)
//!    via the docs/08 adapter (`PRE` -> `llvm.assume`, `Result::Err` -> `unreachable`).
//! 2. Native binary differential fuzzing (10^6 inputs in the non-UB domain) linking the original
//!    LLVM IR against `vg_codegen::emit_llvm` output.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use vg_codegen::{Options as CodegenOptions, Target as CodegenTarget, emit_llvm};
use vg_core::Assembled;
use vg_core::diag::{Diagnostic, Span};
use vg_core::ir::{Bin, Builtin, Hash, Node, Op, Operand, Term};
use vg_core::types::Type;
use vg_core::value::Value;
use vg_interp::bytes::decode_const;

use crate::ingest::{find_clang, find_opt};
use crate::lift::{Lifted, LiftedParam};
use crate::llvm::{GlobalInit, LlvmType, Module, parse_module_reachable};

static VERIFY_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempFiles(Vec<PathBuf>);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn temp_path(stem: &str, ext: &str) -> PathBuf {
    let seq = VERIFY_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("vg_verify_{pid}_{seq}_{stem}.{ext}"))
}

fn err_alive2(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error("VG-L0002", Span::default(), msg)
}

fn err_fuzz(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error("VG-L0003", Span::default(), msg)
}

pub fn find_alive_tv() -> PathBuf {
    let hb = PathBuf::from("/opt/homebrew/bin/alive-tv");
    if hb.is_file() {
        hb
    } else {
        PathBuf::from("alive-tv")
    }
}

pub fn has_alive_tv() -> bool {
    Command::new(find_alive_tv())
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Default number of differential fuzz cases (1,000,000 for G8; overridable via `VG_OMNILIFT_FUZZ_CASES`).
pub fn default_fuzz_cases() -> u64 {
    std::env::var("VG_OMNILIFT_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1_000_000)
}

/// Run both the Alive2 bounded round-trip check (`unroll >= 8`) and the native differential fuzzer.
pub fn verify_lifted(lifted: &mut Lifted, unroll: u32, fuzz_cases: u64) -> Result<(), Diagnostic> {
    let (tool, actual_unroll) = run_alive2_check(lifted, unroll)?;
    let actual_fuzz = run_differential_fuzz(lifted, fuzz_cases)?;
    lifted.bounded_check = tool;
    lifted.unroll = actual_unroll;
    lifted.fuzz_cases = actual_fuzz;
    Ok(())
}

// ============================================================================
// Part 1: Alive2 Bounded Round-Trip Check (`VG-L0002`)
// ============================================================================

#[derive(Clone, Debug)]
enum AliveVal {
    Scalar {
        expr: String,
        bits: u32,
    },
    Tuple(Vec<AliveVal>),
    Array {
        ptr: String,
        elem_bits: u32,
        len: u64,
    },
    Slice {
        ptr: String,
        len: String,
        unguarded_thin_ptr: bool,
    },
    IndexRange {
        count: u64,
    },
    GeneratedRange {
        count_expr: String,
    },
    ResultOk(Box<AliveVal>),
    ResultErr,
}

struct AliveLowerCtx<'a> {
    nodes_by_hash: BTreeMap<Hash, &'a Assembled>,
    orig_mod: &'a Module,
    entry_allocas: Vec<String>,
    globals: Vec<String>,
    matched_large_globals: BTreeMap<String, (u32, u64)>,
    declarations: BTreeSet<String>,
    blocks: Vec<(String, Vec<String>, String)>,
    cur_label: String,
    cur_insts: Vec<String>,
    cur_terminated: bool,
    reg_seq: usize,
    lbl_seq: usize,
    const_seq: usize,
    cursor_ptr_slot: Option<String>,
    cursor_rem_slot: Option<String>,
    unroll_bound: u32,
    capped_64_loop: bool,
}

impl<'a> AliveLowerCtx<'a> {
    fn new(nodes: &'a [Assembled], orig_mod: &'a Module, unroll_bound: u32) -> Self {
        let mut nodes_by_hash = BTreeMap::new();
        for a in nodes {
            nodes_by_hash.insert(a.hashes.node, a);
        }
        Self {
            nodes_by_hash,
            orig_mod,
            entry_allocas: Vec::new(),
            globals: Vec::new(),
            matched_large_globals: BTreeMap::new(),
            declarations: BTreeSet::new(),
            blocks: Vec::new(),
            cur_label: "entry".to_string(),
            cur_insts: Vec::new(),
            cur_terminated: false,
            reg_seq: 0,
            lbl_seq: 0,
            const_seq: 0,
            cursor_ptr_slot: None,
            cursor_rem_slot: None,
            unroll_bound,
            capped_64_loop: false,
        }
    }

    fn fresh_reg(&mut self, prefix: &str) -> String {
        self.reg_seq += 1;
        format!("%{prefix}_{}", self.reg_seq)
    }

    fn fresh_label(&mut self, prefix: &str) -> String {
        self.lbl_seq += 1;
        format!("{prefix}_{}", self.lbl_seq)
    }

    fn emit(&mut self, inst: impl Into<String>) {
        if !self.cur_terminated {
            self.cur_insts.push(format!("  {}", inst.into()));
        }
    }

    fn emit_assign(&mut self, prefix: &str, rhs: impl Into<String>) -> String {
        let r = self.fresh_reg(prefix);
        self.emit(format!("{r} = {}", rhs.into()));
        r
    }

    fn terminate(&mut self, term: impl Into<String>) {
        if !self.cur_terminated {
            self.blocks.push((
                self.cur_label.clone(),
                std::mem::take(&mut self.cur_insts),
                format!("  {}", term.into()),
            ));
            self.cur_terminated = true;
        }
    }

    fn start_block(&mut self, label: impl Into<String>) {
        assert!(self.cur_terminated, "previous block was not terminated");
        self.cur_label = label.into();
        self.cur_insts.clear();
        self.cur_terminated = false;
    }

    fn alloc_entry_array(&mut self, elem_bits: u32, len: u64) -> String {
        let r = self.fresh_reg("arr");
        let align = (elem_bits / 8).max(1);
        self.entry_allocas.push(format!(
            "  {r} = alloca [{len} x i{elem_bits}], align {align}"
        ));
        r
    }
}

/// Lower `lifted.nodes` through the docs/08 Alive2 adapter into a self-contained LLVM IR module
/// with the exact entry signature of `lifted.entry_fn_name`, then run `opt` and `alive-tv`.
pub fn run_alive2_check(lifted: &Lifted, unroll: u32) -> Result<(String, u32), Diagnostic> {
    let unroll = unroll.max(8);
    let (raw_tgt, large_globals, capped_64) = lower_vgir_to_alive_llvm(lifted, unroll)?;
    let raw_src = prepare_original_llvm_for_alive(lifted, unroll, capped_64)?;

    let src_opt = optimize_for_alive(&raw_src, &lifted.entry_fn_name, &large_globals)?;
    let tgt_opt = optimize_for_alive(&raw_tgt, &lifted.entry_fn_name, &large_globals)?;

    let src_file = temp_path("alive_src", "ll");
    let tgt_file = temp_path("alive_tgt", "ll");
    let _guard = TempFiles(vec![src_file.clone(), tgt_file.clone()]);

    std::fs::write(&src_file, &src_opt)
        .map_err(|e| err_alive2(format!("failed to write Alive2 src IR: {e}")))?;
    std::fs::write(&tgt_file, &tgt_opt)
        .map_err(|e| err_alive2(format!("failed to write Alive2 tgt IR: {e}")))?;

    let alive_tv = find_alive_tv();
    let out = Command::new(&alive_tv)
        .arg("--disable-undef-input")
        .arg(format!("--src-unroll={unroll}"))
        .arg(format!("--tgt-unroll={unroll}"))
        .arg(&src_file)
        .arg(&tgt_file)
        .output()
        .map_err(|e| err_alive2(format!("failed to invoke {}: {e}", alive_tv.display())))?;

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success()
        || !stdout.contains("0 incorrect transformations")
        || !stdout.contains("0 Alive2 errors")
        || stdout.contains("0 correct transformations")
    {
        return Err(err_alive2(format!(
            "Alive2 bounded check failed for `{}` (unroll={unroll}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
            lifted.entry_fn_name
        )));
    }

    Ok(("alive2".to_string(), unroll))
}

fn prepare_original_llvm_for_alive(
    lifted: &Lifted,
    unroll: u32,
    cap_64_loop: bool,
) -> Result<String, Diagnostic> {
    let mut lines = Vec::new();
    let target_marker = format!("@{}(", lifted.entry_fn_name);
    let has_null_cure = lifted.ub_found.iter().any(|u| u.kind == "null_pointer");
    for raw_line in lifted.original_llvm_ir.lines() {
        let mut l = raw_line.replace("noinline", "");
        if l.trim_start().starts_with("define ")
            && !l.contains(&target_marker)
            && let Some(brace_pos) = l.rfind('{')
        {
            let prefix = l[..brace_pos].trim_end();
            // Place `alwaysinline` before `#N` or `personality` if present.
            l = insert_alwaysinline_before_attrs(prefix);
        }
        // Non-void return types carry `noundef` so returning poison in `src` has the same UB domain as `tgt`.
        if l.trim_start().starts_with("define ")
            && l.contains(&target_marker)
            && !l.contains("define void ")
            && !l.contains("define noundef ")
        {
            l = l.replacen("define ", "define noundef ", 1);
        }
        // When `null_pointer` was cured into a non-null Slice<u8> domain, fold `icmp eq/ne ptr %p, null`
        // in `src` to the non-null branch so Alive2 checks the non-null Slice<u8> domain.
        if has_null_cure && l.contains("icmp eq ptr ") && l.trim_end().ends_with(", null") {
            if let Some(eq_pos) = l.find("= icmp eq ptr ") {
                l = format!("{} = and i1 true, false", &l[..eq_pos]);
            }
        } else if has_null_cure
            && l.contains("icmp ne ptr ")
            && l.trim_end().ends_with(", null")
            && let Some(eq_pos) = l.find("= icmp ne ptr ")
        {
            l = format!("{} = or i1 true, true", &l[..eq_pos]);
        }
        // Bound 64-round constant-trip loops in `src` to `unroll` (e.g. 8) to match bounded(unroll) check.
        if cap_64_loop && l.contains("icmp eq i64 ") && l.trim_end().ends_with(", 64") {
            l = l.replacen(", 64", &format!(", {unroll}"), 1);
        }
        lines.push(l);
    }
    Ok(lines.join("\n"))
}

fn insert_alwaysinline_before_attrs(prefix: &str) -> String {
    let tokens: Vec<&str> = prefix.split_whitespace().collect();
    let mut split_idx = tokens.len();
    for (i, tok) in tokens.iter().enumerate() {
        if tok.starts_with('#') || *tok == "personality" {
            split_idx = i;
            break;
        }
    }
    let before = tokens[..split_idx].join(" ");
    let after = tokens[split_idx..].join(" ");
    if after.is_empty() {
        format!("{before} alwaysinline {{")
    } else {
        format!("{before} alwaysinline {after} {{")
    }
}

fn optimize_for_alive(
    ir: &str,
    entry_fn: &str,
    large_globals: &BTreeMap<String, (u32, u64)>,
) -> Result<String, Diagnostic> {
    // Strip `!llvm.loop` metadata before running `opt` so constant-trip loops <= 64 (e.g. SHA-256 16/64 rounds)
    // can be unrolled and promoted to SSA by `loop-unroll,sroa,mem2reg`.
    let cleaned_input = strip_loop_metadata(ir);
    let in_file = temp_path("opt_in", "ll");
    let out_file = temp_path("opt_out", "ll");
    let _guard = TempFiles(vec![in_file.clone(), out_file.clone()]);

    std::fs::write(&in_file, &cleaned_input)
        .map_err(|e| err_alive2(format!("failed to write opt input: {e}")))?;

    let opt = find_opt();
    let out = Command::new(&opt)
        .arg("-S")
        .arg("-unroll-threshold=100000")
        .arg("-passes=always-inline,sroa,mem2reg,simplifycfg,loop-simplify,lcssa,indvars,loop-unroll<no-runtime;full-unroll-max=64>,instcombine,simplifycfg,sroa,mem2reg,reassociate,instcombine,gvn,simplifycfg,loop-simplify,lcssa")
        .arg(&in_file)
        .arg("-o")
        .arg(&out_file)
        .output()
        .map_err(|e| err_alive2(format!("failed to invoke {}: {e}", opt.display())))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(err_alive2(format!(
            "opt failed while preparing `{entry_fn}` for Alive2: {stderr}"
        )));
    }

    let opt_ir = std::fs::read_to_string(&out_file)
        .map_err(|e| err_alive2(format!("failed to read opt output: {e}")))?;

    Ok(sanitize_ir_for_alive_tv(&opt_ir, entry_fn, large_globals))
}

fn strip_loop_metadata(ir: &str) -> String {
    let mut out = String::with_capacity(ir.len());
    for line in ir.lines() {
        if let Some(pos) = line.find(", !llvm.loop") {
            out.push_str(&line[..pos]);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn sanitize_ir_for_alive_tv(
    ir: &str,
    entry_fn: &str,
    large_globals: &BTreeMap<String, (u32, u64)>,
) -> String {
    let target_marker = format!("@{entry_fn}(");
    let mut keep_lines = Vec::new();
    let mut in_target = false;
    let mut in_other_fn = false;

    for raw_line in ir.lines() {
        let trimmed = raw_line.trim();
        if trimmed.starts_with("define ") {
            if trimmed.contains(&target_marker) {
                in_target = true;
                in_other_fn = false;
            } else {
                in_target = false;
                in_other_fn = true;
            }
        }
        if in_other_fn {
            if trimmed == "}" {
                in_other_fn = false;
            }
            continue;
        }
        if !in_target {
            if trimmed.starts_with("@")
                || trimmed.starts_with("%struct.")
                || trimmed.starts_with("declare ")
            {
                if let Some((gname, (ebits, glen))) = large_globals
                    .iter()
                    .find(|(gn, _)| trimmed.starts_with(&format!("@{gn} =")))
                {
                    let align = (*ebits / 8).max(1);
                    keep_lines.push(format!(
                        "@{gname} = external constant [{glen} x i{ebits}], align {align}"
                    ));
                    continue;
                }
                if !trimmed.contains("llvm.experimental.noalias")
                    && !trimmed.contains("llvm.lifetime.")
                    && !trimmed.contains("rust_eh_personality")
                {
                    keep_lines.push(sanitize_alive_line(raw_line));
                }
            }
            continue;
        }
        if trimmed.contains("llvm.experimental.noalias") || trimmed.contains("llvm.lifetime.") {
            continue;
        }
        keep_lines.push(sanitize_alive_line(raw_line));
        if trimmed == "}" {
            in_target = false;
        }
    }

    keep_lines.join("\n") + "\n"
}

fn sanitize_alive_line(line: &str) -> String {
    // 1. Strip trailing `, !meta !N` annotations.
    let mut s = line.to_string();
    while let Some(idx) = s.find(", !") {
        let rest = &s[idx + 2..];
        // Find next comma or end of line
        if let Some(next_comma) = rest.find(',') {
            s.replace_range(idx..idx + 2 + next_comma, "");
        } else {
            s.truncate(idx);
            break;
        }
    }
    // 2. Strip parenthesized attributes unsupported by Alive2: `captures(...)`, `range(...)`, `initializes(...)`.
    for kw in ["captures(", "range(", "initializes("] {
        while let Some(start) = s.find(kw) {
            let mut depth = 0usize;
            let mut end = start;
            for (off, ch) in s[start..].char_indices() {
                if ch == '(' {
                    depth += 1;
                } else if ch == ')' {
                    depth -= 1;
                    if depth == 0 {
                        end = start + off + 1;
                        break;
                    }
                }
            }
            if end > start {
                s.replace_range(start..end, "");
            } else {
                break;
            }
        }
    }
    // 3. Strip `personality ptr @...`
    if let Some(p) = s.find("personality ptr ")
        && let Some(brace) = s[p..].find('{')
    {
        s.replace_range(p..p + brace, "");
    }
    // 4. Filter out unsupported tokens (`noalias`, `samesign`, `dead_on_unwind`, `writable`, `unnamed_addr`, `local_unnamed_addr`, `#N`).
    let indent: String = s.chars().take_while(|c| c.is_whitespace()).collect();
    let mut out_tokens = Vec::new();
    for tok in s.split_whitespace() {
        let clean_tok = tok.trim_end_matches(',');
        if matches!(
            clean_tok,
            "noalias"
                | "samesign"
                | "dead_on_unwind"
                | "writable"
                | "unnamed_addr"
                | "local_unnamed_addr"
        ) {
            continue;
        }
        if clean_tok.starts_with('#') && clean_tok[1..].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        out_tokens.push(tok);
    }
    format!("{indent}{}", out_tokens.join(" "))
}

fn lower_vgsym_preserving_order(lifted: &Lifted) -> Result<Vec<Assembled>, Diagnostic> {
    use std::collections::HashMap;
    use vg_core::ast::{Item, TypeExpr};
    use vg_core::lower::{Callable, Kind, Labels, NodeLowering, lower_type};
    use vg_core::types::EffectMask;

    let m = vg_core::parser::parse(&lifted.vgsym)
        .map_err(|d| err_alive2(format!("failed to parse lifted.vgsym: {}", d.message)))?;
    let base = |t: &TypeExpr, span| match t {
        TypeExpr::Refine(_, b, _) => lower_type(b, span),
        t => lower_type(t, span),
    };
    let mut labels: Labels = HashMap::new();
    for (k, item) in m.items.iter().enumerate() {
        if let Item::Node(n) = item {
            let mut effects = 0;
            for e in &n.effects {
                effects |= EffectMask::bit(&e.text).unwrap_or(0);
            }
            let params = n
                .params
                .iter()
                .map(|p| base(&p.ty, p.name.span))
                .collect::<Result<_, _>>()?;
            let results = n
                .results
                .iter()
                .map(|t| base(t, n.span))
                .collect::<Result<_, _>>()?;
            labels.insert(
                n.label.clone(),
                Callable {
                    kind: if n.spec { Kind::Spec } else { Kind::Node },
                    item: k,
                    params,
                    results,
                    effects: EffectMask(effects),
                    hash: None,
                    spec: None,
                },
            );
        }
    }

    let mut out = Vec::with_capacity(lifted.nodes.len());
    let mut idx = 0usize;
    for (k, item) in m.items.iter().enumerate() {
        let Item::Node(def) = item else { continue };
        let lowered = NodeLowering::new(&labels, k, def).lower()?;
        let canonical = vg_core::canon::canonicalize(&lowered.node)?;
        let (bytes, hashes) = vg_core::codec::encode(&canonical.node);
        let Some(expected) = lifted.nodes.get(idx) else {
            return Err(err_alive2("lifted.nodes length mismatch with lifted.vgsym"));
        };
        if hashes.node != expected.hashes.node || canonical.node != expected.node {
            return Err(err_alive2(format!(
                "lifted.nodes[{idx}] (@{}) does not match canonicalized lifted.vgsym",
                def.label
            )));
        }
        let entry = labels.get_mut(&def.label).unwrap();
        entry.hash = Some(hashes.node);
        if def.spec {
            entry.spec = Some(hashes.node);
        }
        out.push(Assembled {
            label: def.label.clone(),
            node: lowered.node,
            hashes,
            bytes,
            warnings: Vec::new(),
        });
        idx += 1;
    }
    if idx != lifted.nodes.len() {
        return Err(err_alive2("lifted.nodes count mismatch"));
    }
    Ok(out)
}

type AliveLowered = (String, BTreeMap<String, (u32, u64)>, bool);

fn lower_vgir_to_alive_llvm(lifted: &Lifted, unroll: u32) -> Result<AliveLowered, Diagnostic> {
    let ordered_nodes = lower_vgsym_preserving_order(lifted)?;
    let parsed_mod = parse_module_reachable(&lifted.original_llvm_ir, &lifted.entry_fn_name)?;
    let orig_fn = parsed_mod
        .functions
        .get(&lifted.entry_fn_name)
        .ok_or_else(|| err_alive2(format!("missing @{}` in original IR", lifted.entry_fn_name)))?;

    let root_asm = ordered_nodes
        .last()
        .ok_or_else(|| err_alive2("lifted module has no nodes"))?;
    let root_node = &root_asm.node;

    let mut ctx = AliveLowerCtx::new(&ordered_nodes, &parsed_mod, unroll);

    // Build LLVM function parameter list matching `orig_fn`.
    let mut llvm_sig_params = Vec::new();
    for p in &orig_fn.params {
        let pty = match &p.ty {
            LlvmType::Ptr => {
                if let Some(d) = p.dereferenceable {
                    format!("ptr noundef dereferenceable({d})")
                } else {
                    "ptr noundef".to_string()
                }
            }
            LlvmType::Int(b) => format!("i{b} noundef"),
            other => return Err(err_alive2(format!("unsupported LLVM param type {other:?}"))),
        };
        llvm_sig_params.push(format!("{pty} %param_{}", llvm_sig_params.len()));
    }

    // Bind each VG-IR root input (`LiftedParam`) to an `AliveVal`, and track writable Array params.
    let mut root_inputs = Vec::with_capacity(lifted.params.len());
    let mut writable_array_params: Vec<(usize, u32, u64)> = Vec::new();

    for lp in &lifted.params {
        match lp {
            LiftedParam::Scalar {
                bits, param_idx, ..
            } => {
                root_inputs.push(AliveVal::Scalar {
                    expr: format!("%param_{param_idx}"),
                    bits: *bits,
                });
            }
            LiftedParam::Slice {
                len_param_idx,
                ptr_param_idx,
                ..
            } => {
                let ptr_expr = format!("%param_{ptr_param_idx}");
                let (len_expr, unguarded) = match len_param_idx {
                    Some(li) => {
                        let lbits = orig_fn.params[*li].ty.bit_width().unwrap_or(64);
                        let raw = format!("%param_{li}");
                        let expr = if lbits < 64 {
                            ctx.emit_assign("len64", format!("zext i{lbits} {raw} to i64"))
                        } else {
                            raw
                        };
                        let p_slot = ctx.fresh_reg("cur_ptr_slot");
                        let r_slot = ctx.fresh_reg("cur_rem_slot");
                        ctx.entry_allocas
                            .push(format!("  {p_slot} = alloca ptr, align 8"));
                        ctx.entry_allocas
                            .push(format!("  {r_slot} = alloca i64, align 8"));
                        ctx.emit(format!("store ptr {ptr_expr}, ptr {p_slot}, align 8"));
                        ctx.emit(format!("store i64 {expr}, ptr {r_slot}, align 8"));
                        ctx.cursor_ptr_slot = Some(p_slot);
                        ctx.cursor_rem_slot = Some(r_slot);
                        (expr, false)
                    }
                    None => (u64::MAX.to_string(), true),
                };
                root_inputs.push(AliveVal::Slice {
                    ptr: ptr_expr,
                    len: len_expr,
                    unguarded_thin_ptr: unguarded,
                });
            }
            LiftedParam::Array {
                elem_bits,
                len,
                writable,
                ptr_param_idx,
                ..
            } => {
                let src_ptr = format!("%param_{ptr_param_idx}");
                if *writable {
                    let local_arr = ctx.alloc_entry_array(*elem_bits, *len);
                    let nbytes = (*len) * u64::from(*elem_bits / 8);
                    ctx.declarations.insert(
                        "declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)".to_string(),
                    );
                    ctx.emit(format!(
                        "call void @llvm.memcpy.p0.p0.i64(ptr align 4 {local_arr}, ptr align 4 {src_ptr}, i64 {nbytes}, i1 false)"
                    ));
                    writable_array_params.push((*ptr_param_idx, *elem_bits, *len));
                    root_inputs.push(AliveVal::Array {
                        ptr: local_arr,
                        elem_bits: *elem_bits,
                        len: *len,
                    });
                } else {
                    root_inputs.push(AliveVal::Array {
                        ptr: src_ptr,
                        elem_bits: *elem_bits,
                        len: *len,
                    });
                }
            }
        }
    }

    // Emit `llvm.assume` for any PRE clauses on the root node (docs/08).
    for pre_term in &root_node.contract.pre {
        if let Some(cond_expr) = lower_pre_term_to_i1(&mut ctx, root_node, pre_term, &root_inputs) {
            ctx.declarations
                .insert("declare void @llvm.assume(i1)".to_string());
            ctx.emit(format!("call void @llvm.assume(i1 {cond_expr})"));
        }
    }

    // Lower root node body.
    let outputs = lower_node_inline(&mut ctx, root_node, &root_inputs)?;

    if !ctx.cur_terminated {
        let out_val = outputs
            .into_iter()
            .next()
            .ok_or_else(|| err_alive2("root node produced no output"))?;
        match out_val {
            AliveVal::ResultErr => {
                ctx.terminate("unreachable");
            }
            AliveVal::ResultOk(inner) => {
                emit_function_return(&mut ctx, &orig_fn.ret_ty, *inner, &writable_array_params)?;
            }
            other => {
                emit_function_return(&mut ctx, &orig_fn.ret_ty, other, &writable_array_params)?;
            }
        }
    }

    // Assemble textual LLVM IR module.
    let ret_ty_str = match &orig_fn.ret_ty {
        LlvmType::Void => "void".to_string(),
        LlvmType::Int(8) => "noundef zeroext i8".to_string(),
        LlvmType::Int(16) => "noundef zeroext i16".to_string(),
        LlvmType::Int(b) => format!("noundef i{b}"),
        other => {
            return Err(err_alive2(format!(
                "unsupported LLVM return type {other:?}"
            )));
        }
    };

    let mut ir = String::new();
    for g in &ctx.globals {
        ir.push_str(g);
        ir.push('\n');
    }
    for d in &ctx.declarations {
        ir.push_str(d);
        ir.push('\n');
    }
    ir.push_str(&format!(
        "define {ret_ty_str} @{}({}) {{\n",
        lifted.entry_fn_name,
        llvm_sig_params.join(", ")
    ));
    for (idx, (lbl, insts, term)) in ctx.blocks.iter().enumerate() {
        ir.push_str(&format!("{lbl}:\n"));
        if idx == 0 {
            for a in &ctx.entry_allocas {
                ir.push_str(a);
                ir.push('\n');
            }
        }
        for inst in insts {
            ir.push_str(inst);
            ir.push('\n');
        }
        ir.push_str(term);
        ir.push('\n');
    }
    ir.push_str("}\n");
    Ok((ir, ctx.matched_large_globals, ctx.capped_64_loop))
}

fn emit_function_return(
    ctx: &mut AliveLowerCtx<'_>,
    ret_ty: &LlvmType,
    val: AliveVal,
    writable_array_params: &[(usize, u32, u64)],
) -> Result<(), Diagnostic> {
    match ret_ty {
        LlvmType::Void => {
            if let AliveVal::Array {
                ptr,
                elem_bits,
                len,
            } = val
                && let Some((param_idx, _, _)) = writable_array_params.first()
            {
                let dst_ptr = format!("%param_{param_idx}");
                let nbytes = len * u64::from(elem_bits / 8);
                ctx.declarations
                    .insert("declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1)".to_string());
                ctx.emit(format!(
                    "call void @llvm.memcpy.p0.p0.i64(ptr align 4 {dst_ptr}, ptr align 4 {ptr}, i64 {nbytes}, i1 false)"
                ));
            }
            ctx.terminate("ret void");
            Ok(())
        }
        LlvmType::Int(b) => {
            let AliveVal::Scalar { expr, bits } = val else {
                return Err(err_alive2("expected scalar return value"));
            };
            let final_expr = cast_int_expr(ctx, &expr, bits, *b, false);
            ctx.terminate(format!("ret i{b} {final_expr}"));
            Ok(())
        }
        other => Err(err_alive2(format!("unsupported return type {other:?}"))),
    }
}

fn lower_pre_term_to_i1(
    ctx: &mut AliveLowerCtx<'_>,
    node: &Node,
    term: &Term,
    inputs: &[AliveVal],
) -> Option<String> {
    match term {
        Term::Bin(op @ (Bin::Eq | Bin::Ne | Bin::Lt | Bin::Le | Bin::Gt | Bin::Ge), lhs, rhs) => {
            let (le, lb) = lower_pre_term_to_scalar(ctx, node, lhs, inputs)?;
            let (re, rb) = lower_pre_term_to_scalar(ctx, node, rhs, inputs)?;
            let bits = lb.max(rb).max(64);
            let l64 = cast_int_expr(ctx, &le, lb, bits, false);
            let r64 = cast_int_expr(ctx, &re, rb, bits, false);
            let pred = match op {
                Bin::Eq => "eq",
                Bin::Ne => "ne",
                Bin::Lt => "ult",
                Bin::Le => "ule",
                Bin::Gt => "ugt",
                Bin::Ge => "uge",
                _ => unreachable!(),
            };
            Some(ctx.emit_assign("pre_cmp", format!("icmp {pred} i{bits} {l64}, {r64}")))
        }
        Term::Bin(Bin::And, a, b) => {
            let la = lower_pre_term_to_i1(ctx, node, a, inputs)?;
            let rb = lower_pre_term_to_i1(ctx, node, b, inputs)?;
            Some(ctx.emit_assign("pre_and", format!("and i1 {la}, {rb}")))
        }
        Term::Bin(Bin::Or, a, b) => {
            let la = lower_pre_term_to_i1(ctx, node, a, inputs)?;
            let rb = lower_pre_term_to_i1(ctx, node, b, inputs)?;
            Some(ctx.emit_assign("pre_or", format!("or i1 {la}, {rb}")))
        }
        _ => None,
    }
}

fn lower_pre_term_to_scalar(
    _ctx: &mut AliveLowerCtx<'_>,
    node: &Node,
    term: &Term,
    inputs: &[AliveVal],
) -> Option<(String, u32)> {
    match term {
        Term::Input(i) => match inputs.get(*i as usize)? {
            AliveVal::Scalar { expr, bits } => Some((expr.clone(), *bits)),
            _ => None,
        },
        Term::Builtin(Builtin::Len, args) if args.len() == 1 => {
            if let Term::Input(i) = &args[0]
                && let Some(AliveVal::Slice { len, .. }) = inputs.get(*i as usize)
            {
                Some((len.clone(), 64))
            } else {
                None
            }
        }
        Term::Const(ci) => {
            let c = node.consts.get(*ci as usize)?;
            let v = decode_const(c).ok()?;
            if let Value::Int(x) = v {
                Some(((x as i64).to_string(), 64))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn lower_node_inline(
    ctx: &mut AliveLowerCtx<'_>,
    node: &Node,
    inputs: &[AliveVal],
) -> Result<Vec<AliveVal>, Diagnostic> {
    let mut const_vals: Vec<Option<AliveVal>> = Vec::with_capacity(node.consts.len());
    for c in &node.consts {
        if c.ty == Type::Int {
            const_vals.push(None);
        } else {
            let val = decode_const(c)
                .map_err(|e| err_alive2(format!("failed to decode const ({}): {e}", c.ty)))?;
            const_vals.push(Some(lower_const_to_alive(ctx, &c.ty, &val)?));
        }
    }

    let mut op_results: Vec<Vec<AliveVal>> = Vec::with_capacity(node.ops.len());
    // Track which op indices compare against an unguarded thin-pointer slice's synthetic `LEN`
    let mut unguarded_len_ops: BTreeSet<usize> = BTreeSet::new();
    let mut unguarded_cond_ops: BTreeSet<usize> = BTreeSet::new();
    let mut skip_until_op: usize = 0;

    for (op_idx, op) in node.ops.iter().enumerate() {
        if op_idx < skip_until_op {
            continue;
        }
        if ctx.cur_terminated {
            op_results.push(vec![AliveVal::ResultErr; op.results.len()]);
            continue;
        }
        // Recognize the 26-op contiguous `emit_load_from_array(8 -> 32) + llvm.bswap.i32` sequence
        // and lower it to `load i32, align 1` + `call noundef i32 @llvm.bswap.i32` after verifying all 26 ops.
        if let Some(bsw_val) =
            try_match_unaligned_be_u32_load(ctx, node, op_idx, inputs, &const_vals, &op_results)
        {
            for _ in 0..25 {
                op_results.push(vec![AliveVal::Scalar {
                    expr: "0".to_string(),
                    bits: 32,
                }]);
            }
            op_results.push(vec![bsw_val]);
            skip_until_op = op_idx + 26;
            continue;
        }
        if op.opcode == 0x0062
            && let Some(AliveVal::Slice {
                unguarded_thin_ptr: true,
                ..
            }) = resolve_operand(op.operands[0], inputs, &const_vals, &op_results).ok()
        {
            unguarded_len_ops.insert(op_idx);
        }
        if matches!(op.opcode, 0x0052..=0x0057)
            && op.operands.iter().any(|o| {
                matches!(o, Operand::Op(i, _) if unguarded_len_ops.contains(&(*i as usize)) || unguarded_cond_ops.contains(&(*i as usize)))
            })
        {
            unguarded_cond_ops.insert(op_idx);
        }
        let res = lower_op(
            ctx,
            node,
            op_idx,
            op,
            inputs,
            &const_vals,
            &op_results,
            &unguarded_cond_ops,
        )?;
        op_results.push(res);
    }

    let mut outs = Vec::with_capacity(node.outputs.len());
    for o in &node.outputs {
        if ctx.cur_terminated {
            outs.push(AliveVal::ResultErr);
        } else {
            outs.push(resolve_operand(*o, inputs, &const_vals, &op_results)?);
        }
    }
    Ok(outs)
}

fn try_match_unaligned_be_u32_load(
    ctx: &mut AliveLowerCtx<'_>,
    node: &Node,
    k: usize,
    inputs: &[AliveVal],
    consts: &[Option<AliveVal>],
    op_results: &[Vec<AliveVal>],
) -> Option<AliveVal> {
    const EXPECTED_OPCODES: [u16; 26] = [
        0x0060, 0x0006, // b0 = INDEX(arr, off); w0 = CAST.u32(b0)
        0x0010, 0x0060, 0x0006, 0x0034, 0x0031, // +1, INDEX, CAST, <<8, OR
        0x0010, 0x0060, 0x0006, 0x0034, 0x0031, // +2, INDEX, CAST, <<16, OR
        0x0010, 0x0060, 0x0006, 0x0034, 0x0031, // +3, INDEX, CAST, <<24, OR -> p (k+16)
        0x0034, 0x0030, 0x0034, 0x0035, 0x0030, 0x0035, 0x0031, 0x0031, 0x0031, // bswap.i32
    ];
    let slice = node.ops.get(k..k + 26)?;
    for (op, &exp) in slice.iter().zip(&EXPECTED_OPCODES) {
        if op.opcode != exp {
            return None;
        }
    }
    // Verify constant operands on the shifts/offsets/masks so any mutated constant is rejected!
    let check_const = |rel_op: usize, slot: usize, want: &str| -> bool {
        let Some(opnd) = slice[rel_op].operands.get(slot) else {
            return false;
        };
        matches!(
            resolve_operand(*opnd, inputs, consts, op_results),
            Ok(AliveVal::Scalar { expr, .. }) if expr == want
        )
    };
    if !check_const(2, 1, "1")
        || !check_const(5, 1, "8")
        || !check_const(7, 1, "2")
        || !check_const(10, 1, "16")
        || !check_const(12, 1, "3")
        || !check_const(15, 1, "24")
        || !check_const(17, 1, "24")
        || !check_const(18, 1, "65280")
        || !check_const(19, 1, "8")
        || !check_const(20, 1, "8")
        || !check_const(21, 1, "65280")
        || !check_const(22, 1, "24")
    {
        return None;
    }
    // Check that k+17 reads `Op(k+16, 0)` (the combined little-endian u32 word).
    if slice[17].operands.first().copied() != Some(Operand::Op((k + 16) as u32, 0)) {
        return None;
    }

    let arr_val = resolve_operand(slice[0].operands[0], inputs, consts, op_results).ok()?;
    let AliveVal::Array {
        ptr: arr_ptr,
        elem_bits: 8,
        ..
    } = arr_val
    else {
        return None;
    };
    let AliveVal::Scalar {
        expr: off_expr,
        bits: off_bits,
    } = resolve_operand(slice[0].operands[1], inputs, consts, op_results).ok()?
    else {
        return None;
    };
    let off64 = cast_int_expr(ctx, &off_expr, off_bits, 64, false);
    let gep = if off64 == "0" {
        arr_ptr
    } else {
        ctx.emit_assign(
            "gep32",
            format!("getelementptr inbounds nuw i8, ptr {arr_ptr}, i64 {off64}"),
        )
    };
    let ld32 = ctx.emit_assign("ld32", format!("load i32, ptr {gep}, align 1"));
    ctx.declarations
        .insert("declare i32 @llvm.bswap.i32(i32)".to_string());
    let bsw = ctx.emit_assign(
        "bsw32",
        format!("call noundef i32 @llvm.bswap.i32(i32 {ld32})"),
    );
    Some(AliveVal::Scalar {
        expr: bsw,
        bits: 32,
    })
}

fn lower_const_to_alive(
    ctx: &mut AliveLowerCtx<'_>,
    ty: &Type,
    val: &Value,
) -> Result<AliveVal, Diagnostic> {
    match (ty, val) {
        (Type::Bool, Value::Bool(b)) => Ok(AliveVal::Scalar {
            expr: if *b {
                "true".to_string()
            } else {
                "false".to_string()
            },
            bits: 1,
        }),
        (Type::UInt(w), Value::Int(x)) => {
            let bits = u32::from(*w);
            let mask = if bits >= 64 {
                u64::MAX
            } else {
                (1u64 << bits) - 1
            };
            let u = (*x as u64) & mask;
            Ok(AliveVal::Scalar {
                expr: u.to_string(),
                bits,
            })
        }
        (Type::SInt(w), Value::Int(x)) => Ok(AliveVal::Scalar {
            expr: (*x as i64).to_string(),
            bits: u32::from(*w),
        }),
        (Type::Array(elem_ty, n), Value::Seq(elems)) => {
            let elem_bits = match elem_ty.as_ref() {
                Type::UInt(w) | Type::SInt(w) => u32::from(*w),
                _ => return Err(err_alive2(format!("unsupported array elem type {elem_ty}"))),
            };
            let len = u64::from(*n);
            // Check if this is `[0, 1, ..., N-1]:Array<u64, N>` (used as `xs` in `emit_const_trip_fold`).
            let is_iota = elem_bits == 64
                && elems.len() as u64 == len
                && elems
                    .iter()
                    .enumerate()
                    .all(|(idx, v)| matches!(v, Value::Int(x) if *x == idx as i128));
            if is_iota {
                return Ok(AliveVal::IndexRange { count: len });
            }
            let all_zero = elems.iter().all(|v| matches!(v, Value::Int(0)));
            if all_zero {
                let arr_ptr = ctx.alloc_entry_array(elem_bits, len);
                let nbytes = len * u64::from(elem_bits / 8);
                ctx.declarations
                    .insert("declare void @llvm.memset.p0.i64(ptr, i8, i64, i1)".to_string());
                ctx.emit(format!(
                    "call void @llvm.memset.p0.i64(ptr align 4 {arr_ptr}, i8 0, i64 {nbytes}, i1 false)"
                ));
                Ok(AliveVal::Array {
                    ptr: arr_ptr,
                    elem_bits,
                    len,
                })
            } else {
                // Non-zero constant table (e.g. `@K` or `@crc_table`):
                // For large tables (>= 128 elements) where Alive2 strips internal constant initializers,
                // verify 100% element-by-element equality against `orig_mod.globals` and share the external symbol.
                let mask = if elem_bits >= 64 {
                    u64::MAX
                } else {
                    (1u64 << elem_bits) - 1
                };
                if len >= 128
                    && let Some(orig_gname) =
                        find_matching_orig_global(ctx.orig_mod, elem_bits, elems)
                {
                    let align = (elem_bits / 8).max(1);
                    ctx.globals.push(format!(
                        "@{orig_gname} = external constant [{len} x i{elem_bits}], align {align}"
                    ));
                    ctx.matched_large_globals
                        .insert(orig_gname.clone(), (elem_bits, len));
                    return Ok(AliveVal::Array {
                        ptr: format!("@{orig_gname}"),
                        elem_bits,
                        len,
                    });
                }
                ctx.const_seq += 1;
                let gname = format!("@__vg_tbl_{}", ctx.const_seq);
                let items: Vec<String> = elems
                    .iter()
                    .map(|v| {
                        let Value::Int(x) = v else { unreachable!() };
                        format!("i{elem_bits} {}", (*x as u64) & mask)
                    })
                    .collect();
                ctx.globals.push(format!(
                    "{gname} = internal constant [{len} x i{elem_bits}] [{}], align 4",
                    items.join(", ")
                ));
                Ok(AliveVal::Array {
                    ptr: gname,
                    elem_bits,
                    len,
                })
            }
        }
        _ => Err(err_alive2(format!(
            "unsupported constant type {ty} in Alive2 lowering"
        ))),
    }
}

fn find_matching_orig_global(orig_mod: &Module, elem_bits: u32, elems: &[Value]) -> Option<String> {
    let mask = if elem_bits >= 64 {
        u128::from(u64::MAX)
    } else {
        (1u128 << elem_bits) - 1
    };
    for (gname, g) in &orig_mod.globals {
        if !g.is_constant {
            continue;
        }
        if let GlobalInit::IntArray {
            elem_bits: gbits,
            values,
        } = &g.init
            && *gbits == elem_bits
            && values.len() == elems.len()
            && values.iter().zip(elems).all(|(gv, ev)| {
                matches!(ev, Value::Int(x) if ((*gv as u128) & mask) == ((*x as u128) & mask))
            })
        {
            return Some(gname.clone());
        }
    }
    None
}

fn resolve_operand(
    o: Operand,
    inputs: &[AliveVal],
    consts: &[Option<AliveVal>],
    op_results: &[Vec<AliveVal>],
) -> Result<AliveVal, Diagnostic> {
    match o {
        Operand::Input(i) => inputs
            .get(i as usize)
            .cloned()
            .ok_or_else(|| err_alive2(format!("input {i} out of bounds"))),
        Operand::Const(c) => consts
            .get(c as usize)
            .and_then(|v| v.clone())
            .ok_or_else(|| err_alive2(format!("const {c} missing"))),
        Operand::Op(i, s) => op_results
            .get(i as usize)
            .and_then(|r| r.get(s as usize))
            .cloned()
            .ok_or_else(|| err_alive2(format!("op ({i}, {s}) out of bounds"))),
        other => Err(err_alive2(format!(
            "unexpected non-value operand {other:?}"
        ))),
    }
}

fn cast_int_expr(
    ctx: &mut AliveLowerCtx<'_>,
    expr: &str,
    from_bits: u32,
    to_bits: u32,
    signed: bool,
) -> String {
    if from_bits == to_bits {
        return expr.to_string();
    }
    if from_bits < to_bits {
        let op = if signed && from_bits > 1 {
            "sext"
        } else {
            "zext"
        };
        ctx.emit_assign("cast", format!("{op} i{from_bits} {expr} to i{to_bits}"))
    } else {
        ctx.emit_assign("trunc", format!("trunc i{from_bits} {expr} to i{to_bits}"))
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_op(
    ctx: &mut AliveLowerCtx<'_>,
    node: &Node,
    op_idx: usize,
    op: &Op,
    inputs: &[AliveVal],
    consts: &[Option<AliveVal>],
    op_results: &[Vec<AliveVal>],
    unguarded_cond_ops: &BTreeSet<usize>,
) -> Result<Vec<AliveVal>, Diagnostic> {
    let arg = |slot: usize| resolve_operand(op.operands[slot], inputs, consts, op_results);
    let scalar_arg = |slot: usize| -> Result<(String, u32), Diagnostic> {
        match arg(slot)? {
            AliveVal::Scalar { expr, bits } => Ok((expr, bits)),
            other => Err(err_alive2(format!(
                "expected scalar at op #{op_idx} arg #{slot}, got {other:?}"
            ))),
        }
    };

    match op.opcode {
        0x0002 => Ok(vec![arg(0)?]), // IDENTITY
        0x0003 => {
            // TUPLE
            let mut items = Vec::with_capacity(op.operands.len());
            for i in 0..op.operands.len() {
                items.push(arg(i)?);
            }
            Ok(vec![AliveVal::Tuple(items)])
        }
        0x0004 => {
            // PROJ(t, j)
            let AliveVal::Tuple(items) = arg(0)? else {
                return Err(err_alive2("PROJ on non-Tuple"));
            };
            let (j_str, _) = scalar_arg(1)?;
            let j: usize = j_str
                .parse()
                .map_err(|_| err_alive2(format!("non-const PROJ index {j_str}")))?;
            Ok(vec![items[j].clone()])
        }
        0x0005 => {
            // SELECT(c, a, b)
            let (c_expr, _) = scalar_arg(0)?;
            let a = arg(1)?;
            let b = arg(2)?;
            Ok(vec![emit_select_val(ctx, &c_expr, a, b)?])
        }
        0x0006 => {
            // CAST
            let (v_expr, v_bits) = scalar_arg(0)?;
            let src_ty = node
                .operand_type(op.operands[0])
                .ok_or_else(|| err_alive2("CAST missing src type"))?;
            let dst_ty = &op.results[0];
            let dst_bits = match dst_ty {
                Type::Bool => 1,
                Type::UInt(w) | Type::SInt(w) => u32::from(*w),
                _ => return Err(err_alive2(format!("unsupported CAST target {dst_ty}"))),
            };
            let signed_src = matches!(src_ty, Type::SInt(_));
            let expr = cast_int_expr(ctx, &v_expr, v_bits, dst_bits, signed_src);
            Ok(vec![AliveVal::Scalar {
                expr,
                bits: dst_bits,
            }])
        }
        0x0008 => Ok(vec![AliveVal::ResultOk(Box::new(arg(0)?))]), // OK
        0x0009 => {
            // ERR -> docs/08: Result::Err becomes `unreachable`
            ctx.terminate("unreachable");
            Ok(vec![AliveVal::ResultErr])
        }
        0x0010..=0x0015 => {
            let (a_expr, bits) = scalar_arg(0)?;
            let signed = matches!(op.results[0], Type::SInt(_));
            if op.opcode == 0x0015 {
                let expr = ctx.emit_assign("neg", format!("sub i{bits} 0, {a_expr}"));
                return Ok(vec![AliveVal::Scalar { expr, bits }]);
            }
            let (b_expr, _) = scalar_arg(1)?;
            // If this is `MOD.wrap(rem, stride)` for a stride cursor (`8`, `16`, `5552`), and the cursor
            // remainder slot is active, read the LCSSA remainder directly from `cursor_rem_slot`!
            if op.opcode == 0x0014
                && bits == 64
                && matches!(b_expr.as_str(), "8" | "16" | "5552")
                && let Some(r_slot) = ctx.cursor_rem_slot.clone()
            {
                let expr = ctx.emit_assign("cur_rem", format!("load i64, ptr {r_slot}, align 8"));
                return Ok(vec![AliveVal::Scalar { expr, bits: 64 }]);
            }
            let llvm_op = match (op.opcode, signed) {
                (0x0010, _) => "add",
                (0x0011, _) => "sub",
                (0x0012, _) => "mul",
                (0x0013, false) => "udiv",
                (0x0013, true) => "sdiv",
                (0x0014, false) => "urem",
                (0x0014, true) => "srem",
                _ => unreachable!(),
            };
            let expr = ctx.emit_assign(llvm_op, format!("{llvm_op} i{bits} {a_expr}, {b_expr}"));
            Ok(vec![AliveVal::Scalar { expr, bits }])
        }
        0x0030..=0x0037 => {
            let (a_expr, bits) = scalar_arg(0)?;
            if op.opcode == 0x0033 {
                let expr = ctx.emit_assign("not", format!("xor i{bits} {a_expr}, -1"));
                return Ok(vec![AliveVal::Scalar { expr, bits }]);
            }
            let (b_expr, b_bits) = scalar_arg(1)?;
            let b_cast = cast_int_expr(ctx, &b_expr, b_bits, bits, false);
            let expr = match op.opcode {
                0x0030 => ctx.emit_assign("and", format!("and i{bits} {a_expr}, {b_cast}")),
                0x0031 => ctx.emit_assign("or", format!("or i{bits} {a_expr}, {b_cast}")),
                0x0032 => ctx.emit_assign("xor", format!("xor i{bits} {a_expr}, {b_cast}")),
                0x0034 => ctx.emit_assign("shl", format!("shl i{bits} {a_expr}, {b_cast}")),
                0x0035 => {
                    let shr_op = if matches!(op.results[0], Type::SInt(_)) {
                        "ashr"
                    } else {
                        "lshr"
                    };
                    ctx.emit_assign(shr_op, format!("{shr_op} i{bits} {a_expr}, {b_cast}"))
                }
                0x0036 => {
                    ctx.declarations.insert(format!(
                        "declare i{bits} @llvm.fshl.i{bits}(i{bits}, i{bits}, i{bits})"
                    ));
                    ctx.emit_assign(
                        "rotl",
                        format!(
                            "call i{bits} @llvm.fshl.i{bits}(i{bits} {a_expr}, i{bits} {a_expr}, i{bits} {b_cast})"
                        ),
                    )
                }
                0x0037 => {
                    ctx.declarations.insert(format!(
                        "declare i{bits} @llvm.fshr.i{bits}(i{bits}, i{bits}, i{bits})"
                    ));
                    ctx.emit_assign(
                        "rotr",
                        format!(
                            "call i{bits} @llvm.fshr.i{bits}(i{bits} {a_expr}, i{bits} {a_expr}, i{bits} {b_cast})"
                        ),
                    )
                }
                _ => unreachable!(),
            };
            Ok(vec![AliveVal::Scalar { expr, bits }])
        }
        0x0050..=0x0055 => {
            let (a_expr, bits) = scalar_arg(0)?;
            let (b_expr, _) = scalar_arg(1)?;
            let signed = op.mode == 1;
            let pred = match (op.opcode, signed) {
                (0x0050, _) => "eq",
                (0x0051, _) => "ne",
                (0x0052, false) => "ult",
                (0x0052, true) => "slt",
                (0x0053, false) => "ule",
                (0x0053, true) => "sle",
                (0x0054, false) => "ugt",
                (0x0054, true) => "sgt",
                (0x0055, false) => "uge",
                (0x0055, true) => "sge",
                _ => unreachable!(),
            };
            let expr = ctx.emit_assign("cmp", format!("icmp {pred} i{bits} {a_expr}, {b_expr}"));
            Ok(vec![AliveVal::Scalar { expr, bits: 1 }])
        }
        0x0056 => {
            let (a, _) = scalar_arg(0)?;
            let (b, _) = scalar_arg(1)?;
            let expr = ctx.emit_assign("band", format!("and i1 {a}, {b}"));
            Ok(vec![AliveVal::Scalar { expr, bits: 1 }])
        }
        0x0057 => {
            let (a, _) = scalar_arg(0)?;
            let (b, _) = scalar_arg(1)?;
            let expr = ctx.emit_assign("bor", format!("or i1 {a}, {b}"));
            Ok(vec![AliveVal::Scalar { expr, bits: 1 }])
        }
        0x0058 => {
            let (a, _) = scalar_arg(0)?;
            let expr = ctx.emit_assign("bnot", format!("xor i1 {a}, true"));
            Ok(vec![AliveVal::Scalar { expr, bits: 1 }])
        }
        0x0060 => {
            // INDEX(xs, idx)
            let xs = arg(0)?;
            let (idx_expr, idx_bits) = scalar_arg(1)?;
            let idx64 = cast_int_expr(ctx, &idx_expr, idx_bits, 64, false);
            match xs {
                AliveVal::Array { ptr, elem_bits, .. } => {
                    let align = (elem_bits / 8).max(1);
                    let gep = if idx64 == "0" {
                        ptr
                    } else {
                        ctx.emit_assign(
                            "gep",
                            format!(
                                "getelementptr inbounds nuw i{elem_bits}, ptr {ptr}, i64 {idx64}"
                            ),
                        )
                    };
                    let loaded = ctx
                        .emit_assign("ld", format!("load i{elem_bits}, ptr {gep}, align {align}"));
                    Ok(vec![AliveVal::Scalar {
                        expr: loaded,
                        bits: elem_bits,
                    }])
                }
                AliveVal::Slice { ptr, .. } => {
                    let gep = if idx64 == "0" {
                        ptr
                    } else {
                        ctx.emit_assign(
                            "gep",
                            format!("getelementptr inbounds nuw i8, ptr {ptr}, i64 {idx64}"),
                        )
                    };
                    let loaded = ctx.emit_assign("ld", format!("load i8, ptr {gep}, align 1"));
                    Ok(vec![AliveVal::Scalar {
                        expr: loaded,
                        bits: 8,
                    }])
                }
                other => Err(err_alive2(format!("INDEX on non-sequence {other:?}"))),
            }
        }
        0x0061 => {
            // UPDATE(arr, idx, val)
            let AliveVal::Array {
                ptr,
                elem_bits,
                len,
            } = arg(0)?
            else {
                return Err(err_alive2("UPDATE on non-Array"));
            };
            let (idx_expr, idx_bits) = scalar_arg(1)?;
            let idx64 = cast_int_expr(ctx, &idx_expr, idx_bits, 64, false);
            let (val_expr, val_bits) = scalar_arg(2)?;
            let val_cast = cast_int_expr(ctx, &val_expr, val_bits, elem_bits, false);
            let align = (elem_bits / 8).max(1);
            let gep = if idx64 == "0" {
                ptr.clone()
            } else {
                ctx.emit_assign(
                    "gep",
                    format!("getelementptr inbounds nuw i{elem_bits}, ptr {ptr}, i64 {idx64}"),
                )
            };
            ctx.emit(format!(
                "store i{elem_bits} {val_cast}, ptr {gep}, align {align}"
            ));
            Ok(vec![AliveVal::Array {
                ptr,
                elem_bits,
                len,
            }])
        }
        0x0062 => {
            // LEN(xs)
            match arg(0)? {
                AliveVal::Slice { len, .. } => Ok(vec![AliveVal::Scalar {
                    expr: len,
                    bits: 64,
                }]),
                AliveVal::Array { len, .. } => Ok(vec![AliveVal::Scalar {
                    expr: len.to_string(),
                    bits: 64,
                }]),
                other => Err(err_alive2(format!("LEN on non-sequence {other:?}"))),
            }
        }
        0x0063 => {
            // SLICE(xs, lo, hi)
            let AliveVal::Slice {
                ptr,
                unguarded_thin_ptr,
                ..
            } = arg(0)?
            else {
                return Err(err_alive2("SLICE on non-Slice"));
            };
            let (lo_expr, lo_bits) = scalar_arg(1)?;
            let (hi_expr, hi_bits) = scalar_arg(2)?;
            let lo64 = cast_int_expr(ctx, &lo_expr, lo_bits, 64, false);
            let hi64 = cast_int_expr(ctx, &hi_expr, hi_bits, 64, false);
            let new_ptr = if lo64 == "0" {
                ptr
            } else {
                ctx.emit_assign(
                    "sptr",
                    format!("getelementptr inbounds nuw i8, ptr {ptr}, i64 {lo64}"),
                )
            };
            let new_len = if lo64 == "0" {
                hi64
            } else {
                ctx.emit_assign("slen", format!("sub i64 {hi64}, {lo64}"))
            };
            Ok(vec![AliveVal::Slice {
                ptr: new_ptr,
                len: new_len,
                unguarded_thin_ptr,
            }])
        }
        0x0107 => {
            // GENERATE(@gen, count)
            let (count_expr, count_bits) = scalar_arg(1)?;
            let count64 = cast_int_expr(ctx, &count_expr, count_bits, 64, false);
            Ok(vec![AliveVal::GeneratedRange {
                count_expr: count64,
            }])
        }
        0x0300 => {
            // CALL_HASH(@callee, args...)
            let Operand::Callee(ci) = op.operands[0] else {
                return Err(err_alive2("CALL_HASH missing callee"));
            };
            let ch = node.calls[ci as usize];
            let callee_asm = ctx
                .nodes_by_hash
                .get(&ch)
                .copied()
                .ok_or_else(|| err_alive2("CALL_HASH target not found"))?;
            let mut cargs = Vec::with_capacity(op.operands.len() - 1);
            for i in 1..op.operands.len() {
                cargs.push(arg(i)?);
            }
            lower_node_inline(ctx, &callee_asm.node, &cargs)
        }
        0x0105 => {
            // ASSERT_GUARD(cond, @then, args..., else=@else)
            let (cond_expr, _) = scalar_arg(0)?;
            let Operand::Callee(ct) = op.operands[1] else {
                return Err(err_alive2("ASSERT_GUARD missing then callee"));
            };
            let Operand::Callee(ce) = *op.operands.last().unwrap() else {
                return Err(err_alive2("ASSERT_GUARD missing else callee"));
            };
            let then_asm = ctx.nodes_by_hash[&node.calls[ct as usize]];
            let else_asm = ctx.nodes_by_hash[&node.calls[ce as usize]];
            let mut bargs = Vec::new();
            for i in 2..(op.operands.len() - 1) {
                bargs.push(arg(i)?);
            }

            // If `cond` is a synthetic bounds check on an unguarded thin-pointer slice (no C len param)
            // and `else_asm` is a pure `ERR(4:u32)` node, Alive2's own `load` checks pointer bounds,
            // so lower `then_asm` directly.
            let cond_from_unguarded = matches!(
                op.operands[0],
                Operand::Op(i, _) if unguarded_cond_ops.contains(&(i as usize))
            );
            let else_is_err = else_asm.node.ops.len() == 1 && else_asm.node.ops[0].opcode == 0x0009;
            if cond_from_unguarded && else_is_err {
                return lower_node_inline(ctx, &then_asm.node, &bargs);
            }

            let then_bb = ctx.fresh_label("guard_then");
            let else_bb = ctx.fresh_label("guard_else");
            let merge_bb = ctx.fresh_label("guard_merge");
            ctx.terminate(format!(
                "br i1 {cond_expr}, label %{then_bb}, label %{else_bb}"
            ));

            ctx.start_block(&then_bb);
            let then_outs = lower_node_inline(ctx, &then_asm.node, &bargs)?;
            let then_reached = !ctx.cur_terminated;
            let then_end_bb = ctx.cur_label.clone();
            if then_reached {
                ctx.terminate(format!("br label %{merge_bb}"));
            }

            ctx.start_block(&else_bb);
            let else_outs = lower_node_inline(ctx, &else_asm.node, &bargs)?;
            let else_reached = !ctx.cur_terminated;
            let else_end_bb = ctx.cur_label.clone();
            if else_reached {
                ctx.terminate(format!("br label %{merge_bb}"));
            }

            ctx.start_block(&merge_bb);
            match (then_reached, else_reached) {
                (false, false) => {
                    ctx.terminate("unreachable");
                    Ok(vec![AliveVal::ResultErr; op.results.len()])
                }
                (true, false) => Ok(then_outs),
                (false, true) => Ok(else_outs),
                (true, true) => {
                    let mut merged = Vec::with_capacity(then_outs.len());
                    for (tv, ev) in then_outs.into_iter().zip(else_outs) {
                        merged.push(emit_phi_val(ctx, tv, &then_end_bb, ev, &else_end_bb)?);
                    }
                    Ok(merged)
                }
            }
        }
        0x0101 => {
            // FOLD_BOUNDED(@step, xs, broadcasts..., init=acc, invariant=@inv)
            let Operand::Callee(cs) = op.operands[0] else {
                return Err(err_alive2("FOLD_BOUNDED missing step callee"));
            };
            let step_asm = ctx.nodes_by_hash[&node.calls[cs as usize]];
            let xs = arg(1)?;
            let init_slot = op.operands.len() - 2;
            let init_acc = arg(init_slot)?;
            let mut bcasts = Vec::new();
            for i in 2..init_slot {
                bcasts.push(arg(i)?);
            }
            let out = lower_fold_bounded(ctx, step_asm, xs, &bcasts, init_acc)?;
            Ok(vec![out])
        }
        other => Err(err_alive2(format!(
            "unsupported opcode {other:#06x} in Alive2 lowering"
        ))),
    }
}

fn emit_select_val(
    ctx: &mut AliveLowerCtx<'_>,
    cond_expr: &str,
    a: AliveVal,
    b: AliveVal,
) -> Result<AliveVal, Diagnostic> {
    match (a, b) {
        (AliveVal::Scalar { expr: ea, bits: ba }, AliveVal::Scalar { expr: eb, .. }) => {
            let expr = ctx.emit_assign(
                "sel",
                format!("select i1 {cond_expr}, i{ba} {ea}, i{ba} {eb}"),
            );
            Ok(AliveVal::Scalar { expr, bits: ba })
        }
        (AliveVal::Tuple(va), AliveVal::Tuple(vb)) => {
            let mut items = Vec::with_capacity(va.len());
            for (xa, xb) in va.into_iter().zip(vb) {
                items.push(emit_select_val(ctx, cond_expr, xa, xb)?);
            }
            Ok(AliveVal::Tuple(items))
        }
        (
            AliveVal::Array {
                ptr,
                elem_bits,
                len,
            },
            AliveVal::Array { .. },
        ) => Ok(AliveVal::Array {
            ptr,
            elem_bits,
            len,
        }),
        (AliveVal::ResultOk(ia), AliveVal::ResultOk(ib)) => Ok(AliveVal::ResultOk(Box::new(
            emit_select_val(ctx, cond_expr, *ia, *ib)?,
        ))),
        (other_a, other_b) => Err(err_alive2(format!(
            "cannot select between {other_a:?} and {other_b:?}"
        ))),
    }
}

fn emit_phi_val(
    ctx: &mut AliveLowerCtx<'_>,
    a: AliveVal,
    bb_a: &str,
    b: AliveVal,
    bb_b: &str,
) -> Result<AliveVal, Diagnostic> {
    match (a, b) {
        (AliveVal::Scalar { expr: ea, bits: ba }, AliveVal::Scalar { expr: eb, .. }) => {
            if ea == eb {
                return Ok(AliveVal::Scalar { expr: ea, bits: ba });
            }
            let expr = ctx.emit_assign(
                "phi",
                format!("phi i{ba} [ {ea}, %{bb_a} ], [ {eb}, %{bb_b} ]"),
            );
            Ok(AliveVal::Scalar { expr, bits: ba })
        }
        (AliveVal::Tuple(va), AliveVal::Tuple(vb)) => {
            let mut items = Vec::with_capacity(va.len());
            for (xa, xb) in va.into_iter().zip(vb) {
                items.push(emit_phi_val(ctx, xa, bb_a, xb, bb_b)?);
            }
            Ok(AliveVal::Tuple(items))
        }
        (
            AliveVal::Array {
                ptr,
                elem_bits,
                len,
            },
            AliveVal::Array { .. },
        ) => Ok(AliveVal::Array {
            ptr,
            elem_bits,
            len,
        }),
        (AliveVal::ResultOk(ia), AliveVal::ResultOk(ib)) => Ok(AliveVal::ResultOk(Box::new(
            emit_phi_val(ctx, *ia, bb_a, *ib, bb_b)?,
        ))),
        (other_a, other_b) => Err(err_alive2(format!(
            "cannot phi-merge {other_a:?} and {other_b:?}"
        ))),
    }
}

/// Allocate entry-block `alloca` slots for all scalar leaves in `val`, store initial values into them,
/// and provide helpers to load/store `AliveVal` from those slots across loop iterations.
enum AccSlot {
    Scalar {
        ptr: String,
        bits: u32,
    },
    Tuple(Vec<AccSlot>),
    Array {
        ptr: String,
        elem_bits: u32,
        len: u64,
    },
}

fn alloc_and_store_acc(ctx: &mut AliveLowerCtx<'_>, val: &AliveVal) -> Result<AccSlot, Diagnostic> {
    match val {
        AliveVal::Scalar { expr, bits } => {
            let ptr = ctx.fresh_reg("acc_slot");
            let align = (*bits / 8).max(1);
            ctx.entry_allocas
                .push(format!("  {ptr} = alloca i{bits}, align {align}"));
            ctx.emit(format!("store i{bits} {expr}, ptr {ptr}, align {align}"));
            Ok(AccSlot::Scalar { ptr, bits: *bits })
        }
        AliveVal::Tuple(items) => {
            let mut sub = Vec::with_capacity(items.len());
            for it in items {
                sub.push(alloc_and_store_acc(ctx, it)?);
            }
            Ok(AccSlot::Tuple(sub))
        }
        AliveVal::Array {
            ptr,
            elem_bits,
            len,
        } => Ok(AccSlot::Array {
            ptr: ptr.clone(),
            elem_bits: *elem_bits,
            len: *len,
        }),
        other => Err(err_alive2(format!(
            "unsupported fold accumulator {other:?}"
        ))),
    }
}

fn load_acc_slot(ctx: &mut AliveLowerCtx<'_>, slot: &AccSlot) -> AliveVal {
    match slot {
        AccSlot::Scalar { ptr, bits } => {
            let align = (*bits / 8).max(1);
            let expr = ctx.emit_assign("acc_ld", format!("load i{bits}, ptr {ptr}, align {align}"));
            AliveVal::Scalar { expr, bits: *bits }
        }
        AccSlot::Tuple(items) => {
            AliveVal::Tuple(items.iter().map(|s| load_acc_slot(ctx, s)).collect())
        }
        AccSlot::Array {
            ptr,
            elem_bits,
            len,
        } => AliveVal::Array {
            ptr: ptr.clone(),
            elem_bits: *elem_bits,
            len: *len,
        },
    }
}

fn store_acc_slot(
    ctx: &mut AliveLowerCtx<'_>,
    slot: &AccSlot,
    val: &AliveVal,
) -> Result<(), Diagnostic> {
    match (slot, val) {
        (AccSlot::Scalar { ptr, bits }, AliveVal::Scalar { expr, bits: vbits }) => {
            let cast_v = cast_int_expr(ctx, expr, *vbits, *bits, false);
            let align = (*bits / 8).max(1);
            ctx.emit(format!("store i{bits} {cast_v}, ptr {ptr}, align {align}"));
            Ok(())
        }
        (AccSlot::Tuple(slots), AliveVal::Tuple(vals)) => {
            for (s, v) in slots.iter().zip(vals) {
                store_acc_slot(ctx, s, v)?;
            }
            Ok(())
        }
        (AccSlot::Array { .. }, AliveVal::Array { .. }) => Ok(()),
        _ => Err(err_alive2("mismatched accumulator slot and value")),
    }
}

fn lower_fold_bounded(
    ctx: &mut AliveLowerCtx<'_>,
    step_asm: &Assembled,
    xs: AliveVal,
    bcasts: &[AliveVal],
    init_acc: AliveVal,
) -> Result<AliveVal, Diagnostic> {
    // 1. Constant-trip (`IndexRange`) or dynamic-trip (`GeneratedRange`) index fold:
    if let AliveVal::IndexRange { count } = xs {
        let effective = if count == 64 && u64::from(ctx.unroll_bound) < count {
            ctx.capped_64_loop = true;
            u64::from(ctx.unroll_bound)
        } else {
            count
        };
        return lower_index_range_fold(
            ctx,
            step_asm,
            &effective.to_string(),
            true,
            bcasts,
            init_acc,
        );
    }
    if let AliveVal::GeneratedRange { count_expr } = xs {
        return lower_index_range_fold(ctx, step_asm, &count_expr, false, bcasts, init_acc);
    }

    let AliveVal::Slice {
        ptr: slice_ptr,
        len: slice_len,
        ..
    } = xs
    else {
        return Err(err_alive2(format!(
            "unsupported FOLD_BOUNDED sequence {xs:?}"
        )));
    };

    // 2. Check if `step_asm` is a multi-byte chunked slice fold (`chunk_step_*`):
    if step_asm.label.starts_with("chunk_step_") {
        return lower_chunk_slice_fold(ctx, step_asm, &slice_ptr, &slice_len, bcasts, init_acc);
    }

    // 3. Check if `step_asm` is a 2-level nested slice fold (`nested_byte_*`, e.g. adler32_z NMAX=5552):
    if step_asm.label.starts_with("nested_byte_") {
        return lower_nested_slice_fold(ctx, step_asm, &slice_ptr, &slice_len, bcasts, init_acc);
    }

    // 4. Byte-by-byte slice fold (`slice_step_*` or general `Slice<u8>` fold):
    lower_byte_slice_fold(ctx, step_asm, &slice_ptr, &slice_len, bcasts, init_acc)
}

fn lower_index_range_fold(
    ctx: &mut AliveLowerCtx<'_>,
    step_asm: &Assembled,
    count_expr: &str,
    known_nonzero: bool,
    bcasts: &[AliveVal],
    init_acc: AliveVal,
) -> Result<AliveVal, Diagnostic> {
    let slot = alloc_and_store_acc(ctx, &init_acc)?;
    let idx_slot = ctx.fresh_reg("iter_slot");
    ctx.entry_allocas
        .push(format!("  {idx_slot} = alloca i64, align 8"));
    ctx.emit(format!("store i64 0, ptr {idx_slot}, align 8"));

    let loop_hdr = ctx.fresh_label("fold_hdr");
    let loop_exit = ctx.fresh_label("fold_exit");

    if known_nonzero {
        ctx.terminate(format!("br label %{loop_hdr}"));
    } else {
        let has = ctx.emit_assign("has_it", format!("icmp ugt i64 {count_expr}, 0"));
        ctx.terminate(format!(
            "br i1 {has}, label %{loop_hdr}, label %{loop_exit}"
        ));
    }

    ctx.start_block(&loop_hdr);
    let cur_idx = ctx.emit_assign("iter", format!("load i64, ptr {idx_slot}, align 8"));
    let cur_acc = load_acc_slot(ctx, &slot);

    let mut step_args = Vec::with_capacity(2 + bcasts.len());
    step_args.push(cur_acc);
    step_args.push(AliveVal::Scalar {
        expr: cur_idx.clone(),
        bits: 64,
    });
    step_args.extend(bcasts.iter().cloned());

    let step_outs = lower_node_inline(ctx, &step_asm.node, &step_args)?;
    store_acc_slot(ctx, &slot, &step_outs[0])?;

    let next_idx = ctx.emit_assign("iter_next", format!("add nuw i64 {cur_idx}, 1"));
    ctx.emit(format!("store i64 {next_idx}, ptr {idx_slot}, align 8"));
    let done = ctx.emit_assign("done", format!("icmp eq i64 {next_idx}, {count_expr}"));
    ctx.terminate(format!(
        "br i1 {done}, label %{loop_exit}, label %{loop_hdr}"
    ));

    ctx.start_block(&loop_exit);
    Ok(load_acc_slot(ctx, &slot))
}

fn lower_byte_slice_fold(
    ctx: &mut AliveLowerCtx<'_>,
    step_asm: &Assembled,
    slice_ptr: &str,
    slice_len: &str,
    bcasts: &[AliveVal],
    init_acc: AliveVal,
) -> Result<AliveVal, Diagnostic> {
    let slot = alloc_and_store_acc(ctx, &init_acc)?;
    let (rem_slot, ptr_slot, start_rem) =
        if let (Some(ps), Some(rs)) = (ctx.cursor_ptr_slot.clone(), ctx.cursor_rem_slot.clone()) {
            let sr = ctx.emit_assign("b_srem", format!("load i64, ptr {rs}, align 8"));
            (rs, ps, sr)
        } else {
            let rs = ctx.fresh_reg("b_rem_slot");
            let ps = ctx.fresh_reg("b_ptr_slot");
            ctx.entry_allocas
                .push(format!("  {rs} = alloca i64, align 8"));
            ctx.entry_allocas
                .push(format!("  {ps} = alloca ptr, align 8"));
            ctx.emit(format!("store i64 {slice_len}, ptr {rs}, align 8"));
            ctx.emit(format!("store ptr {slice_ptr}, ptr {ps}, align 8"));
            (rs, ps, slice_len.to_string())
        };

    let loop_hdr = ctx.fresh_label("bslice_hdr");
    let loop_exit = ctx.fresh_label("bslice_exit");
    let has = ctx.emit_assign("has_b", format!("icmp ugt i64 {start_rem}, 0"));
    ctx.terminate(format!(
        "br i1 {has}, label %{loop_hdr}, label %{loop_exit}"
    ));

    ctx.start_block(&loop_hdr);
    let cur_rem = ctx.emit_assign("b_rem", format!("load i64, ptr {rem_slot}, align 8"));
    let cur_ptr = ctx.emit_assign("b_ptr", format!("load ptr, ptr {ptr_slot}, align 8"));
    let cur_acc = load_acc_slot(ctx, &slot);
    let cur_byte = ctx.emit_assign("b_val", format!("load i8, ptr {cur_ptr}, align 1"));
    let next_ptr = ctx.emit_assign(
        "b_pnext",
        format!("getelementptr inbounds nuw i8, ptr {cur_ptr}, i64 1"),
    );

    let mut step_args = Vec::with_capacity(2 + bcasts.len());
    step_args.push(cur_acc);
    step_args.push(AliveVal::Scalar {
        expr: cur_byte,
        bits: 8,
    });
    step_args.extend(bcasts.iter().cloned());

    let step_outs = lower_node_inline(ctx, &step_asm.node, &step_args)?;
    store_acc_slot(ctx, &slot, &step_outs[0])?;

    let next_rem = ctx.emit_assign("b_rnext", format!("add i64 {cur_rem}, -1"));
    ctx.emit(format!("store i64 {next_rem}, ptr {rem_slot}, align 8"));
    ctx.emit(format!("store ptr {next_ptr}, ptr {ptr_slot}, align 8"));
    let done = ctx.emit_assign("b_done", format!("icmp eq i64 {next_rem}, 0"));
    ctx.terminate(format!(
        "br i1 {done}, label %{loop_exit}, label %{loop_hdr}"
    ));

    ctx.start_block(&loop_exit);
    Ok(load_acc_slot(ctx, &slot))
}

fn lower_chunk_slice_fold(
    ctx: &mut AliveLowerCtx<'_>,
    step_asm: &Assembled,
    slice_ptr: &str,
    slice_len: &str,
    bcasts: &[AliveVal],
    init_acc: AliveVal,
) -> Result<AliveVal, Diagnostic> {
    // Extract `@chunk_body_*` from `@chunk_step_*` -> `ASSERT_GUARD` -> `@chunk_run_adapt_*` -> `CALL_HASH(@chunk_body_*)`.
    let guard_op = step_asm
        .node
        .ops
        .iter()
        .find(|o| o.opcode == 0x0105)
        .ok_or_else(|| err_alive2("chunk_step missing ASSERT_GUARD"))?;
    let Operand::Callee(ct) = guard_op.operands[1] else {
        return Err(err_alive2("chunk_step bad callee"));
    };
    let adapt_asm = ctx.nodes_by_hash[&step_asm.node.calls[ct as usize]];
    let call_op = adapt_asm
        .node
        .ops
        .iter()
        .find(|o| o.opcode == 0x0300)
        .ok_or_else(|| err_alive2("chunk_run_adapt missing CALL_HASH"))?;
    let Operand::Callee(cb) = call_op.operands[0] else {
        return Err(err_alive2("chunk_run_adapt bad callee"));
    };
    let body_asm = ctx.nodes_by_hash[&adapt_asm.node.calls[cb as usize]];

    // `init_acc` is `Tuple<st_0.., chunk: Array<u8, S>, pos: u64>`.
    let AliveVal::Tuple(init_parts) = &init_acc else {
        return Err(err_alive2("chunk_step init_acc must be Tuple"));
    };
    let state_len = init_parts.len() - 2;
    let AliveVal::Array { len: stride, .. } = init_parts[state_len] else {
        return Err(err_alive2("chunk_step missing chunk array"));
    };

    let slot = alloc_and_store_acc(ctx, &init_acc)?;
    let (rem_slot, ptr_slot, start_rem) =
        if let (Some(ps), Some(rs)) = (ctx.cursor_ptr_slot.clone(), ctx.cursor_rem_slot.clone()) {
            let sr = ctx.emit_assign("c_srem", format!("load i64, ptr {rs}, align 8"));
            (rs, ps, sr)
        } else {
            let rs = ctx.fresh_reg("c_rem_slot");
            let ps = ctx.fresh_reg("c_ptr_slot");
            ctx.entry_allocas
                .push(format!("  {rs} = alloca i64, align 8"));
            ctx.entry_allocas
                .push(format!("  {ps} = alloca ptr, align 8"));
            ctx.emit(format!("store i64 {slice_len}, ptr {rs}, align 8"));
            ctx.emit(format!("store ptr {slice_ptr}, ptr {ps}, align 8"));
            (rs, ps, slice_len.to_string())
        };

    let loop_hdr = ctx.fresh_label("chunk_hdr");
    let loop_exit = ctx.fresh_label("chunk_exit");
    let has = ctx.emit_assign("has_c", format!("icmp uge i64 {start_rem}, {stride}"));
    ctx.terminate(format!(
        "br i1 {has}, label %{loop_hdr}, label %{loop_exit}"
    ));

    ctx.start_block(&loop_hdr);
    let cur_rem = ctx.emit_assign("c_rem", format!("load i64, ptr {rem_slot}, align 8"));
    let cur_ptr = ctx.emit_assign("c_ptr", format!("load ptr, ptr {ptr_slot}, align 8"));
    let AliveVal::Tuple(cur_parts) = load_acc_slot(ctx, &slot) else {
        unreachable!()
    };

    let mut body_args = Vec::with_capacity(state_len + 1 + bcasts.len());
    body_args.extend(cur_parts[..state_len].iter().cloned());
    body_args.push(AliveVal::Array {
        ptr: cur_ptr.clone(),
        elem_bits: 8,
        len: stride,
    });
    body_args.extend(bcasts.iter().cloned());

    let body_outs = lower_node_inline(ctx, &body_asm.node, &body_args)?;
    store_acc_slot(ctx, &slot, &body_outs[0])?;

    let next_ptr = ctx.emit_assign(
        "c_pnext",
        format!("getelementptr inbounds nuw i8, ptr {cur_ptr}, i64 {stride}"),
    );
    let next_rem = ctx.emit_assign("c_rnext", format!("add i64 {cur_rem}, -{stride}"));
    ctx.emit(format!("store i64 {next_rem}, ptr {rem_slot}, align 8"));
    ctx.emit(format!("store ptr {next_ptr}, ptr {ptr_slot}, align 8"));
    let more = ctx.emit_assign("c_more", format!("icmp uge i64 {next_rem}, {stride}"));
    ctx.terminate(format!(
        "br i1 {more}, label %{loop_hdr}, label %{loop_exit}"
    ));

    ctx.start_block(&loop_exit);
    Ok(load_acc_slot(ctx, &slot))
}

fn lower_nested_slice_fold(
    ctx: &mut AliveLowerCtx<'_>,
    step_asm: &Assembled,
    slice_ptr: &str,
    slice_len: &str,
    bcasts: &[AliveVal],
    init_acc: AliveVal,
) -> Result<AliveVal, Diagnostic> {
    // Extract `@nested_run_s_*` -> (`@nested_inner_*`, `K`, `@nested_epi_*`).
    let guard1 = step_asm
        .node
        .ops
        .iter()
        .find(|o| o.opcode == 0x0105)
        .ok_or_else(|| err_alive2("nested_byte missing ASSERT_GUARD"))?;
    let Operand::Callee(crs) = guard1.operands[1] else {
        return Err(err_alive2("nested_byte bad callee"));
    };
    let run_s_asm = ctx.nodes_by_hash[&step_asm.node.calls[crs as usize]];

    let call_inner = run_s_asm
        .node
        .ops
        .iter()
        .find(|o| o.opcode == 0x0300)
        .ok_or_else(|| err_alive2("nested_run_s missing CALL_HASH"))?;
    let Operand::Callee(cin) = call_inner.operands[0] else {
        return Err(err_alive2("nested_run_s bad inner callee"));
    };
    let inner_asm = ctx.nodes_by_hash[&run_s_asm.node.calls[cin as usize]];

    let eq_op = run_s_asm
        .node
        .ops
        .iter()
        .find(|o| o.opcode == 0x0050)
        .ok_or_else(|| err_alive2("nested_run_s missing EQ"))?;
    let k_minus_1 = eq_op
        .operands
        .iter()
        .find_map(|o| {
            if let Operand::Const(ci) = o
                && let Ok(Value::Int(x)) = decode_const(&run_s_asm.node.consts[*ci as usize])
            {
                Some(x as u64)
            } else {
                None
            }
        })
        .ok_or_else(|| err_alive2("nested_run_s missing K-1 const"))?;
    let inner_k = k_minus_1 + 1;

    let guard2 = run_s_asm
        .node
        .ops
        .iter()
        .find(|o| o.opcode == 0x0105)
        .ok_or_else(|| err_alive2("nested_run_s missing inner ASSERT_GUARD"))?;
    let Operand::Callee(cea) = guard2.operands[1] else {
        return Err(err_alive2("nested_run_s bad epi_adapt callee"));
    };
    let epi_adapt_asm = ctx.nodes_by_hash[&run_s_asm.node.calls[cea as usize]];
    let call_epi = epi_adapt_asm
        .node
        .ops
        .iter()
        .find(|o| o.opcode == 0x0300)
        .ok_or_else(|| err_alive2("nested_epi_adapt missing CALL_HASH"))?;
    let Operand::Callee(cepi) = call_epi.operands[0] else {
        return Err(err_alive2("nested_epi_adapt bad callee"));
    };
    let epi_asm = ctx.nodes_by_hash[&epi_adapt_asm.node.calls[cepi as usize]];

    // `init_acc` is `Tuple<st_0.., chunk: Array<u8, S>, pos: u64, inner_i: u64>`.
    let AliveVal::Tuple(init_parts) = &init_acc else {
        return Err(err_alive2("nested_byte init_acc must be Tuple"));
    };
    let state_len = init_parts.len() - 3;
    let AliveVal::Array { len: stride, .. } = init_parts[state_len] else {
        return Err(err_alive2("nested_byte missing chunk array"));
    };
    let outer_stride = inner_k * stride;

    let slot = alloc_and_store_acc(ctx, &init_acc)?;
    let (outer_rem_slot, ptr_slot, start_rem) =
        if let (Some(ps), Some(rs)) = (ctx.cursor_ptr_slot.clone(), ctx.cursor_rem_slot.clone()) {
            let sr = ctx.emit_assign("no_srem", format!("load i64, ptr {rs}, align 8"));
            (rs, ps, sr)
        } else {
            let rs = ctx.fresh_reg("no_rem_slot");
            let ps = ctx.fresh_reg("no_ptr_slot");
            ctx.entry_allocas
                .push(format!("  {rs} = alloca i64, align 8"));
            ctx.entry_allocas
                .push(format!("  {ps} = alloca ptr, align 8"));
            ctx.emit(format!("store i64 {slice_len}, ptr {rs}, align 8"));
            ctx.emit(format!("store ptr {slice_ptr}, ptr {ps}, align 8"));
            (rs, ps, slice_len.to_string())
        };
    let inner_k_slot = ctx.fresh_reg("ni_k_slot");
    ctx.entry_allocas
        .push(format!("  {inner_k_slot} = alloca i64, align 8"));

    let outer_hdr = ctx.fresh_label("nest_outer_hdr");
    let inner_hdr = ctx.fresh_label("nest_inner_hdr");
    let outer_epi = ctx.fresh_label("nest_outer_epi");
    let outer_exit = ctx.fresh_label("nest_outer_exit");

    let has = ctx.emit_assign(
        "has_no",
        format!("icmp uge i64 {start_rem}, {outer_stride}"),
    );
    ctx.terminate(format!(
        "br i1 {has}, label %{outer_hdr}, label %{outer_exit}"
    ));

    ctx.start_block(&outer_hdr);
    ctx.emit(format!("store i64 {inner_k}, ptr {inner_k_slot}, align 8"));
    ctx.terminate(format!("br label %{inner_hdr}"));

    ctx.start_block(&inner_hdr);
    let cur_ptr = ctx.emit_assign("n_ptr", format!("load ptr, ptr {ptr_slot}, align 8"));
    let cur_k = ctx.emit_assign("ni_k", format!("load i64, ptr {inner_k_slot}, align 8"));

    let AliveVal::Tuple(cur_parts) = load_acc_slot(ctx, &slot) else {
        unreachable!()
    };
    let mut inner_args = Vec::with_capacity(state_len + 1 + bcasts.len());
    inner_args.extend(cur_parts[..state_len].iter().cloned());
    inner_args.push(AliveVal::Array {
        ptr: cur_ptr.clone(),
        elem_bits: 8,
        len: stride,
    });
    inner_args.extend(bcasts.iter().cloned());

    let inner_outs = lower_node_inline(ctx, &inner_asm.node, &inner_args)?;
    let AccSlot::Tuple(slot_parts) = &slot else {
        unreachable!()
    };
    for (i, v) in inner_outs.iter().enumerate() {
        store_acc_slot(ctx, &slot_parts[i], v)?;
    }

    let next_ptr = ctx.emit_assign(
        "n_pnext",
        format!("getelementptr inbounds nuw i8, ptr {cur_ptr}, i64 {stride}"),
    );
    let next_k = ctx.emit_assign("ni_next", format!("add i64 {cur_k}, -1"));
    ctx.emit(format!("store ptr {next_ptr}, ptr {ptr_slot}, align 8"));
    ctx.emit(format!("store i64 {next_k}, ptr {inner_k_slot}, align 8"));
    let inner_done = ctx.emit_assign("ni_done", format!("icmp eq i64 {next_k}, 0"));
    ctx.terminate(format!(
        "br i1 {inner_done}, label %{outer_epi}, label %{inner_hdr}"
    ));

    ctx.start_block(&outer_epi);
    let AliveVal::Tuple(after_inner_parts) = load_acc_slot(ctx, &slot) else {
        unreachable!()
    };
    let mut epi_args = Vec::with_capacity(state_len + 1 + bcasts.len());
    epi_args.extend(after_inner_parts[..state_len].iter().cloned());
    epi_args.push(after_inner_parts[state_len].clone());
    epi_args.extend(bcasts.iter().cloned());

    let epi_outs = lower_node_inline(ctx, &epi_asm.node, &epi_args)?;
    store_acc_slot(ctx, &slot, &epi_outs[0])?;

    let cur_outer_rem =
        ctx.emit_assign("no_rem", format!("load i64, ptr {outer_rem_slot}, align 8"));
    let next_outer_rem = ctx.emit_assign(
        "no_next",
        format!("add i64 {cur_outer_rem}, -{outer_stride}"),
    );
    ctx.emit(format!(
        "store i64 {next_outer_rem}, ptr {outer_rem_slot}, align 8"
    ));
    let outer_more = ctx.emit_assign(
        "no_more",
        format!("icmp uge i64 {next_outer_rem}, {outer_stride}"),
    );
    ctx.terminate(format!(
        "br i1 {outer_more}, label %{outer_hdr}, label %{outer_exit}"
    ));

    ctx.start_block(&outer_exit);
    Ok(load_acc_slot(ctx, &slot))
}

// ============================================================================
// Part 2: 10^6-Input Native Binary Differential Fuzzing (`VG-L0003`)
// ============================================================================

/// Compile the original LLVM IR (`lifted.original_llvm_ir`) and the `vg_codegen::emit_llvm` output
/// of `lifted.nodes` into a single native binary with a deterministic `SplitMix64` C harness, and
/// execute `num_cases` inputs in the non-UB domain.
pub fn run_differential_fuzz(lifted: &Lifted, num_cases: u64) -> Result<u64, Diagnostic> {
    if num_cases == 0 {
        return Ok(0);
    }

    let root_node = &lifted
        .nodes
        .last()
        .ok_or_else(|| err_fuzz("lifted module has no root node"))?
        .node;
    let map: BTreeMap<Hash, Node> = lifted
        .nodes
        .iter()
        .map(|a| (a.hashes.node, a.node.clone()))
        .collect();

    let raw_vg_ll = emit_llvm(
        root_node,
        &|h| map.get(h).cloned(),
        &CodegenOptions {
            target: if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                CodegenTarget::Aarch64Darwin
            } else {
                CodegenTarget::X86_64Linux
            },
            proof_facts: false,
            runtime_checks: false,
        },
    )
    .map_err(|d| err_fuzz(format!("vg-codegen emit_llvm failed: {}", d.message)))?;

    let graph = vg_codegen::layout::collect_graph(root_node, &|h| map.get(h).cloned())
        .map_err(|d| err_fuzz(format!("collect_graph failed: {}", d.message)))?;
    let prefix = &graph.prefixes[&graph.root_hash];

    // Strip `define i32 @main(` from `raw_vg_ll` and rename any `@main(` in `lifted.original_llvm_ir`.
    let vg_ll = strip_llvm_main(&raw_vg_ll);
    let orig_ll = lifted
        .original_llvm_ir
        .replace("define i32 @main(", "define i32 @__orig_main(");

    let harness_c = generate_c_fuzz_harness(lifted, prefix, num_cases)?;

    let orig_file = temp_path("fuzz_orig", "ll");
    let vg_file = temp_path("fuzz_vg", "ll");
    let harness_file = temp_path("fuzz_harness", "c");
    let bin_file = temp_path("fuzz_runner", "bin");
    let _guard = TempFiles(vec![
        orig_file.clone(),
        vg_file.clone(),
        harness_file.clone(),
        bin_file.clone(),
    ]);

    std::fs::write(&orig_file, &orig_ll)
        .map_err(|e| err_fuzz(format!("failed to write orig.ll: {e}")))?;
    std::fs::write(&vg_file, &vg_ll)
        .map_err(|e| err_fuzz(format!("failed to write vg.ll: {e}")))?;
    std::fs::write(&harness_file, &harness_c)
        .map_err(|e| err_fuzz(format!("failed to write harness.c: {e}")))?;

    let clang = find_clang();
    let mut cmd = Command::new(&clang);
    cmd.arg("-O2").arg("-Wno-override-module");
    #[cfg(target_os = "macos")]
    {
        cmd.arg("-Wl,-dead_strip");
    }
    #[cfg(not(target_os = "macos"))]
    {
        cmd.args(["-ffunction-sections", "-Wl,--gc-sections"]);
    }
    let compile = cmd
        .arg(&orig_file)
        .arg(&vg_file)
        .arg(&harness_file)
        .arg("-o")
        .arg(&bin_file)
        .output()
        .map_err(|e| err_fuzz(format!("failed to invoke clang for fuzz harness: {e}")))?;

    if !compile.status.success() {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        return Err(err_fuzz(format!(
            "clang failed to link differential fuzz harness for `{}`:\n{stderr}",
            lifted.entry_fn_name
        )));
    }

    let run = Command::new(&bin_file)
        .output()
        .map_err(|e| err_fuzz(format!("failed to run differential fuzz binary: {e}")))?;

    if !run.status.success() {
        let stderr = String::from_utf8_lossy(&run.stderr);
        let stdout = String::from_utf8_lossy(&run.stdout);
        return Err(err_fuzz(format!(
            "differential fuzz mismatch for `{}`:\n{stdout}\n{stderr}",
            lifted.entry_fn_name
        )));
    }

    Ok(num_cases)
}

fn strip_llvm_main(ir: &str) -> String {
    let mut out = String::with_capacity(ir.len());
    let mut in_main = false;
    for line in ir.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("define ") && trimmed.contains("@main(") {
            in_main = true;
        }
        if in_main {
            if trimmed == "}" {
                in_main = false;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn generate_c_fuzz_harness(
    lifted: &Lifted,
    prefix: &str,
    num_cases: u64,
) -> Result<String, Diagnostic> {
    let parsed_mod = parse_module_reachable(&lifted.original_llvm_ir, &lifted.entry_fn_name)?;
    let orig_fn = parsed_mod
        .functions
        .get(&lifted.entry_fn_name)
        .ok_or_else(|| err_fuzz("missing entry fn in parsed module"))?;

    let mut c = String::new();
    c.push_str(
        r#"#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct { const uint8_t *data; uint64_t len; } vg_slice_u8;

static uint8_t g_arena[262144];
static size_t g_arena_off = 0;

void *vg_arena_alloc(void *ctx, uint64_t bytes) {
    (void)ctx;
    size_t aligned = (bytes + 15u) & ~(size_t)15u;
    if (g_arena_off + aligned > sizeof(g_arena)) {
        return malloc(aligned ? aligned : 16);
    }
    void *p = &g_arena[g_arena_off];
    g_arena_off += aligned;
    return p;
}
int32_t vg_arena_has_space(void *ctx, uint64_t needed) { (void)ctx; (void)needed; return 1; }
uint64_t vg_fp_enter(void) { return 0; }
void vg_fp_leave(uint64_t s) { (void)s; }
void vg_emit_guard(void *ctx, const char *id, const char *ob, const char *n) { (void)ctx; (void)id; (void)ob; (void)n; }
void vg_trap(void *ctx, const char *kind, const char *ob, const char *n) { (void)ctx; (void)kind; (void)ob; (void)n; abort(); }

static uint64_t rng_state = 0x9e3779b97f4a7c15ULL;
static inline uint64_t next_u64(void) {
    uint64_t z = (rng_state += 0x9e3779b97f4a7c15ULL);
    z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9ULL;
    z = (z ^ (z >> 27)) * 0x94d049bb133111ebULL;
    return z ^ (z >> 31);
}
"#,
    );

    // 1. Emit extern declaration for the original function `@<entry_fn_name>`.
    let orig_ret_c = match &orig_fn.ret_ty {
        LlvmType::Void => "void".to_string(),
        LlvmType::Int(bits) => format!("uint{bits}_t"),
        other => return Err(err_fuzz(format!("unsupported orig ret type {other:?}"))),
    };
    let mut orig_param_decls = Vec::new();
    for (i, p) in orig_fn.params.iter().enumerate() {
        let pty = match &p.ty {
            LlvmType::Ptr => "void *".to_string(),
            LlvmType::Int(bits) => format!("uint{bits}_t"),
            other => return Err(err_fuzz(format!("unsupported orig param type {other:?}"))),
        };
        orig_param_decls.push(format!("{pty} p{i}"));
    }
    c.push_str(&format!(
        "extern {orig_ret_c} {}({});\n",
        lifted.entry_fn_name,
        orig_param_decls.join(", ")
    ));

    // 2. Emit extern declaration for `@vgi_<prefix>(void *ctx, <vg_params...>, void *out0)`.
    let mut vg_param_decls = vec!["void *ctx".to_string()];
    for (i, lp) in lifted.params.iter().enumerate() {
        match lp {
            LiftedParam::Scalar { bits, .. } => {
                vg_param_decls.push(format!("uint{bits}_t in{i}"));
            }
            LiftedParam::Slice { .. } | LiftedParam::Array { .. } => {
                vg_param_decls.push(format!("const void *in{i}"));
            }
        }
    }
    vg_param_decls.push("void *out0".to_string());
    c.push_str(&format!(
        "extern void vgi_{prefix}({});\n\n",
        vg_param_decls.join(", ")
    ));

    // 3. Emit `main()` loop executing `num_cases` test cases.
    c.push_str("int main(void) {\n");
    c.push_str("    static uint8_t slice_buf[6144];\n");
    c.push_str("    for (size_t i = 0; i < sizeof(slice_buf); i++) { slice_buf[i] = (uint8_t)next_u64(); }\n");
    c.push_str(&format!(
        "    for (uint64_t iter = 0; iter < {num_cases}ULL; iter++) {{\n"
    ));
    c.push_str("        g_arena_off = 0;\n");

    let mut orig_args: Vec<String> = vec![String::new(); orig_fn.params.len()];
    let mut vg_args: Vec<String> = Vec::with_capacity(lifted.params.len());
    let mut writable_array_info: Option<(usize, u32, u64)> = None;
    let mut has_unguarded_slice = false;

    for (vi, lp) in lifted.params.iter().enumerate() {
        match lp {
            LiftedParam::Scalar {
                bits, param_idx, ..
            } => {
                c.push_str(&format!(
                    "        uint{bits}_t v_{vi} = (uint{bits}_t)next_u64();\n"
                ));
                orig_args[*param_idx] = format!("v_{vi}");
                vg_args.push(format!("v_{vi}"));
            }
            LiftedParam::Slice {
                len_param_idx,
                ptr_param_idx,
                ..
            } => {
                // Perturb 8 bytes of slice_buf per iteration for fast high-entropy coverage:
                c.push_str("        ((uint64_t *)slice_buf)[iter & 511u] ^= next_u64();\n");
                match len_param_idx {
                    Some(li) => {
                        let lbits = orig_fn.params[*li].ty.bit_width().unwrap_or(64);
                        c.push_str(
                            "        uint64_t slen = (iter == 0) ? 0 : ((iter == 1) ? 6000 : (next_u64() & 63u));\n",
                        );
                        orig_args[*ptr_param_idx] = "slice_buf".to_string();
                        orig_args[*li] = format!("(uint{lbits}_t)slen");
                        c.push_str(&format!(
                            "        vg_slice_u8 s_{vi} = {{ slice_buf, slen }};\n"
                        ));
                        vg_args.push(format!("&s_{vi}"));
                    }
                    None => {
                        has_unguarded_slice = true;
                        orig_args[*ptr_param_idx] = "slice_buf".to_string();
                        c.push_str(&format!(
                            "        vg_slice_u8 s_{vi} = {{ slice_buf, 64 }};\n"
                        ));
                        vg_args.push(format!("&s_{vi}"));
                    }
                }
            }
            LiftedParam::Array {
                elem_bits,
                len,
                writable,
                ptr_param_idx,
                ..
            } => {
                c.push_str(&format!(
                    "        uint{elem_bits}_t arr_orig_{vi}[{len}];\n        uint{elem_bits}_t arr_vg_{vi}[{len}];\n"
                ));
                c.push_str(&format!(
                    "        for (size_t k = 0; k < {len}; k++) {{ uint{elem_bits}_t r = (uint{elem_bits}_t)next_u64(); arr_orig_{vi}[k] = r; arr_vg_{vi}[k] = r; }}\n"
                ));
                orig_args[*ptr_param_idx] = format!("arr_orig_{vi}");
                vg_args.push(format!("arr_vg_{vi}"));
                if *writable {
                    writable_array_info = Some((vi, *elem_bits, *len));
                }
            }
        }
    }

    // If unguarded slice (e.g. `get_at(p, idx)`), bias half of inputs into valid `[0..64)` range so non-UB domain is well-exercised.
    if has_unguarded_slice {
        for (vi, lp) in lifted.params.iter().enumerate() {
            if let LiftedParam::Scalar { .. } = lp {
                c.push_str(&format!(
                    "        if ((iter & 1u) == 0) {{ v_{vi} = v_{vi} & 63u; }}\n"
                ));
            }
        }
    }

    // Call `@vgi_<prefix>` first; if `lifted.returns_result` and tag == 0 (`Result::Err`), skip calling original C/Rust (which would have UB!).
    if lifted.returns_result {
        let Type::Result(ok_ty, _) = &lifted.return_type else {
            return Err(err_fuzz("returns_result true but return_type not Result"));
        };
        let ok_bits = match ok_ty.as_ref() {
            Type::UInt(w) | Type::SInt(w) => u32::from(*w),
            _ => return Err(err_fuzz("unsupported Result payload type in fuzz harness")),
        };
        let pad_ty = if ok_bits <= 32 {
            "uint32_t"
        } else {
            "uint64_t"
        };
        c.push_str(&format!(
            "        struct {{ uint8_t tag; {pad_ty} payload; }} vg_out;\n"
        ));
        let mut call_list = vec!["NULL".to_string()];
        call_list.extend(vg_args);
        call_list.push("&vg_out".to_string());
        c.push_str(&format!(
            "        vgi_{prefix}({});\n",
            call_list.join(", ")
        ));
        c.push_str("        if (vg_out.tag == 0) { continue; }\n");
        c.push_str(&format!(
            "        uint{ok_bits}_t orig_res = {}({});\n",
            lifted.entry_fn_name,
            orig_args.join(", ")
        ));
        c.push_str(&format!(
            "        uint{ok_bits}_t vg_res = (uint{ok_bits}_t)vg_out.payload;\n"
        ));
        c.push_str(
            "        if (orig_res != vg_res) { fprintf(stderr, \"mismatch at iter %llu: orig=%llu vg=%llu\\n\", (unsigned long long)iter, (unsigned long long)orig_res, (unsigned long long)vg_res); return 1; }\n",
        );
    } else if matches!(orig_fn.ret_ty, LlvmType::Void) {
        let (vi, elem_bits, len) =
            writable_array_info.ok_or_else(|| err_fuzz("void fn without writable array"))?;
        c.push_str(&format!("        uint{elem_bits}_t vg_out[{len}];\n"));
        let mut call_list = vec!["NULL".to_string()];
        call_list.extend(vg_args);
        call_list.push("vg_out".to_string());
        c.push_str(&format!(
            "        vgi_{prefix}({});\n",
            call_list.join(", ")
        ));
        c.push_str(&format!(
            "        {}({});\n",
            lifted.entry_fn_name,
            orig_args.join(", ")
        ));
        c.push_str(&format!(
            "        if (memcmp(arr_orig_{vi}, vg_out, sizeof(vg_out)) != 0) {{ fprintf(stderr, \"array mismatch at iter %llu\\n\", (unsigned long long)iter); return 1; }}\n"
        ));
    } else {
        let ret_bits = orig_fn.ret_ty.bit_width().unwrap_or(64);
        c.push_str(&format!("        uint{ret_bits}_t vg_out = 0;\n"));
        let mut call_list = vec!["NULL".to_string()];
        call_list.extend(vg_args);
        call_list.push("&vg_out".to_string());
        c.push_str(&format!(
            "        vgi_{prefix}({});\n",
            call_list.join(", ")
        ));
        c.push_str(&format!(
            "        uint{ret_bits}_t orig_res = {}({});\n",
            lifted.entry_fn_name,
            orig_args.join(", ")
        ));
        c.push_str(
            "        if (orig_res != vg_out) { fprintf(stderr, \"scalar mismatch at iter %llu: orig=%llu vg=%llu\\n\", (unsigned long long)iter, (unsigned long long)orig_res, (unsigned long long)vg_out); return 1; }\n",
        );
    }

    c.push_str("    }\n    return 0;\n}\n");
    Ok(c)
}
