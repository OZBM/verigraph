//! Node-level type checker, effect & handle checker, Eff linearity, slice escape & resource accounting.

use std::collections::BTreeMap;

use vg_core::canon;
use vg_core::diag::{Diagnostic, Span};
use vg_core::float;
use vg_core::ir::{Const, Hash, Node, Op, Operand};
use vg_core::opcode::{self, OpInfo, Shape};
use vg_core::types::{EffectMask, Handle, Type};

use crate::Summary;
use crate::cost::{
    CostExpr, MAX_STATIC_BYTES, MAX_STATIC_ELEMS, MAX_TYPE_DEPTH, abi_size_align,
    collection_len_expr, const_u64, fixed_byte_size, is_fixed_size, static_element_count, val_expr,
};
use crate::formula;

const EFF_ARENA: u32 = EffectMask::ARENA;
const EFF_CLOCK: u32 = 0x0002;
const EFF_FILE_READ: u32 = 0x0004;
const EFF_FILE_WRITE: u32 = 0x0008;
const EFF_NET_READ: u32 = 0x0010;
const EFF_NET_WRITE: u32 = 0x0020;
const EFF_GPU: u32 = 0x0040;

#[derive(Clone)]
pub(crate) struct CalleeInfo {
    pub node: Node,
    pub ops_expr: CostExpr,
    pub arena_bytes: CostExpr,
    pub used_effects: EffectMask,
    /// For each result slot `s`, `result_views_param[s][p]` is true when returning slot `s`
    /// can contain a slice viewing parameter `p` (docs/13: "whose slice results may view their slice arguments").
    pub result_views_param: Vec<Vec<bool>>,
    /// For each result slot `s`, `result_handle_param[s]` tells whether that Handle result
    /// passes through a Handle parameter from input `p`.
    pub result_handle_valid: Vec<Option<Handle>>,
}

#[derive(Clone)]
struct ValMeta {
    ty: Type,
    /// True if this value is a fixed-size value freshly computed in the node's own frame
    /// (never Input or Const).
    origin_local_frame: bool,
    /// True if this value is or contains a Slice that views a local fixed-size frame value.
    has_local_slice: bool,
    /// True if this value is a Slice directly allocated in the arena by a single op (`SIMD_MAP` slice,
    /// `GENERATE` dynamic, `CONCAT` slice, `UPDATE` slice).
    owns_fresh_arena_slice: bool,
    /// For each parameter index `p` of the current node, whether this value (or a slice inside it)
    /// may view/alias parameter `p`.
    from_params: Vec<bool>,
    /// Reachable handle provenance if `ty` is `Type::Handle(h)`.
    valid_handle: Option<Handle>,
    /// Known symbolic element count if `ty` is a collection (`Array`, `Tensor`, or `Slice`).
    len_expr: Option<CostExpr>,
}

fn diag(code: &'static str, op_idx: Option<usize>, msg: impl Into<String>) -> Diagnostic {
    let mut d = Diagnostic::error(code, Span::default(), msg);
    if let Some(i) = op_idx {
        d.op = Some(format!("op {i}"));
    }
    d
}

type MetaLookup<'a> = &'a dyn Fn(Operand, &[Vec<ValMeta>]) -> Option<ValMeta>;

fn contains_eff_or_handle(ty: &Type) -> bool {
    match ty {
        Type::Eff | Type::Handle(_) => true,
        Type::Array(e, _) | Type::Slice(e) | Type::Tensor(e, _) => contains_eff_or_handle(e),
        Type::Tuple(ms) => ms.iter().any(contains_eff_or_handle),
        Type::Result(a, b) => contains_eff_or_handle(a) || contains_eff_or_handle(b),
        _ => false,
    }
}

fn type_contains_slice(ty: &Type) -> bool {
    match ty {
        Type::Slice(_) => true,
        Type::Array(e, _) | Type::Tensor(e, _) => type_contains_slice(e),
        Type::Tuple(ms) => ms.iter().any(type_contains_slice),
        Type::Result(a, b) => type_contains_slice(a) || type_contains_slice(b),
        _ => false,
    }
}

fn is_valid_scalar(ty: &Type) -> bool {
    matches!(
        ty,
        Type::UInt(8 | 16 | 32 | 64)
            | Type::SInt(8 | 16 | 32 | 64)
            | Type::Float(16 | 32 | 64)
            | Type::BFloat16
    )
}

fn is_f16_or_bf16(ty: &Type) -> bool {
    matches!(ty, Type::Float(16) | Type::BFloat16)
}

fn is_mvp_arith_float(ty: &Type) -> bool {
    matches!(ty, Type::Float(32 | 64))
}

fn has_byte_form(ty: &Type) -> bool {
    match ty {
        Type::Slice(e) => e.byte_size().is_some(),
        _ => ty.byte_size().is_some(),
    }
}

/// Validates structural well-formedness, nesting depth, static limits, and `Eff`/`Handle` placement
/// of a type. `allow_top_eff_handle` is true only at top-level signature parameters/results and
/// top-level op result slots for `Eff`/`Handle`.
fn validate_type(
    ty: &Type,
    allow_top_eff_handle: bool,
    depth: usize,
    op_idx: Option<usize>,
) -> Result<(), Diagnostic> {
    if depth > MAX_TYPE_DEPTH {
        return Err(diag(
            "VG-T0118",
            op_idx,
            format!("type nested deeper than {MAX_TYPE_DEPTH} levels"),
        ));
    }
    match ty {
        Type::Bool | Type::BFloat16 => Ok(()),
        Type::UInt(8 | 16 | 32 | 64) | Type::SInt(8 | 16 | 32 | 64) => Ok(()),
        Type::Float(16 | 32 | 64) => Ok(()),
        Type::UInt(_) | Type::SInt(_) | Type::Float(_) => Err(diag(
            "VG-T0101",
            op_idx,
            format!("invalid scalar bit width in {ty}"),
        )),
        Type::Int => Err(diag(
            "VG-T0101",
            op_idx,
            "unbounded Int type may only appear on formula constants",
        )),
        Type::Arena(_) => Err(diag(
            "VG-S0104",
            op_idx,
            "Arena<N> is [ALPHA] and not available in the MVP",
        )),
        Type::Eff | Type::Handle(_) => {
            if allow_top_eff_handle {
                Ok(())
            } else {
                Err(diag(
                    "VG-T0119",
                    op_idx,
                    format!("{ty} is not allowed inside a composite type or constant"),
                ))
            }
        }
        Type::Array(e, _) => {
            validate_type(e, false, depth + 1, op_idx)?;
            check_static_size(ty, op_idx)
        }
        Type::Slice(e) => validate_type(e, false, depth + 1, op_idx),
        Type::Tensor(e, dims) => {
            if dims.is_empty() {
                return Err(diag(
                    "VG-T0101",
                    op_idx,
                    "Tensor must have at least one dimension",
                ));
            }
            validate_type(e, false, depth + 1, op_idx)?;
            check_static_size(ty, op_idx)
        }
        Type::Tuple(ms) => {
            if ms.is_empty() {
                return Err(diag(
                    "VG-T0101",
                    op_idx,
                    "Tuple must have at least one member",
                ));
            }
            for m in ms {
                validate_type(m, false, depth + 1, op_idx)?;
            }
            check_static_size(ty, op_idx)
        }
        Type::Result(ok, err) => {
            validate_type(ok, false, depth + 1, op_idx)?;
            validate_type(err, false, depth + 1, op_idx)?;
            check_static_size(ty, op_idx)
        }
    }
}

fn check_static_size(ty: &Type, op_idx: Option<usize>) -> Result<(), Diagnostic> {
    if let Some(elems) = static_element_count(ty)
        && elems > MAX_STATIC_ELEMS
    {
        return Err(diag(
            "VG-T0118",
            op_idx,
            format!("static type {ty} has {elems} elements, above 2^32 - 1"),
        ));
    }
    if matches!(ty, Type::Tensor(_, dims) if dims.iter().try_fold(1u64, |a, &d| a.checked_mul(u64::from(d))).is_none())
    {
        return Err(diag(
            "VG-T0118",
            op_idx,
            format!("tensor element count overflows u64 in {ty}"),
        ));
    }
    if is_fixed_size(ty) {
        match abi_size_align(ty) {
            Some((sz, _)) if sz <= MAX_STATIC_BYTES => Ok(()),
            _ => Err(diag(
                "VG-T0118",
                op_idx,
                format!("fixed-size type {ty} exceeds the 16 MiB static limit"),
            )),
        }
    } else if matches!(ty, Type::Array(..) | Type::Tensor(..)) {
        // Non-fixed-size static arrays/tensors (e.g. Array<Slice<u8>, N>) must still fit within 16 MiB stack frame.
        match abi_size_align(ty) {
            Some((sz, _)) if sz <= MAX_STATIC_BYTES => Ok(()),
            _ => Err(diag(
                "VG-T0118",
                op_idx,
                format!("static type {ty} exceeds the 16 MiB static limit"),
            )),
        }
    } else {
        Ok(())
    }
}

fn check_const_valid(c: &Const) -> Result<(), Diagnostic> {
    if c.ty == Type::Int {
        let ok = vg_core::lower::int_value(&c.bytes)
            .is_some_and(|v| c.bytes == vg_core::lower::int_const(v).bytes);
        if !ok {
            return Err(diag(
                "VG-S0204",
                None,
                "Int constant encoding is not canonical",
            ));
        }
        return Ok(());
    }
    validate_type(&c.ty, false, 1, None)?;
    if c.ty.byte_size() != Some(c.bytes.len() as u64) {
        return Err(diag(
            "VG-S0204",
            None,
            format!(
                "constant of type {} has {} bytes, expected {:?}",
                c.ty,
                c.bytes.len(),
                c.ty.byte_size()
            ),
        ));
    }
    check_const_bytes(&c.ty, &c.bytes)
}

fn check_const_bytes(ty: &Type, bytes: &[u8]) -> Result<(), Diagnostic> {
    match ty {
        Type::Bool => {
            if bytes[0] > 1 {
                return Err(diag("VG-S0204", None, "Bool constant byte must be 0 or 1"));
            }
        }
        t if t.is_float() => {
            let mut w = [0u8; 8];
            w[..bytes.len()].copy_from_slice(bytes);
            let bits = u64::from_le_bytes(w);
            if float::is_nan(t, bits) && bits != float::canonical_nan(t) {
                return Err(diag(
                    "VG-S0204",
                    None,
                    "non-canonical NaN in float constant",
                ));
            }
        }
        Type::Array(..) | Type::Tensor(..) => {
            let elem = ty.elem().expect("array or tensor has element");
            let sz = elem.byte_size().unwrap_or(0) as usize;
            if sz > 0 {
                for chunk in bytes.chunks(sz) {
                    check_const_bytes(elem, chunk)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn required_handle_effect_ok(h: Handle, mask: EffectMask) -> Result<(), &'static str> {
    match h {
        Handle::FileIn => (mask.0 & EFF_FILE_READ != 0)
            .then_some(())
            .ok_or("FILE_READ"),
        Handle::FileOut => (mask.0 & EFF_FILE_WRITE != 0)
            .then_some(())
            .ok_or("FILE_WRITE"),
        Handle::Clock => (mask.0 & EFF_CLOCK != 0).then_some(()).ok_or("CLOCK"),
        Handle::Socket => ((mask.0 & (EFF_NET_READ | EFF_NET_WRITE)) != 0)
            .then_some(())
            .ok_or("NET_READ or NET_WRITE"),
        Handle::GpuQueue => (mask.0 & EFF_GPU != 0).then_some(()).ok_or("GPU"),
    }
}

fn is_valid_cast(src: &Type, dst: &Type) -> bool {
    if src == dst {
        return false;
    }
    if !is_valid_scalar(dst) {
        return false;
    }
    match src {
        Type::Bool => dst.is_int(),
        t if is_valid_scalar(t) => true,
        _ => false,
    }
}

pub(crate) fn check_node_entry(
    node: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Result<Summary, Vec<Diagnostic>> {
    let mut cache = BTreeMap::new();
    let mut visiting = Vec::new();
    if let Ok(h) = canon::hashes(node) {
        visiting.push(h.node);
    }
    let mut diags = Vec::new();
    let info = check_node_cached(node, callee, &mut cache, &mut visiting, &mut diags);
    if diags.is_empty() {
        Ok(Summary {
            effects: node.effects,
            cost_ops: info.ops_expr.lower_bound(),
            ops_expr: info.ops_expr,
            arena_bytes: info.arena_bytes,
            heap_bytes: 0,
            used_effects: info.used_effects,
        })
    } else {
        Err(diags)
    }
}

pub(crate) fn check_node_with_cache(
    node: &Node,
    self_hash: Hash,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    cache: &mut BTreeMap<Hash, CalleeInfo>,
) -> Result<Summary, Vec<Diagnostic>> {
    let mut visiting = vec![self_hash];
    let mut diags = Vec::new();
    let info = check_node_cached(node, callee, cache, &mut visiting, &mut diags);
    cache.insert(self_hash, info.clone());
    if diags.is_empty() {
        Ok(Summary {
            effects: node.effects,
            cost_ops: info.ops_expr.lower_bound(),
            ops_expr: info.ops_expr,
            arena_bytes: info.arena_bytes,
            heap_bytes: 0,
            used_effects: info.used_effects,
        })
    } else {
        Err(diags)
    }
}

fn resolve_callees(
    node: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    cache: &mut BTreeMap<Hash, CalleeInfo>,
    visiting: &mut Vec<Hash>,
    diags: &mut Vec<Diagnostic>,
) {
    for &h in &node.calls {
        if visiting.contains(&h) {
            diags.push(diag(
                "VG-S0007",
                None,
                "recursion or call cycle detected; REC_CALL is [ALPHA]",
            ));
            continue;
        }
        if cache.contains_key(&h) {
            continue;
        }
        let Some(c_node) = callee(&h) else {
            diags.push(diag(
                "VG-S0009",
                None,
                format!("unresolvable callee hash #{}", &vg_core::ir::hex(&h)[..12]),
            ));
            continue;
        };
        visiting.push(h);
        let mut sub_diags = Vec::new();
        let c_info = check_node_cached(&c_node, callee, cache, visiting, &mut sub_diags);
        visiting.pop();
        cache.insert(h, c_info);
        diags.extend(sub_diags);
    }
}

fn check_node_cached(
    node: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    cache: &mut BTreeMap<Hash, CalleeInfo>,
    visiting: &mut Vec<Hash>,
    diags: &mut Vec<Diagnostic>,
) -> CalleeInfo {
    let n_params = node.params.len();
    let empty_info = || CalleeInfo {
        node: node.clone(),
        ops_expr: CostExpr::zero(),
        arena_bytes: CostExpr::zero(),
        used_effects: EffectMask(0),
        result_views_param: vec![vec![false; n_params]; node.results.len()],
        result_handle_valid: vec![None; node.results.len()],
    };

    // 1. Resolve and validate all callees transitively.
    resolve_callees(node, callee, cache, visiting, diags);

    // 2. Effect mask checks & signature checks.
    if node.effects.0 & EffectMask::RESERVED != 0 {
        diags.push(diag(
            "VG-E0105",
            None,
            format!("reserved effect bit set in mask {:#06x}", node.effects.0),
        ));
    }
    if node.spec && !node.effects.is_pure() {
        diags.push(diag(
            "VG-E0101",
            None,
            "a SPEC node must be pure (non-ARENA effects are forbidden)",
        ));
    }

    for (idx, t) in node.params.iter().enumerate() {
        if let Err(d) = validate_type(t, true, 1, None) {
            diags.push(d);
        }
        if *t == Type::Eff && idx != 0 {
            let code = if node.effects.is_pure() {
                "VG-T0119"
            } else {
                "VG-E0103"
            };
            diags.push(diag(
                code,
                None,
                format!("Eff parameter may only appear at parameter 0, not {idx}"),
            ));
        }
        if let Type::Handle(hk) = t
            && let Err(req) = required_handle_effect_ok(*hk, node.effects)
        {
            diags.push(diag(
                "VG-E0104",
                None,
                format!(
                    "handle parameter `{}` requires effect {req} in EFFECTS",
                    hk.name()
                ),
            ));
        }
    }
    for (idx, t) in node.results.iter().enumerate() {
        if let Err(d) = validate_type(t, true, 1, None) {
            diags.push(d);
        }
        if *t == Type::Eff && idx != 0 {
            let code = if node.effects.is_pure() {
                "VG-T0119"
            } else {
                "VG-E0103"
            };
            diags.push(diag(
                code,
                None,
                format!("Eff result may only appear at result 0, not {idx}"),
            ));
        }
    }

    if !node.effects.is_pure() {
        let first_p = node.params.first() == Some(&Type::Eff);
        let first_r = node.results.first() == Some(&Type::Eff);
        if !first_p || !first_r {
            diags.push(diag(
                "VG-E0103",
                None,
                "effectful node must have Eff as its first parameter and first result",
            ));
        }
    } else {
        if node.params.first() == Some(&Type::Eff) {
            diags.push(diag(
                "VG-T0119",
                None,
                "pure node cannot take an Eff parameter",
            ));
        }
        if node.results.first() == Some(&Type::Eff) {
            diags.push(diag(
                "VG-T0119",
                None,
                "pure node cannot return an Eff result",
            ));
        }
    }

    // 3. CONSTS checks.
    for c in &node.consts {
        if let Err(d) = check_const_valid(c) {
            diags.push(d);
        }
    }

    // 4. LEARNABLE checks (VG-T0117).
    for (li, l) in node.learnable.iter().enumerate() {
        if !l.ty.is_float() || !is_valid_scalar(&l.ty) {
            diags.push(diag(
                "VG-T0117",
                None,
                format!("LEARNABLE entry {li} must be a float type, found {}", l.ty),
            ));
            continue;
        }
        let read_bound = |ci: u32| -> Result<f64, Diagnostic> {
            let c = node.consts.get(ci as usize).ok_or_else(|| {
                diag(
                    "VG-S0203",
                    None,
                    format!("LEARNABLE bound index {ci} out of range"),
                )
            })?;
            if c.ty != l.ty || c.ty.byte_size() != Some(c.bytes.len() as u64) {
                return Err(diag(
                    "VG-T0117",
                    None,
                    format!(
                        "LEARNABLE bound constant type {} does not match {}",
                        c.ty, l.ty
                    ),
                ));
            }
            let mut b = [0u8; 8];
            b[..c.bytes.len()].copy_from_slice(&c.bytes);
            let bits = u64::from_le_bytes(b);
            if float::is_nan(&c.ty, bits) || float::is_inf(&c.ty, bits) {
                return Err(diag(
                    "VG-T0117",
                    None,
                    "LEARNABLE bound must be finite (not NaN or infinity)",
                ));
            }
            Ok(float::to_f64(&c.ty, bits))
        };
        match (read_bound(l.lo), read_bound(l.hi)) {
            (Ok(lo), Ok(hi)) if lo > hi => {
                diags.push(diag(
                    "VG-T0117",
                    None,
                    format!("LEARNABLE entry {li}: lower bound {lo} exceeds upper bound {hi}"),
                ));
            }
            (Err(d), _) | (_, Err(d)) => diags.push(d),
            _ => {}
        }
        if let Some(ci) = l.loss {
            match node.calls.get(ci as usize).and_then(|h| cache.get(h)) {
                None => {
                    if (ci as usize) >= node.calls.len() {
                        diags.push(diag(
                            "VG-S0203",
                            None,
                            format!("LEARNABLE loss index {ci} out of range"),
                        ));
                    }
                }
                Some(loss_c) => {
                    if loss_c.node.spec {
                        diags.push(diag(
                            "VG-S0110",
                            None,
                            "LEARNABLE loss must be a NODE, not a SPEC",
                        ));
                    }
                    if loss_c.node.effects.0 != 0 {
                        diags.push(diag(
                            "VG-T0107",
                            None,
                            "LEARNABLE loss node must be completely pure (empty EFFECTS)",
                        ));
                    }
                }
            }
        }
    }

    // 5. Count runtime uses of every Input and Op result across all ops and outputs.
    let mut use_counts: BTreeMap<Operand, u32> = BTreeMap::new();
    for op in &node.ops {
        for &o in &op.operands {
            if matches!(o, Operand::Input(_) | Operand::Op(_, _)) {
                *use_counts.entry(o).or_default() += 1;
            }
        }
    }
    for &o in &node.outputs {
        if matches!(o, Operand::Input(_) | Operand::Op(_, _)) {
            *use_counts.entry(o).or_default() += 1;
        }
    }

    // 6. Walk ops in order, checking structure, types, effects, handles, escape, and resource cost.
    let mut op_metas: Vec<Vec<ValMeta>> = Vec::with_capacity(node.ops.len());
    let mut total_ops = CostExpr::zero();
    let mut total_arena = CostExpr::zero();
    let mut used_effects: u32 = 0;

    let get_meta = |o: Operand, op_metas: &[Vec<ValMeta>]| -> Option<ValMeta> {
        match o {
            Operand::Input(idx) => {
                let ty = node.input_type(idx)?.clone();
                let mut from_params = vec![false; n_params];
                if (idx as usize) < n_params {
                    from_params[idx as usize] = true;
                }
                let valid_handle = match &ty {
                    Type::Handle(hk) if (idx as usize) < n_params => Some(*hk),
                    _ => None,
                };
                let len_expr = static_element_count(&ty)
                    .map(CostExpr::Const)
                    .or_else(|| matches!(ty, Type::Slice(_)).then(|| CostExpr::Len(o)));
                Some(ValMeta {
                    ty,
                    origin_local_frame: false,
                    has_local_slice: false,
                    owns_fresh_arena_slice: false,
                    from_params,
                    valid_handle,
                    len_expr,
                })
            }
            Operand::Const(ci) => {
                let ty = node.consts.get(ci as usize)?.ty.clone();
                let len_expr = static_element_count(&ty).map(CostExpr::Const);
                Some(ValMeta {
                    ty,
                    origin_local_frame: false,
                    has_local_slice: false,
                    owns_fresh_arena_slice: false,
                    from_params: vec![false; n_params],
                    valid_handle: None,
                    len_expr,
                })
            }
            Operand::Op(idx, slot) => op_metas.get(idx as usize)?.get(slot as usize).cloned(),
            Operand::Callee(_) | Operand::Invariant(_) => None,
        }
    };

    for (i, op) in node.ops.iter().enumerate() {
        // Validate op.results types up front for VG-T0118 / VG-T0119 / VG-S0104.
        for rty in &op.results {
            if let Err(d) = validate_type(rty, true, 1, Some(i)) {
                diags.push(d);
            }
        }

        let Some(info) = opcode::by_code(op.opcode) else {
            diags.push(diag(
                "VG-S0101",
                Some(i),
                format!("unknown opcode {:#06x}", op.opcode),
            ));
            op_metas.push(fallback_op_metas(op, n_params));
            continue;
        };
        if !info.mvp || op.opcode == opcode::IDENTITY {
            diags.push(diag(
                "VG-S0104",
                Some(i),
                format!("{} is not available in the MVP", info.name),
            ));
            op_metas.push(fallback_op_metas(op, n_params));
            continue;
        }
        if info.mode_name(op.mode).is_none() {
            diags.push(diag(
                "VG-S0102",
                Some(i),
                format!("{} has no mode byte {}", info.name, op.mode),
            ));
            op_metas.push(fallback_op_metas(op, n_params));
            continue;
        }
        if !shape_arity_ok(info, op.operands.len()) {
            diags.push(diag(
                "VG-S0103",
                Some(i),
                format!(
                    "{} has wrong operand count {}",
                    info.name,
                    op.operands.len()
                ),
            ));
            op_metas.push(fallback_op_metas(op, n_params));
            continue;
        }

        let mut operands_ok = true;
        for (j, &o) in op.operands.iter().enumerate() {
            if is_callee_slot(info.shape, j, op.operands.len()) {
                if !matches!(o, Operand::Callee(c) if (c as usize) < node.calls.len()) {
                    let code = match o {
                        Operand::Callee(_) => "VG-S0203",
                        _ => "VG-S0109",
                    };
                    diags.push(diag(
                        code,
                        Some(i),
                        format!("{} operand {j} must be a valid callee index", info.name),
                    ));
                    operands_ok = false;
                }
            } else if is_invariant_slot(info.shape, j, op.operands.len()) {
                if !matches!(o, Operand::Invariant(v) if (v as usize) < node.contract.invariants.len())
                {
                    diags.push(diag(
                        "VG-S0107",
                        Some(i),
                        "FOLD_BOUNDED invariant operand must name an INVARIANT of this node",
                    ));
                    operands_ok = false;
                }
            } else if !value_operand_in_range(node, o, i) {
                diags.push(diag(
                    "VG-S0203",
                    Some(i),
                    format!("operand {o:?} out of range or used before definition"),
                ));
                operands_ok = false;
            }
        }
        if !operands_ok {
            op_metas.push(fallback_op_metas(op, n_params));
            continue;
        }

        match check_single_op(
            node,
            i,
            op,
            info,
            &op_metas,
            &get_meta,
            &use_counts,
            cache,
            &mut total_ops,
            &mut total_arena,
            &mut used_effects,
            diags,
        ) {
            Some(metas) => {
                // Compare op.results against expected result types.
                if op.results.len() != metas.len() {
                    diags.push(diag(
                        "VG-S0106",
                        Some(i),
                        format!(
                            "{} produces {} results, but op declares {}",
                            info.name,
                            metas.len(),
                            op.results.len()
                        ),
                    ));
                    op_metas.push(metas);
                } else {
                    let mut all_match = true;
                    for (slot, (got, exp)) in op.results.iter().zip(&metas).enumerate() {
                        if *got != exp.ty {
                            all_match = false;
                            diags.push(diag(
                                "VG-T0115",
                                Some(i),
                                format!(
                                    "op {i} result slot {slot} declared {got}, expected {}",
                                    exp.ty
                                ),
                            ));
                        }
                    }
                    if all_match {
                        op_metas.push(metas);
                    } else {
                        op_metas.push(fallback_op_metas(op, n_params));
                    }
                }
            }
            None => {
                op_metas.push(fallback_op_metas(op, n_params));
            }
        }
    }

    // 7. Check RETURN (`node.outputs`) and slice escape (VG-T0120).
    let mut result_views_param = vec![vec![false; n_params]; node.results.len()];
    let mut result_handle_valid = vec![None; node.results.len()];
    if node.outputs.len() != node.results.len() {
        diags.push(diag(
            "VG-T0108",
            None,
            format!(
                "RETURN has {} values; SIGNATURE has {} results",
                node.outputs.len(),
                node.results.len()
            ),
        ));
    } else {
        for (j, (&out_op, res_ty)) in node.outputs.iter().zip(&node.results).enumerate() {
            if !value_operand_in_range(node, out_op, node.ops.len()) {
                diags.push(diag(
                    "VG-S0203",
                    None,
                    format!("output {j} operand {out_op:?} is out of range"),
                ));
                continue;
            }
            if let Some(meta) = get_meta(out_op, &op_metas) {
                if meta.ty != *res_ty {
                    diags.push(diag(
                        "VG-T0108",
                        None,
                        format!("RETURN value {j} has type {}, expected {res_ty}", meta.ty),
                    ));
                }
                if let Type::Handle(hk) = res_ty
                    && meta.valid_handle != Some(*hk)
                {
                    diags.push(diag(
                        "VG-T0119",
                        None,
                        format!(
                            "returned handle {} does not originate from an input handle",
                            hk.name()
                        ),
                    ));
                }
                result_handle_valid[j] = meta.valid_handle;
                if type_contains_slice(res_ty) {
                    if meta.has_local_slice {
                        diags.push(diag(
                            "VG-T0120",
                            None,
                            format!(
                                "returned slice at result {j} may view a value of this node's frame"
                            ),
                        ));
                    }
                    result_views_param[j] = meta.from_params;
                }
            }
        }
    }

    // 8. Check Eff linearity (VG-E0102): every Eff input and every Eff op result used exactly once.
    for (idx, ty) in node.params.iter().enumerate() {
        if *ty == Type::Eff {
            let uses = use_counts
                .get(&Operand::Input(idx as u32))
                .copied()
                .unwrap_or(0);
            if uses != 1 {
                diags.push(diag(
                    "VG-E0102",
                    None,
                    format!("Eff input {idx} is used {uses} times; each Eff must be used once"),
                ));
            }
        }
    }
    for (i, op) in node.ops.iter().enumerate() {
        for (s, ty) in op.results.iter().enumerate() {
            if *ty == Type::Eff {
                let uses = use_counts
                    .get(&Operand::Op(i as u32, s as u8))
                    .copied()
                    .unwrap_or(0);
                if uses != 1 {
                    diags.push(diag(
                        "VG-E0102",
                        Some(i),
                        format!(
                            "Eff result {s} of op {i} is used {uses} times; each Eff must be used once"
                        ),
                    ));
                }
            }
        }
    }

    // 9. Check contract formulas (VG-C0101..C0108).
    formula::check_contract(node, cache, diags);

    let _ = empty_info;
    CalleeInfo {
        node: node.clone(),
        ops_expr: total_ops,
        arena_bytes: total_arena,
        used_effects: EffectMask(used_effects),
        result_views_param,
        result_handle_valid,
    }
}

fn fallback_op_metas(op: &Op, n_params: usize) -> Vec<ValMeta> {
    op.results
        .iter()
        .map(|ty| ValMeta {
            ty: ty.clone(),
            origin_local_frame: is_fixed_size(ty),
            has_local_slice: false,
            owns_fresh_arena_slice: false,
            from_params: vec![false; n_params],
            valid_handle: None,
            len_expr: static_element_count(ty).map(CostExpr::Const),
        })
        .collect()
}

fn shape_arity_ok(info: &OpInfo, k: usize) -> bool {
    match info.shape {
        Shape::Fixed(m) => k == m as usize,
        Shape::AtLeast(m) => k >= m as usize,
        Shape::Kernel => k >= 2,
        Shape::Fold => k >= 4,
        Shape::Guard => k >= 3,
        Shape::Call => k >= 1,
    }
}

fn is_callee_slot(shape: Shape, j: usize, total: usize) -> bool {
    match shape {
        Shape::Kernel | Shape::Fold | Shape::Call => j == 0,
        Shape::Guard => j == 1 || j + 1 == total,
        _ => false,
    }
}

fn is_invariant_slot(shape: Shape, j: usize, total: usize) -> bool {
    shape == Shape::Fold && j + 1 == total
}

fn value_operand_in_range(node: &Node, o: Operand, before_op: usize) -> bool {
    match o {
        Operand::Input(idx) => (idx as usize) < node.input_count(),
        Operand::Op(idx, slot) => {
            (idx as usize) < before_op && (slot as usize) < node.ops[idx as usize].results.len()
        }
        Operand::Const(c) => node
            .consts
            .get(c as usize)
            .is_some_and(|ct| ct.ty != Type::Int),
        Operand::Callee(_) | Operand::Invariant(_) => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn check_single_op(
    node: &Node,
    i: usize,
    op: &Op,
    info: &OpInfo,
    op_metas: &[Vec<ValMeta>],
    get_meta: MetaLookup<'_>,
    use_counts: &BTreeMap<Operand, u32>,
    cache: &BTreeMap<Hash, CalleeInfo>,
    total_ops: &mut CostExpr,
    total_arena: &mut CostExpr,
    used_effects: &mut u32,
    diags: &mut Vec<Diagnostic>,
) -> Option<Vec<ValMeta>> {
    let n_params = node.params.len();
    let get = |idx: usize| get_meta(op.operands[idx], op_metas);
    let reject_eff_handle_in = |indices: &[usize], diags: &mut Vec<Diagnostic>| -> bool {
        let mut bad = false;
        for &idx in indices {
            if let Some(m) = get(idx)
                && contains_eff_or_handle(&m.ty)
            {
                diags.push(diag(
                    "VG-T0119",
                    Some(i),
                    format!("{} does not allow Eff or Handle operands", info.name),
                ));
                bad = true;
            }
        }
        bad
    };
    let make_local_meta = |ty: Type, has_local_slice: bool, from_params: Vec<bool>| ValMeta {
        origin_local_frame: is_fixed_size(&ty),
        len_expr: static_element_count(&ty).map(CostExpr::Const),
        ty,
        has_local_slice,
        owns_fresh_arena_slice: false,
        from_params,
        valid_handle: None,
    };

    // Note: ASSERT_GUARD's second callee operand is at the last slot! Let's handle Callee slots cleanly:
    // In canonical & lowered IR, ASSERT_GUARD has operands `[cond, @then, a..., @else]`.
    let lookup_callee = |arg_pos: usize| -> Option<&CalleeInfo> {
        let Operand::Callee(ci) = op.operands[arg_pos] else {
            return None;
        };
        let h = node.calls.get(ci as usize)?;
        cache.get(h)
    };

    match info.name {
        "TUPLE" => {
            let all_idx: Vec<usize> = (0..op.operands.len()).collect();
            if reject_eff_handle_in(&all_idx, diags) {
                return None;
            }
            let ms_meta: Vec<ValMeta> =
                all_idx.iter().map(|&idx| get(idx)).collect::<Option<_>>()?;
            let mut has_local = false;
            let mut from_params = vec![false; n_params];
            let mut tys = Vec::with_capacity(ms_meta.len());
            for m in ms_meta {
                has_local |= m.has_local_slice;
                or_into(&mut from_params, &m.from_params);
                tys.push(m.ty);
            }
            let tup_ty = Type::Tuple(tys);
            if let Err(d) = validate_type(&tup_ty, false, 1, Some(i)) {
                diags.push(d);
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![make_local_meta(tup_ty, has_local, from_params)])
        }

        "PROJ" => {
            if reject_eff_handle_in(&[0, 1], diags) {
                return None;
            }
            let m0 = get(0)?;
            let Type::Tuple(ms) = &m0.ty else {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("PROJ expects a Tuple, found {}", m0.ty),
                ));
                return None;
            };
            let Operand::Const(ci) = op.operands[1] else {
                diags.push(diag(
                    "VG-S0108",
                    Some(i),
                    "PROJ index must be a u32 literal",
                ));
                return None;
            };
            let c = node.consts.get(ci as usize)?;
            if c.ty != Type::UInt(32) || c.bytes.len() != 4 {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("PROJ index must be u32, found {}", c.ty),
                ));
                return None;
            }
            let j = u32::from_le_bytes(c.bytes[..4].try_into().ok()?) as usize;
            let Some(res_ty) = ms.get(j).cloned() else {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("PROJ index {j} out of range for {}", m0.ty),
                ));
                return None;
            };
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![ValMeta {
                ty: res_ty.clone(),
                origin_local_frame: m0.origin_local_frame && is_fixed_size(&res_ty),
                has_local_slice: m0.has_local_slice && type_contains_slice(&res_ty),
                owns_fresh_arena_slice: false,
                from_params: if type_contains_slice(&res_ty) {
                    m0.from_params
                } else {
                    vec![false; n_params]
                },
                valid_handle: None,
                len_expr: static_element_count(&res_ty).map(CostExpr::Const),
            }])
        }

        "SELECT" => {
            if reject_eff_handle_in(&[0, 1, 2], diags) {
                return None;
            }
            let (m0, m1, m2) = (get(0)?, get(1)?, get(2)?);
            if m0.ty != Type::Bool {
                diags.push(diag(
                    "VG-T0111",
                    Some(i),
                    format!("SELECT condition must be Bool, found {}", m0.ty),
                ));
                return None;
            }
            if m1.ty != m2.ty {
                diags.push(diag(
                    "VG-T0102",
                    Some(i),
                    format!(
                        "SELECT branches have different types: {} vs {}",
                        m1.ty, m2.ty
                    ),
                ));
                return None;
            }
            let mut from_params = m1.from_params;
            or_into(&mut from_params, &m2.from_params);
            let len_expr = if m1.len_expr.is_some() && m1.len_expr == m2.len_expr {
                m1.len_expr
            } else {
                static_element_count(&m1.ty).map(CostExpr::Const)
            };
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![ValMeta {
                ty: m1.ty,
                origin_local_frame: m1.origin_local_frame || m2.origin_local_frame,
                has_local_slice: m1.has_local_slice || m2.has_local_slice,
                owns_fresh_arena_slice: false,
                from_params,
                valid_handle: None,
                len_expr,
            }])
        }

        "CAST" => {
            if reject_eff_handle_in(&[0], diags) {
                return None;
            }
            let m0 = get(0)?;
            if op.results.len() != 1 {
                diags.push(diag("VG-S0106", Some(i), "CAST produces exactly 1 result"));
                return None;
            }
            let dst = &op.results[0];
            if !is_valid_cast(&m0.ty, dst) {
                diags.push(diag(
                    "VG-T0113",
                    Some(i),
                    format!("CAST not allowed from {} to {dst}", m0.ty),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![make_local_meta(
                dst.clone(),
                false,
                vec![false; n_params],
            )])
        }

        "AS_BYTES" => {
            if reject_eff_handle_in(&[0], diags) {
                return None;
            }
            let m0 = get(0)?;
            if !has_byte_form(&m0.ty) {
                diags.push(diag(
                    "VG-T0114",
                    Some(i),
                    format!("AS_BYTES has no byte representation for {}", m0.ty),
                ));
                return None;
            }
            let has_local = if matches!(m0.ty, Type::Slice(_)) {
                m0.has_local_slice
            } else {
                m0.origin_local_frame
            };
            let len_expr = match &m0.ty {
                Type::Slice(e) => {
                    let elem_b = e.byte_size().unwrap_or(0);
                    Some(CostExpr::mul(
                        m0.len_expr
                            .unwrap_or_else(|| collection_len_expr(node, op.operands[0])),
                        CostExpr::Const(elem_b),
                    ))
                }
                _ => m0.ty.byte_size().map(CostExpr::Const),
            };
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![ValMeta {
                ty: Type::Slice(Box::new(Type::UInt(8))),
                origin_local_frame: false,
                has_local_slice: has_local,
                owns_fresh_arena_slice: false,
                from_params: m0.from_params,
                valid_handle: None,
                len_expr,
            }])
        }

        "OK" | "ERR" => {
            if reject_eff_handle_in(&[0], diags) {
                return None;
            }
            let m0 = get(0)?;
            if op.results.len() != 1 {
                diags.push(diag(
                    "VG-S0106",
                    Some(i),
                    format!("{} produces exactly 1 result", info.name),
                ));
                return None;
            }
            let Type::Result(ok_ty, err_ty) = &op.results[0] else {
                diags.push(diag(
                    "VG-T0115",
                    Some(i),
                    format!("{} requires a Result<T, E> result type", info.name),
                ));
                return None;
            };
            let want = if info.name == "OK" { ok_ty } else { err_ty };
            if m0.ty != **want {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!(
                        "{} operand has type {}, expected {}",
                        info.name, m0.ty, want
                    ),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![make_local_meta(
                op.results[0].clone(),
                m0.has_local_slice,
                m0.from_params,
            )])
        }

        "IS_OK" | "GET_OK" | "GET_ERR" => {
            if reject_eff_handle_in(&[0], diags) {
                return None;
            }
            let m0 = get(0)?;
            let Type::Result(ok_ty, err_ty) = &m0.ty else {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("{} requires a Result, found {}", info.name, m0.ty),
                ));
                return None;
            };
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            if info.name == "IS_OK" {
                Some(vec![make_local_meta(
                    Type::Bool,
                    false,
                    vec![false; n_params],
                )])
            } else {
                let res_ty = if info.name == "GET_OK" {
                    (**ok_ty).clone()
                } else {
                    (**err_ty).clone()
                };
                Some(vec![ValMeta {
                    ty: res_ty.clone(),
                    origin_local_frame: m0.origin_local_frame && is_fixed_size(&res_ty),
                    has_local_slice: m0.has_local_slice && type_contains_slice(&res_ty),
                    owns_fresh_arena_slice: false,
                    from_params: if type_contains_slice(&res_ty) {
                        m0.from_params
                    } else {
                        vec![false; n_params]
                    },
                    valid_handle: None,
                    len_expr: static_element_count(&res_ty).map(CostExpr::Const),
                }])
            }
        }

        "ADD" | "SUB" | "MUL" | "DIV" | "MOD" | "AND" | "OR" | "XOR" => {
            if reject_eff_handle_in(&[0, 1], diags) {
                return None;
            }
            let (m0, m1) = (get(0)?, get(1)?);
            if m0.ty.is_float() || m1.ty.is_float() {
                diags.push(diag(
                    "VG-T0104",
                    Some(i),
                    format!("integer op {} applied to floating-point operand", info.name),
                ));
                return None;
            }
            if !m0.ty.is_int() || !m1.ty.is_int() {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!(
                        "{} requires integer operands, found {} and {}",
                        info.name, m0.ty, m1.ty
                    ),
                ));
                return None;
            }
            if m0.ty != m1.ty {
                diags.push(diag(
                    "VG-T0102",
                    Some(i),
                    format!(
                        "{} operands have different types: {} vs {}",
                        info.name, m0.ty, m1.ty
                    ),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![make_local_meta(m0.ty, false, vec![false; n_params])])
        }

        "NEG" | "NOT" => {
            if reject_eff_handle_in(&[0], diags) {
                return None;
            }
            let m0 = get(0)?;
            if m0.ty.is_float() {
                diags.push(diag(
                    "VG-T0104",
                    Some(i),
                    format!("integer op {} applied to floating-point operand", info.name),
                ));
                return None;
            }
            if !m0.ty.is_int() {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("{} requires an integer operand, found {}", info.name, m0.ty),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![make_local_meta(m0.ty, false, vec![false; n_params])])
        }

        "SHL" | "SHR" | "ROTL" | "ROTR" => {
            if reject_eff_handle_in(&[0, 1], diags) {
                return None;
            }
            let (m0, m1) = (get(0)?, get(1)?);
            if m0.ty.is_float() {
                diags.push(diag(
                    "VG-T0104",
                    Some(i),
                    format!("integer op {} applied to floating-point value", info.name),
                ));
                return None;
            }
            if !m0.ty.is_int() {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!(
                        "{} value operand must be an integer, found {}",
                        info.name, m0.ty
                    ),
                ));
                return None;
            }
            if m1.ty != Type::UInt(32) {
                diags.push(diag(
                    "VG-T0112",
                    Some(i),
                    format!(
                        "{} shift/rotate amount must be u32, found {}",
                        info.name, m1.ty
                    ),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![make_local_meta(m0.ty, false, vec![false; n_params])])
        }

        "FADD" | "FSUB" | "FMUL" | "FDIV" | "FMA" | "FNEG" | "FABS" | "FMIN" | "FMAX"
        | "FCLAMP" | "FCOPYSIGN" | "FSQRT" => {
            let indices: Vec<usize> = (0..op.operands.len()).collect();
            if reject_eff_handle_in(&indices, diags) {
                return None;
            }
            let ms: Vec<ValMeta> = indices.iter().map(|&idx| get(idx)).collect::<Option<_>>()?;
            if ms.iter().any(|m| is_f16_or_bf16(&m.ty)) {
                diags.push(diag(
                    "VG-T0105",
                    Some(i),
                    "f16 and bf16 are storage-only in the MVP; CAST to f32 or f64 first",
                ));
                return None;
            }
            if ms.iter().any(|m| m.ty.is_int()) {
                diags.push(diag(
                    "VG-T0104",
                    Some(i),
                    format!("float op {} applied to integer operand", info.name),
                ));
                return None;
            }
            if ms.iter().any(|m| !is_mvp_arith_float(&m.ty)) {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("{} requires f32 or f64 operands", info.name),
                ));
                return None;
            }
            let first = &ms[0].ty;
            if ms[1..].iter().any(|m| &m.ty != first) {
                diags.push(diag(
                    "VG-T0102",
                    Some(i),
                    format!("{} operands must share one float type", info.name),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![make_local_meta(
                first.clone(),
                false,
                vec![false; n_params],
            )])
        }

        "EQ" | "NE" => {
            if reject_eff_handle_in(&[0, 1], diags) {
                return None;
            }
            let (m0, m1) = (get(0)?, get(1)?);
            if is_f16_or_bf16(&m0.ty) || is_f16_or_bf16(&m1.ty) {
                diags.push(diag(
                    "VG-T0105",
                    Some(i),
                    "f16 and bf16 cannot be compared directly; CAST to f32 or f64 first",
                ));
                return None;
            }
            let valid_eq_ty = |t: &Type| *t == Type::Bool || t.is_int() || is_mvp_arith_float(t);
            if !valid_eq_ty(&m0.ty) || !valid_eq_ty(&m1.ty) {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("{} requires integer, f32/f64, or Bool operands", info.name),
                ));
                return None;
            }
            if m0.ty != m1.ty {
                diags.push(diag(
                    "VG-T0102",
                    Some(i),
                    format!(
                        "{} operands have different types: {} vs {}",
                        info.name, m0.ty, m1.ty
                    ),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![make_local_meta(
                Type::Bool,
                false,
                vec![false; n_params],
            )])
        }

        "LT" | "LE" | "GT" | "GE" => {
            if reject_eff_handle_in(&[0, 1], diags) {
                return None;
            }
            let (m0, m1) = (get(0)?, get(1)?);
            if is_f16_or_bf16(&m0.ty) || is_f16_or_bf16(&m1.ty) {
                diags.push(diag(
                    "VG-T0105",
                    Some(i),
                    "f16 and bf16 cannot be compared directly; CAST to f32 or f64 first",
                ));
                return None;
            }
            let valid_ord_ty = |t: &Type| t.is_int() || is_mvp_arith_float(t);
            if !valid_ord_ty(&m0.ty) || !valid_ord_ty(&m1.ty) {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("{} requires integer or f32/f64 operands", info.name),
                ));
                return None;
            }
            if m0.ty != m1.ty {
                diags.push(diag(
                    "VG-T0102",
                    Some(i),
                    format!(
                        "{} operands have different types: {} vs {}",
                        info.name, m0.ty, m1.ty
                    ),
                ));
                return None;
            }
            let mode_ok = match op.mode {
                1 => matches!(m0.ty, Type::SInt(_)),
                2 => matches!(m0.ty, Type::UInt(_)),
                3 => is_mvp_arith_float(&m0.ty),
                _ => false,
            };
            if !mode_ok {
                diags.push(diag(
                    "VG-T0103",
                    Some(i),
                    format!("{} mode does not match operand type {}", info.name, m0.ty),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![make_local_meta(
                Type::Bool,
                false,
                vec![false; n_params],
            )])
        }

        "BAND" | "BOR" => {
            if reject_eff_handle_in(&[0, 1], diags) {
                return None;
            }
            let (m0, m1) = (get(0)?, get(1)?);
            if m0.ty != Type::Bool || m1.ty != Type::Bool {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!(
                        "{} requires Bool operands, found {} and {}",
                        info.name, m0.ty, m1.ty
                    ),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![make_local_meta(
                Type::Bool,
                false,
                vec![false; n_params],
            )])
        }

        "BNOT" => {
            if reject_eff_handle_in(&[0], diags) {
                return None;
            }
            let m0 = get(0)?;
            if m0.ty != Type::Bool {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("BNOT requires a Bool operand, found {}", m0.ty),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![make_local_meta(
                Type::Bool,
                false,
                vec![false; n_params],
            )])
        }

        "INDEX" => {
            if reject_eff_handle_in(&[0, 1], diags) {
                return None;
            }
            let (m0, m1) = (get(0)?, get(1)?);
            let elem = match &m0.ty {
                Type::Array(e, _) | Type::Slice(e) => (**e).clone(),
                t => {
                    diags.push(diag(
                        "VG-T0101",
                        Some(i),
                        format!("INDEX requires Array or Slice, found {t}"),
                    ));
                    return None;
                }
            };
            if m1.ty != Type::UInt(64) {
                diags.push(diag(
                    "VG-T0112",
                    Some(i),
                    format!("INDEX index must be u64, found {}", m1.ty),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![ValMeta {
                ty: elem.clone(),
                origin_local_frame: m0.origin_local_frame && is_fixed_size(&elem),
                has_local_slice: m0.has_local_slice && type_contains_slice(&elem),
                owns_fresh_arena_slice: false,
                from_params: if type_contains_slice(&elem) {
                    m0.from_params
                } else {
                    vec![false; n_params]
                },
                valid_handle: None,
                len_expr: static_element_count(&elem).map(CostExpr::Const),
            }])
        }

        "UPDATE" => {
            if reject_eff_handle_in(&[0, 1, 2], diags) {
                return None;
            }
            let (m0, m1, m2) = (get(0)?, get(1)?, get(2)?);
            let elem = match &m0.ty {
                Type::Array(e, _) | Type::Slice(e) => (**e).clone(),
                t => {
                    diags.push(diag(
                        "VG-T0101",
                        Some(i),
                        format!("UPDATE requires Array or Slice, found {t}"),
                    ));
                    return None;
                }
            };
            if m1.ty != Type::UInt(64) {
                diags.push(diag(
                    "VG-T0112",
                    Some(i),
                    format!("UPDATE index must be u64, found {}", m1.ty),
                ));
                return None;
            }
            if m2.ty != elem {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("UPDATE value has type {}, expected {elem}", m2.ty),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            match &m0.ty {
                Type::Array(..) => {
                    let mut from_params = m0.from_params;
                    or_into(&mut from_params, &m2.from_params);
                    Some(vec![make_local_meta(
                        m0.ty.clone(),
                        m0.has_local_slice || m2.has_local_slice,
                        from_params,
                    )])
                }
                Type::Slice(_) => {
                    let slice_len = m0
                        .len_expr
                        .clone()
                        .unwrap_or_else(|| collection_len_expr(node, op.operands[0]));
                    let in_place = m0.owns_fresh_arena_slice
                        && use_counts.get(&op.operands[0]).copied().unwrap_or(0) == 1;
                    if !in_place {
                        if node.effects.0 & EFF_ARENA == 0 {
                            diags.push(diag(
                                "VG-E0106",
                                Some(i),
                                "copying UPDATE of a slice allocates in the arena, but ARENA is missing",
                            ));
                        }
                        *used_effects |= EFF_ARENA;
                        let elem_sz = abi_size_align(&elem).map(|(s, _)| s).unwrap_or(0);
                        let bytes = CostExpr::align16(CostExpr::mul(
                            slice_len.clone(),
                            CostExpr::Const(elem_sz),
                        ));
                        *total_arena = CostExpr::add(total_arena.clone(), bytes);
                    }
                    let mut from_params = if in_place {
                        m0.from_params
                    } else {
                        vec![false; n_params]
                    };
                    if type_contains_slice(&elem) {
                        or_into(&mut from_params, &m2.from_params);
                    }
                    Some(vec![ValMeta {
                        ty: m0.ty.clone(),
                        origin_local_frame: false,
                        has_local_slice: (in_place && m0.has_local_slice) || m2.has_local_slice,
                        owns_fresh_arena_slice: true,
                        from_params,
                        valid_handle: None,
                        len_expr: Some(slice_len),
                    }])
                }
                _ => unreachable!(),
            }
        }

        "LEN" => {
            if reject_eff_handle_in(&[0], diags) {
                return None;
            }
            let m0 = get(0)?;
            if !matches!(m0.ty, Type::Array(..) | Type::Slice(_)) {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("LEN requires Array or Slice, found {}", m0.ty),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![make_local_meta(
                Type::UInt(64),
                false,
                vec![false; n_params],
            )])
        }

        "SLICE" => {
            if reject_eff_handle_in(&[0, 1, 2], diags) {
                return None;
            }
            let (m0, m1, m2) = (get(0)?, get(1)?, get(2)?);
            let elem = match &m0.ty {
                Type::Array(e, _) | Type::Slice(e) => (**e).clone(),
                t => {
                    diags.push(diag(
                        "VG-T0101",
                        Some(i),
                        format!("SLICE requires Array or Slice, found {t}"),
                    ));
                    return None;
                }
            };
            if m1.ty != Type::UInt(64) || m2.ty != Type::UInt(64) {
                diags.push(diag("VG-T0112", Some(i), "SLICE bounds must be u64"));
                return None;
            }
            let has_local = if matches!(m0.ty, Type::Slice(_)) {
                m0.has_local_slice
            } else {
                m0.origin_local_frame
            };
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![ValMeta {
                ty: Type::Slice(Box::new(elem)),
                origin_local_frame: false,
                has_local_slice: has_local,
                owns_fresh_arena_slice: false,
                from_params: m0.from_params,
                valid_handle: None,
                len_expr: None,
            }])
        }

        "CONCAT" => {
            if reject_eff_handle_in(&[0, 1], diags) {
                return None;
            }
            let (m0, m1) = (get(0)?, get(1)?);
            let e0 = match &m0.ty {
                Type::Array(e, _) | Type::Slice(e) => (**e).clone(),
                t => {
                    diags.push(diag(
                        "VG-T0101",
                        Some(i),
                        format!("CONCAT requires Arrays or Slices, found {t}"),
                    ));
                    return None;
                }
            };
            let e1 = match &m1.ty {
                Type::Array(e, _) | Type::Slice(e) => (**e).clone(),
                t => {
                    diags.push(diag(
                        "VG-T0101",
                        Some(i),
                        format!("CONCAT requires Arrays or Slices, found {t}"),
                    ));
                    return None;
                }
            };
            if e0 != e1 {
                diags.push(diag(
                    "VG-T0102",
                    Some(i),
                    format!("CONCAT element types differ: {e0} vs {e1}"),
                ));
                return None;
            }
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            match (&m0.ty, &m1.ty) {
                (Type::Array(_, n), Type::Array(_, m)) => {
                    let Some(sum) = n.checked_add(*m) else {
                        diags.push(diag(
                            "VG-T0118",
                            Some(i),
                            "CONCAT array length exceeds 2^32 - 1",
                        ));
                        return None;
                    };
                    let res_ty = Type::Array(Box::new(e0), sum);
                    if let Err(d) = validate_type(&res_ty, false, 1, Some(i)) {
                        diags.push(d);
                        return None;
                    }
                    let mut from_params = m0.from_params;
                    or_into(&mut from_params, &m1.from_params);
                    Some(vec![make_local_meta(
                        res_ty,
                        m0.has_local_slice || m1.has_local_slice,
                        from_params,
                    )])
                }
                _ => {
                    if node.effects.0 & EFF_ARENA == 0 {
                        diags.push(diag(
                            "VG-E0106",
                            Some(i),
                            "CONCAT producing a Slice allocates in the arena, but ARENA is missing",
                        ));
                    }
                    *used_effects |= EFF_ARENA;
                    let elem_sz = abi_size_align(&e0).map(|(s, _)| s).unwrap_or(0);
                    let len_a = m0
                        .len_expr
                        .unwrap_or_else(|| collection_len_expr(node, op.operands[0]));
                    let len_b = m1
                        .len_expr
                        .unwrap_or_else(|| collection_len_expr(node, op.operands[1]));
                    let sum_len = CostExpr::add(len_a, len_b);
                    let bytes =
                        CostExpr::align16(CostExpr::mul(sum_len.clone(), CostExpr::Const(elem_sz)));
                    *total_arena = CostExpr::add(total_arena.clone(), bytes);
                    let elem_has_slice = type_contains_slice(&e0);
                    let mut from_params = vec![false; n_params];
                    if elem_has_slice {
                        or_into(&mut from_params, &m0.from_params);
                        or_into(&mut from_params, &m1.from_params);
                    }
                    Some(vec![ValMeta {
                        ty: Type::Slice(Box::new(e0)),
                        origin_local_frame: false,
                        has_local_slice: elem_has_slice
                            && (m0.has_local_slice || m1.has_local_slice),
                        owns_fresh_arena_slice: true,
                        from_params,
                        valid_handle: None,
                        len_expr: Some(sum_len),
                    }])
                }
            }
        }

        "SIMD_MAP" => {
            let kernel = lookup_callee(0)?;
            if !check_kernel_callee(kernel, Some(i), diags) {
                return None;
            }
            let val_indices: Vec<usize> = (1..op.operands.len()).collect();
            if reject_eff_handle_in(&val_indices, diags) {
                return None;
            }
            let m_xs = get(1)?;
            let elem_t = match &m_xs.ty {
                Type::Array(e, _) | Type::Tensor(e, _) | Type::Slice(e) => (**e).clone(),
                t => {
                    diags.push(diag(
                        "VG-T0101",
                        Some(i),
                        format!("SIMD_MAP requires Array, Tensor, or Slice, found {t}"),
                    ));
                    return None;
                }
            };
            let broadcasts: Vec<Type> = (2..op.operands.len())
                .map(|idx| get(idx).map(|m| m.ty))
                .collect::<Option<_>>()?;
            let mut expected_params = Vec::with_capacity(1 + broadcasts.len());
            expected_params.push(elem_t);
            expected_params.extend(broadcasts);
            if kernel.node.params != expected_params {
                diags.push(diag(
                    "VG-T0106",
                    Some(i),
                    "SIMD_MAP kernel signature does not match element and broadcast types",
                ));
                return None;
            }
            let out_elem = kernel.node.results[0].clone();
            let (res_ty, allocates) = match &m_xs.ty {
                Type::Array(_, n) => (Type::Array(Box::new(out_elem.clone()), *n), false),
                Type::Tensor(_, dims) => (
                    Type::Tensor(Box::new(out_elem.clone()), dims.clone()),
                    false,
                ),
                Type::Slice(_) => (Type::Slice(Box::new(out_elem.clone())), true),
                _ => unreachable!(),
            };
            if let Err(d) = validate_type(&res_ty, false, 1, Some(i)) {
                diags.push(d);
                return None;
            }
            let n_expr = m_xs
                .len_expr
                .unwrap_or_else(|| collection_len_expr(node, op.operands[1]));
            let op_cost = CostExpr::add(
                CostExpr::Const(1),
                CostExpr::mul(n_expr.clone(), kernel.ops_expr.clone()),
            );
            *total_ops = CostExpr::add(total_ops.clone(), op_cost);
            if allocates {
                if node.effects.0 & EFF_ARENA == 0 {
                    diags.push(diag(
                        "VG-E0106",
                        Some(i),
                        "SIMD_MAP over a Slice allocates in the arena, but ARENA is missing",
                    ));
                }
                *used_effects |= EFF_ARENA;
                let elem_sz = fixed_byte_size(&out_elem).unwrap_or(0);
                let bytes =
                    CostExpr::align16(CostExpr::mul(n_expr.clone(), CostExpr::Const(elem_sz)));
                *total_arena = CostExpr::add(total_arena.clone(), bytes);
            }
            Some(vec![ValMeta {
                ty: res_ty,
                origin_local_frame: !allocates,
                has_local_slice: false,
                owns_fresh_arena_slice: allocates,
                from_params: vec![false; n_params],
                valid_handle: None,
                len_expr: Some(n_expr),
            }])
        }

        "FOLD_BOUNDED" => {
            let kernel = lookup_callee(0)?;
            if !check_kernel_callee(kernel, Some(i), diags) {
                return None;
            }
            // Operands: [@f, xs, b..., init, invariant]
            let n_ops = op.operands.len();
            let init_idx = n_ops - 2;
            let val_indices: Vec<usize> = (1..n_ops - 1).collect();
            if reject_eff_handle_in(&val_indices, diags) {
                return None;
            }
            let m_xs = get(1)?;
            let elem_t = match &m_xs.ty {
                Type::Array(e, _) | Type::Slice(e) => (**e).clone(),
                t => {
                    diags.push(diag(
                        "VG-T0101",
                        Some(i),
                        format!("FOLD_BOUNDED requires Array or Slice, found {t}"),
                    ));
                    return None;
                }
            };
            let acc_t = get(init_idx)?.ty;
            let broadcasts: Vec<Type> = (2..init_idx)
                .map(|idx| get(idx).map(|m| m.ty))
                .collect::<Option<_>>()?;
            let mut expected_params = Vec::with_capacity(2 + broadcasts.len());
            expected_params.push(acc_t.clone());
            expected_params.push(elem_t);
            expected_params.extend(broadcasts);
            if kernel.node.params != expected_params || kernel.node.results[0] != acc_t {
                diags.push(diag(
                    "VG-T0106",
                    Some(i),
                    "FOLD_BOUNDED kernel signature does not match (acc, elem, b...) -> (acc)",
                ));
                return None;
            }
            let n_expr = m_xs
                .len_expr
                .unwrap_or_else(|| collection_len_expr(node, op.operands[1]));
            let op_cost = CostExpr::add(
                CostExpr::Const(1),
                CostExpr::mul(n_expr, kernel.ops_expr.clone()),
            );
            *total_ops = CostExpr::add(total_ops.clone(), op_cost);
            Some(vec![make_local_meta(acc_t, false, vec![false; n_params])])
        }

        "GENERATE" => {
            let kernel = lookup_callee(0)?;
            if !check_kernel_callee(kernel, Some(i), diags) {
                return None;
            }
            let val_indices: Vec<usize> = (1..op.operands.len()).collect();
            if reject_eff_handle_in(&val_indices, diags) {
                return None;
            }
            let m_n = get(1)?;
            if m_n.ty != Type::UInt(64) {
                diags.push(diag(
                    "VG-T0112",
                    Some(i),
                    format!("GENERATE count must be u64, found {}", m_n.ty),
                ));
                return None;
            }
            let broadcasts: Vec<Type> = (2..op.operands.len())
                .map(|idx| get(idx).map(|m| m.ty))
                .collect::<Option<_>>()?;
            let mut expected_params = Vec::with_capacity(1 + broadcasts.len());
            expected_params.push(Type::UInt(64));
            expected_params.extend(broadcasts);
            if kernel.node.params != expected_params {
                diags.push(diag(
                    "VG-T0106",
                    Some(i),
                    "GENERATE kernel signature does not match (u64, b...) -> (U)",
                ));
                return None;
            }
            let out_elem = kernel.node.results[0].clone();
            let (res_ty, n_expr, allocates) = if let Some(cnt) = const_u64(node, op.operands[1]) {
                let Ok(n32) = u32::try_from(cnt) else {
                    diags.push(diag(
                        "VG-T0118",
                        Some(i),
                        format!("GENERATE static count {cnt} exceeds 2^32 - 1"),
                    ));
                    return None;
                };
                let arr_ty = Type::Array(Box::new(out_elem.clone()), n32);
                if let Err(d) = validate_type(&arr_ty, false, 1, Some(i)) {
                    diags.push(d);
                    return None;
                }
                (arr_ty, CostExpr::Const(cnt), false)
            } else {
                (
                    Type::Slice(Box::new(out_elem.clone())),
                    val_expr(node, op.operands[1]),
                    true,
                )
            };
            let op_cost = CostExpr::add(
                CostExpr::Const(1),
                CostExpr::mul(n_expr.clone(), kernel.ops_expr.clone()),
            );
            *total_ops = CostExpr::add(total_ops.clone(), op_cost);
            if allocates {
                if node.effects.0 & EFF_ARENA == 0 {
                    diags.push(diag(
                        "VG-E0106",
                        Some(i),
                        "dynamic GENERATE allocates in the arena, but ARENA is missing",
                    ));
                }
                *used_effects |= EFF_ARENA;
                let elem_sz = fixed_byte_size(&out_elem).unwrap_or(0);
                let bytes =
                    CostExpr::align16(CostExpr::mul(n_expr.clone(), CostExpr::Const(elem_sz)));
                *total_arena = CostExpr::add(total_arena.clone(), bytes);
            }
            Some(vec![ValMeta {
                ty: res_ty,
                origin_local_frame: !allocates,
                has_local_slice: false,
                owns_fresh_arena_slice: allocates,
                from_params: vec![false; n_params],
                valid_handle: None,
                len_expr: Some(n_expr),
            }])
        }

        "ASSERT_GUARD" => {
            if reject_eff_handle_in(&[0], diags) {
                return None;
            }
            let m_cond = get(0)?;
            if m_cond.ty != Type::Bool {
                diags.push(diag(
                    "VG-T0111",
                    Some(i),
                    format!("ASSERT_GUARD condition must be Bool, found {}", m_cond.ty),
                ));
                return None;
            }
            let else_pos = op.operands.len() - 1;
            let f_then = lookup_callee(1)?;
            let f_else = lookup_callee(else_pos)?;
            if f_then.node.spec || f_else.node.spec {
                diags.push(diag(
                    "VG-S0110",
                    Some(i),
                    "SPEC node cannot be called by ASSERT_GUARD",
                ));
                return None;
            }
            if f_then.node.params != f_else.node.params
                || f_then.node.results != f_else.node.results
            {
                diags.push(diag(
                    "VG-T0110",
                    Some(i),
                    "ASSERT_GUARD branches have different signatures",
                ));
                return None;
            }
            let arg_ops = &op.operands[2..else_pos];
            if !check_call_args_and_effects(
                node,
                i,
                "ASSERT_GUARD",
                &f_then.node,
                arg_ops,
                op_metas,
                get_meta,
                used_effects,
                diags,
            ) {
                return None;
            }
            check_callee_effect_mask(node, i, &f_else.node, used_effects, diags);

            let subst_ops_then = f_then
                .ops_expr
                .substitute_for_call(arg_ops, node, i as u32, false);
            let subst_ops_else = f_else
                .ops_expr
                .substitute_for_call(arg_ops, node, i as u32, false);
            *total_ops = CostExpr::add(
                total_ops.clone(),
                CostExpr::add(
                    CostExpr::Const(1),
                    CostExpr::max(subst_ops_then, subst_ops_else),
                ),
            );
            let arena_then = if f_then.node.effects.0 & EFF_ARENA != 0 {
                f_then
                    .arena_bytes
                    .substitute_for_call(arg_ops, node, i as u32, true)
            } else {
                CostExpr::zero()
            };
            let arena_else = if f_else.node.effects.0 & EFF_ARENA != 0 {
                f_else
                    .arena_bytes
                    .substitute_for_call(arg_ops, node, i as u32, true)
            } else {
                CostExpr::zero()
            };
            *total_arena =
                CostExpr::add(total_arena.clone(), CostExpr::max(arena_then, arena_else));

            Some(build_call_result_metas(
                arg_ops,
                &[f_then, f_else],
                op_metas,
                get_meta,
                n_params,
            ))
        }

        "CALL_HASH" => {
            let f = lookup_callee(0)?;
            if f.node.spec {
                diags.push(diag(
                    "VG-S0110",
                    Some(i),
                    "SPEC node cannot be called from a body via CALL_HASH",
                ));
                return None;
            }
            let arg_ops = &op.operands[1..];
            if !check_call_args_and_effects(
                node,
                i,
                "CALL_HASH",
                &f.node,
                arg_ops,
                op_metas,
                get_meta,
                used_effects,
                diags,
            ) {
                return None;
            }
            let subst_ops = f
                .ops_expr
                .substitute_for_call(arg_ops, node, i as u32, false);
            *total_ops = CostExpr::add(
                total_ops.clone(),
                CostExpr::add(CostExpr::Const(1), subst_ops),
            );
            if f.node.effects.0 & EFF_ARENA != 0 {
                let subst_arena = f
                    .arena_bytes
                    .substitute_for_call(arg_ops, node, i as u32, true);
                *total_arena = CostExpr::add(total_arena.clone(), subst_arena);
            }
            Some(build_call_result_metas(
                arg_ops,
                &[f],
                op_metas,
                get_meta,
                n_params,
            ))
        }

        "FILE_READ" => {
            let (m0, m1, m2) = (get(0)?, get(1)?, get(2)?);
            if m0.ty != Type::Eff {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("FILE_READ operand 0 must be Eff, found {}", m0.ty),
                ));
                return None;
            }
            if m1.ty != Type::Handle(Handle::FileIn) {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("FILE_READ operand 1 must be FileIn, found {}", m1.ty),
                ));
                return None;
            }
            if m1.valid_handle != Some(Handle::FileIn) {
                diags.push(diag(
                    "VG-T0119",
                    Some(i),
                    "FILE_READ handle must originate from an input parameter",
                ));
                return None;
            }
            if m2.ty != Type::UInt(64) {
                diags.push(diag(
                    "VG-T0112",
                    Some(i),
                    format!("FILE_READ max size must be u64, found {}", m2.ty),
                ));
                return None;
            }
            if node.effects.0 & EFF_FILE_READ == 0 {
                diags.push(diag(
                    "VG-E0101",
                    Some(i),
                    "FILE_READ requires FILE_READ in EFFECTS",
                ));
            }
            if node.effects.0 & EFF_ARENA == 0 {
                diags.push(diag(
                    "VG-E0106",
                    Some(i),
                    "FILE_READ allocates in the arena, but ARENA is missing",
                ));
            }
            *used_effects |= EFF_FILE_READ | EFF_ARENA;
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            let bytes = CostExpr::align16(val_expr(node, op.operands[2]));
            *total_arena = CostExpr::add(total_arena.clone(), bytes);

            let res_ty = Type::Result(
                Box::new(Type::Slice(Box::new(Type::UInt(8)))),
                Box::new(Type::UInt(32)),
            );
            Some(vec![
                make_local_meta(Type::Eff, false, vec![false; n_params]),
                ValMeta {
                    ty: res_ty,
                    origin_local_frame: false,
                    has_local_slice: false,
                    owns_fresh_arena_slice: false,
                    from_params: vec![false; n_params],
                    valid_handle: None,
                    len_expr: None,
                },
            ])
        }

        "FILE_WRITE" => {
            let (m0, m1, m2) = (get(0)?, get(1)?, get(2)?);
            if m0.ty != Type::Eff {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("FILE_WRITE operand 0 must be Eff, found {}", m0.ty),
                ));
                return None;
            }
            if m1.ty != Type::Handle(Handle::FileOut) {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("FILE_WRITE operand 1 must be FileOut, found {}", m1.ty),
                ));
                return None;
            }
            if m1.valid_handle != Some(Handle::FileOut) {
                diags.push(diag(
                    "VG-T0119",
                    Some(i),
                    "FILE_WRITE handle must originate from an input parameter",
                ));
                return None;
            }
            if m2.ty != Type::Slice(Box::new(Type::UInt(8))) {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("FILE_WRITE data must be Slice<u8>, found {}", m2.ty),
                ));
                return None;
            }
            if node.effects.0 & EFF_FILE_WRITE == 0 {
                diags.push(diag(
                    "VG-E0101",
                    Some(i),
                    "FILE_WRITE requires FILE_WRITE in EFFECTS",
                ));
            }
            *used_effects |= EFF_FILE_WRITE;
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            let res_ty = Type::Result(Box::new(Type::UInt(64)), Box::new(Type::UInt(32)));
            Some(vec![
                make_local_meta(Type::Eff, false, vec![false; n_params]),
                make_local_meta(res_ty, false, vec![false; n_params]),
            ])
        }

        "CLOCK_READ" => {
            let (m0, m1) = (get(0)?, get(1)?);
            if m0.ty != Type::Eff {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("CLOCK_READ operand 0 must be Eff, found {}", m0.ty),
                ));
                return None;
            }
            if m1.ty != Type::Handle(Handle::Clock) {
                diags.push(diag(
                    "VG-T0101",
                    Some(i),
                    format!("CLOCK_READ operand 1 must be Clock, found {}", m1.ty),
                ));
                return None;
            }
            if m1.valid_handle != Some(Handle::Clock) {
                diags.push(diag(
                    "VG-T0119",
                    Some(i),
                    "CLOCK_READ handle must originate from an input parameter",
                ));
                return None;
            }
            if node.effects.0 & EFF_CLOCK == 0 {
                diags.push(diag(
                    "VG-E0101",
                    Some(i),
                    "CLOCK_READ requires CLOCK in EFFECTS",
                ));
            }
            *used_effects |= EFF_CLOCK;
            *total_ops = CostExpr::add(total_ops.clone(), CostExpr::Const(1));
            Some(vec![
                make_local_meta(Type::Eff, false, vec![false; n_params]),
                make_local_meta(Type::UInt(64), false, vec![false; n_params]),
            ])
        }

        _ => unreachable!("handled by opcode MVP check"),
    }
}

fn check_kernel_callee(
    kernel: &CalleeInfo,
    op_idx: Option<usize>,
    diags: &mut Vec<Diagnostic>,
) -> bool {
    if kernel.node.spec {
        diags.push(diag(
            "VG-S0110",
            op_idx,
            "SPEC node cannot be used as a combinator kernel",
        ));
        return false;
    }
    if kernel.node.effects.0 != 0 {
        diags.push(diag(
            "VG-T0107",
            op_idx,
            "combinator kernel must have empty EFFECTS (even ARENA is forbidden)",
        ));
        return false;
    }
    if kernel.node.results.len() != 1 {
        diags.push(diag(
            "VG-T0107",
            op_idx,
            format!(
                "combinator kernel must have 1 result, found {}",
                kernel.node.results.len()
            ),
        ));
        return false;
    }
    for t in kernel.node.params.iter().chain(&kernel.node.results) {
        if contains_eff_or_handle(t) {
            diags.push(diag(
                "VG-T0119",
                op_idx,
                format!("combinator kernel cannot use Eff or Handle type {t}"),
            ));
            return false;
        }
        if !is_fixed_size(t) {
            diags.push(diag(
                "VG-T0107",
                op_idx,
                format!("combinator kernel type {t} is not fixed-size"),
            ));
            return false;
        }
    }
    true
}

fn check_callee_effect_mask(
    caller: &Node,
    op_idx: usize,
    callee: &Node,
    used_effects: &mut u32,
    diags: &mut Vec<Diagnostic>,
) {
    let missing = callee.effects.0 & !caller.effects.0;
    if missing != 0 {
        let names = EffectMask(missing).names().join(", ");
        diags.push(diag(
            "VG-E0101",
            Some(op_idx),
            format!("callee requires effects [{names}] missing from EFFECTS"),
        ));
    }
    *used_effects |= callee.effects.0;
}

#[allow(clippy::too_many_arguments)]
fn check_call_args_and_effects(
    caller: &Node,
    op_idx: usize,
    op_name: &str,
    callee: &Node,
    arg_ops: &[Operand],
    op_metas: &[Vec<ValMeta>],
    get_meta: MetaLookup<'_>,
    used_effects: &mut u32,
    diags: &mut Vec<Diagnostic>,
) -> bool {
    if arg_ops.len() != callee.params.len() {
        diags.push(diag(
            "VG-T0109",
            Some(op_idx),
            format!(
                "{op_name} takes {} arguments, found {}",
                callee.params.len(),
                arg_ops.len()
            ),
        ));
        return false;
    }
    for (k, (&arg, param_ty)) in arg_ops.iter().zip(&callee.params).enumerate() {
        let Some(m) = get_meta(arg, op_metas) else {
            return false;
        };
        if m.ty != *param_ty {
            diags.push(diag(
                "VG-T0109",
                Some(op_idx),
                format!(
                    "{op_name} argument {k} has type {}, expected {param_ty}",
                    m.ty
                ),
            ));
            return false;
        }
        if let Type::Handle(hk) = param_ty
            && m.valid_handle != Some(*hk)
        {
            diags.push(diag(
                "VG-T0119",
                Some(op_idx),
                format!(
                    "{op_name} handle argument {k} ({}) does not originate from an input handle",
                    hk.name()
                ),
            ));
            return false;
        }
    }
    check_callee_effect_mask(caller, op_idx, callee, used_effects, diags);
    true
}

fn build_call_result_metas(
    arg_ops: &[Operand],
    callees: &[&CalleeInfo],
    op_metas: &[Vec<ValMeta>],
    get_meta: MetaLookup<'_>,
    n_params: usize,
) -> Vec<ValMeta> {
    let arg_metas: Vec<ValMeta> = arg_ops
        .iter()
        .map(|&a| get_meta(a, op_metas).expect("validated"))
        .collect();
    let results = &callees[0].node.results;
    let mut out = Vec::with_capacity(results.len());
    for (slot, res_ty) in results.iter().enumerate() {
        let mut has_local = false;
        let mut from_params = vec![false; n_params];
        let mut valid_handle = None;

        for c in callees {
            if let Some(hk) = c.result_handle_valid.get(slot).copied().flatten() {
                // Only valid if caller passed a valid matching handle in args.
                if arg_metas.iter().any(|am| am.valid_handle == Some(hk)) {
                    valid_handle = Some(hk);
                }
            }
            if type_contains_slice(res_ty) {
                for (p, am) in arg_metas.iter().enumerate() {
                    let views_p = c
                        .result_views_param
                        .get(slot)
                        .and_then(|v| v.get(p))
                        .copied()
                        .unwrap_or(false);
                    if views_p {
                        if am.has_local_slice || am.origin_local_frame {
                            has_local = true;
                        }
                        or_into(&mut from_params, &am.from_params);
                    }
                }
            }
        }
        out.push(ValMeta {
            ty: res_ty.clone(),
            origin_local_frame: is_fixed_size(res_ty),
            has_local_slice: has_local,
            owns_fresh_arena_slice: false,
            from_params,
            valid_handle,
            len_expr: static_element_count(res_ty).map(CostExpr::Const),
        });
    }
    out
}

fn or_into(dst: &mut [bool], src: &[bool]) {
    for (d, &s) in dst.iter_mut().zip(src) {
        *d |= s;
    }
}
