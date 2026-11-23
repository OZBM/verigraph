//! Target metadata, C ABI type layouts, deterministic call-graph collection, and symbol prefixes.

use std::collections::{BTreeMap, BTreeSet};

use vg_core::canon::hashes;
use vg_core::diag::{Diagnostic, Span};
use vg_core::ir::{Hash, Node, Term, hex};
use vg_core::opcode;
use vg_core::types::Type;

use crate::Target;

impl Target {
    pub fn triple(self) -> &'static str {
        match self {
            Target::Aarch64Darwin => "aarch64-apple-darwin",
            Target::X86_64Linux => "x86_64-unknown-linux-gnu",
            Target::Wasm32Wasi => "wasm32-unknown-wasip1",
        }
    }

    pub fn clang_target(self) -> &'static str {
        match self {
            Target::Aarch64Darwin => "aarch64-apple-darwin",
            Target::X86_64Linux => "x86_64-unknown-linux-gnu",
            Target::Wasm32Wasi => "wasm32-wasip1",
        }
    }

    pub fn datalayout(self) -> &'static str {
        match self {
            Target::Aarch64Darwin => {
                "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
            }
            Target::X86_64Linux => {
                "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128"
            }
            Target::Wasm32Wasi => {
                "e-m:e-p:32:32-p10:8:8-p20:8:8-i64:64-i128:128-n32:64-S128-ni:1:10:20"
            }
        }
    }

    pub fn clang_cpu_flags(self) -> &'static [&'static str] {
        match self {
            Target::Aarch64Darwin => &["-mcpu=apple-m1"],
            Target::X86_64Linux => &["-march=x86-64-v3"],
            Target::Wasm32Wasi => &["-msimd128", "-mno-relaxed-simd"],
        }
    }

    pub fn ptr_bytes(self) -> u64 {
        match self {
            Target::Aarch64Darwin | Target::X86_64Linux => 8,
            Target::Wasm32Wasi => 4,
        }
    }

    pub fn ptr_int_ty(self) -> &'static str {
        match self {
            Target::Aarch64Darwin | Target::X86_64Linux => "i64",
            Target::Wasm32Wasi => "i32",
        }
    }
}

pub fn align_up(v: u64, align: u64) -> u64 {
    if align <= 1 {
        return v;
    }
    (v + align - 1) & !(align - 1)
}

/// True when a VG type is passed by value as a scalar in the C ABI (`docs/06`).
pub fn is_scalar_abi(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Bool
            | Type::UInt(_)
            | Type::SInt(_)
            | Type::Float(_)
            | Type::BFloat16
            | Type::Handle(_)
    )
}

/// True when a VG type is omitted from LLVM parameter/result lists (`Eff`).
pub fn is_erased(ty: &Type) -> bool {
    matches!(ty, Type::Eff)
}

pub fn tensor_elem_count(dims: &[u32]) -> Result<u64, Diagnostic> {
    let mut prod = 1u64;
    for &d in dims {
        prod = prod.checked_mul(u64::from(d)).ok_or_else(|| {
            Diagnostic::error(
                "VG-T0118",
                Span::default(),
                "tensor element count overflows u64",
            )
        })?;
    }
    Ok(prod)
}

pub fn align_of(ty: &Type, target: Target) -> Result<u64, Diagnostic> {
    let _ = target;
    Ok(match ty {
        Type::Bool | Type::UInt(8) | Type::SInt(8) => 1,
        Type::UInt(16) | Type::SInt(16) | Type::Float(16) | Type::BFloat16 => 2,
        Type::UInt(32) | Type::SInt(32) | Type::Float(32) | Type::Handle(_) => 4,
        Type::UInt(64) | Type::SInt(64) | Type::Float(64) => 8,
        Type::Array(elem, _) | Type::Tensor(elem, _) => align_of(elem, target)?,
        Type::Slice(_) => 8,
        Type::Tuple(ms) => {
            let mut m = 1u64;
            for item in ms {
                m = m.max(align_of(item, target)?);
            }
            m
        }
        Type::Result(ok, err) => align_of(ok, target)?.max(align_of(err, target)?).max(1),
        Type::Eff | Type::Int | Type::Arena(_) => {
            return Err(Diagnostic::error(
                "VG-T0119",
                Span::default(),
                format!("type {ty} has no C ABI layout"),
            ));
        }
        other => {
            return Err(Diagnostic::error(
                "VG-T0101",
                Span::default(),
                format!("unsupported type in layout: {other}"),
            ));
        }
    })
}

pub fn size_of(ty: &Type, target: Target) -> Result<u64, Diagnostic> {
    Ok(match ty {
        Type::Bool | Type::UInt(8) | Type::SInt(8) => 1,
        Type::UInt(16) | Type::SInt(16) | Type::Float(16) | Type::BFloat16 => 2,
        Type::UInt(32) | Type::SInt(32) | Type::Float(32) | Type::Handle(_) => 4,
        Type::UInt(64) | Type::SInt(64) | Type::Float(64) => 8,
        Type::Array(elem, n) => size_of(elem, target)?
            .checked_mul(u64::from(*n))
            .ok_or_else(|| {
                Diagnostic::error("VG-T0118", Span::default(), "array byte size overflows u64")
            })?,
        Type::Tensor(elem, dims) => size_of(elem, target)?
            .checked_mul(tensor_elem_count(dims)?)
            .ok_or_else(|| {
                Diagnostic::error(
                    "VG-T0118",
                    Span::default(),
                    "tensor byte size overflows u64",
                )
            })?,
        Type::Slice(_) => align_up(target.ptr_bytes(), 8) + 8,
        Type::Tuple(ms) => {
            let mut off = 0u64;
            let mut max_a = 1u64;
            for m in ms {
                let a = align_of(m, target)?;
                let s = size_of(m, target)?;
                max_a = max_a.max(a);
                off = align_up(off, a).checked_add(s).ok_or_else(|| {
                    Diagnostic::error("VG-T0118", Span::default(), "tuple size overflows u64")
                })?;
            }
            align_up(off, max_a)
        }
        Type::Result(ok, err) => {
            let (u_align, u_count, _) = result_union_layout(ok, err, target)?;
            u_align + u_count * u_align
        }
        Type::Eff | Type::Int | Type::Arena(_) => {
            return Err(Diagnostic::error(
                "VG-T0119",
                Span::default(),
                format!("type {ty} has no C ABI layout"),
            ));
        }
        other => {
            return Err(Diagnostic::error(
                "VG-T0101",
                Span::default(),
                format!("unsupported type in layout: {other}"),
            ));
        }
    })
}

/// Returns `(union_align, union_unit_count, union_offset_in_struct)` for `Result<ok, err>`.
/// Struct layout: `{ i8, [union_unit_count x i<8 * union_align>] }`.
pub fn result_union_layout(
    ok: &Type,
    err: &Type,
    target: Target,
) -> Result<(u64, u64, u64), Diagnostic> {
    let u_align = align_of(ok, target)?
        .max(align_of(err, target)?)
        .clamp(1, 8);
    let max_size = size_of(ok, target)?.max(size_of(err, target)?);
    let u_size = align_up(max_size, u_align);
    let count = u_size / u_align;
    Ok((u_align, count, u_align))
}

/// Byte offsets of each member in `Tuple<...>`.
pub fn tuple_member_offsets(ms: &[Type], target: Target) -> Result<Vec<u64>, Diagnostic> {
    let mut off = 0u64;
    let mut offsets = Vec::with_capacity(ms.len());
    for m in ms {
        let a = align_of(m, target)?;
        let s = size_of(m, target)?;
        off = align_up(off, a);
        offsets.push(off);
        off = off.checked_add(s).ok_or_else(|| {
            Diagnostic::error("VG-T0118", Span::default(), "tuple size overflows u64")
        })?;
    }
    Ok(offsets)
}

/// Exact packed byte size for `docs/13` binary stdin/stdout and `AS_BYTES` on scalars/collections.
pub fn docs13_fixed_byte_size(ty: &Type) -> Result<Option<u64>, Diagnostic> {
    Ok(match ty {
        Type::Bool | Type::UInt(8) | Type::SInt(8) => Some(1),
        Type::UInt(16) | Type::SInt(16) | Type::Float(16) | Type::BFloat16 => Some(2),
        Type::UInt(32) | Type::SInt(32) | Type::Float(32) => Some(4),
        Type::UInt(64) | Type::SInt(64) | Type::Float(64) => Some(8),
        Type::Array(elem, n) => {
            docs13_fixed_byte_size(elem)?.and_then(|s| s.checked_mul(u64::from(*n)))
        }
        Type::Tensor(elem, dims) => {
            let count = tensor_elem_count(dims)?;
            docs13_fixed_byte_size(elem)?.and_then(|s| s.checked_mul(count))
        }
        Type::Slice(_) | Type::Tuple(_) | Type::Result(..) => None,
        Type::Eff | Type::Handle(_) | Type::Int | Type::Arena(_) => None,
        _ => None,
    })
}

pub fn llvm_type(ty: &Type, target: Target) -> Result<String, Diagnostic> {
    Ok(match ty {
        Type::Bool => "i8".to_string(),
        Type::UInt(w) | Type::SInt(w) => format!("i{w}"),
        Type::Float(16) | Type::BFloat16 => "i16".to_string(),
        Type::Float(32) => "float".to_string(),
        Type::Float(64) => "double".to_string(),
        Type::Handle(_) => "i32".to_string(),
        Type::Array(elem, n) => format!("[{n} x {}]", llvm_type(elem, target)?),
        Type::Tensor(elem, dims) => {
            let count = tensor_elem_count(dims)?;
            format!("[{count} x {}]", llvm_type(elem, target)?)
        }
        Type::Slice(_) => "{ ptr, i64 }".to_string(),
        Type::Tuple(ms) => {
            let parts: Vec<String> = ms
                .iter()
                .map(|m| llvm_type(m, target))
                .collect::<Result<_, _>>()?;
            format!("{{ {} }}", parts.join(", "))
        }
        Type::Result(ok, err) => {
            let (u_align, count, _) = result_union_layout(ok, err, target)?;
            format!("{{ i8, [{count} x i{}] }}", u_align * 8)
        }
        Type::Eff | Type::Int | Type::Arena(_) => {
            return Err(Diagnostic::error(
                "VG-T0119",
                Span::default(),
                format!("type {ty} has no LLVM value type"),
            ));
        }
        other => {
            return Err(Diagnostic::error(
                "VG-T0101",
                Span::default(),
                format!("unsupported type for LLVM emission: {other}"),
            ));
        }
    })
}

/// Deterministic symbol prefixes for a set of hashes: 12 hex digits, growing to 16, 24, or 64 on
/// collisions (`docs/06`).
pub fn assign_prefixes<'a>(
    hashes_iter: impl IntoIterator<Item = &'a Hash>,
) -> BTreeMap<Hash, String> {
    let all: BTreeSet<Hash> = hashes_iter.into_iter().copied().collect();
    let hexes: BTreeMap<Hash, String> = all.iter().map(|h| (*h, hex(h))).collect();
    let mut lens: BTreeMap<Hash, usize> = all.iter().map(|h| (*h, 12usize)).collect();

    for next_len in [16usize, 24, 64] {
        let mut groups: BTreeMap<String, Vec<Hash>> = BTreeMap::new();
        for &h in &all {
            let l = lens[&h];
            groups
                .entry(hexes[&h][..l].to_string())
                .or_default()
                .push(h);
        }
        let mut any_clash = false;
        for group in groups.values() {
            if group.len() > 1 {
                any_clash = true;
                for h in group {
                    lens.insert(*h, next_len);
                }
            }
        }
        if !any_clash {
            break;
        }
    }

    all.into_iter()
        .map(|h| {
            let l = lens[&h];
            let p = hexes[&h][..l].to_string();
            (h, p)
        })
        .collect()
}

/// Collected call graph in deterministic topological order (callees before callers).
pub struct CallGraph {
    pub root_hash: Hash,
    /// `(hash, node)` in topological post-order, finishing with `(root_hash, root)`.
    pub order: Vec<(Hash, Node)>,
    pub by_hash: BTreeMap<Hash, Node>,
    pub prefixes: BTreeMap<Hash, String>,
}

fn collect_term_callees(t: &Term, calls: &[Hash], out: &mut BTreeSet<Hash>) {
    match t {
        Term::Spec(c, args) | Term::Fold(c, args) | Term::Apply(c, args) => {
            if let Some(h) = calls.get(*c as usize) {
                out.insert(*h);
            }
            for a in args {
                collect_term_callees(a, calls, out);
            }
        }
        Term::Not(a) | Term::Neg(a) | Term::Proj(a, _) => collect_term_callees(a, calls, out),
        Term::Bin(_, a, b) => {
            collect_term_callees(a, calls, out);
            collect_term_callees(b, calls, out);
        }
        Term::Index(a, idxs) => {
            collect_term_callees(a, calls, out);
            for i in idxs {
                collect_term_callees(i, calls, out);
            }
        }
        Term::Forall(rs, b) => {
            for (lo, hi) in rs {
                collect_term_callees(lo, calls, out);
                collect_term_callees(hi, calls, out);
            }
            collect_term_callees(b, calls, out);
        }
        Term::Builtin(_, args) => {
            for a in args {
                collect_term_callees(a, calls, out);
            }
        }
        Term::Input(_)
        | Term::Result(_)
        | Term::Port(..)
        | Term::Bound(_)
        | Term::Const(_)
        | Term::Resource(_) => {}
    }
}

fn node_runtime_callees(node: &Node, include_pre_specs: bool) -> BTreeSet<Hash> {
    let mut deps = BTreeSet::new();
    for op in &node.ops {
        if matches!(
            op.opcode,
            0x0300 | 0x0100 | opcode::FOLD_BOUNDED | opcode::GENERATE | 0x0105
        ) {
            for operand in &op.operands {
                if let vg_core::ir::Operand::Callee(c) = operand
                    && let Some(h) = node.calls.get(*c as usize)
                {
                    deps.insert(*h);
                }
            }
        }
    }
    if include_pre_specs {
        for t in &node.contract.pre {
            collect_term_callees(t, &node.calls, &mut deps);
        }
    }
    deps
}

/// Discovers all nodes reachable at runtime (or from `root.contract.pre` for checked entry).
/// Does NOT require resolving hashes referenced only in erased clauses (`POST`, `COST`, `INVARIANT`,
/// `LEARNABLE.loss`).
pub fn collect_graph(
    root: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Result<CallGraph, Diagnostic> {
    struct Dfs<'a> {
        callee: &'a dyn Fn(&Hash) -> Option<Node>,
        by_hash: BTreeMap<Hash, Node>,
        visiting: BTreeSet<Hash>,
        visited: BTreeSet<Hash>,
        order: Vec<(Hash, Node)>,
    }

    fn dfs(
        h: Hash,
        node: Node,
        is_root_pre_needed: bool,
        st: &mut Dfs<'_>,
    ) -> Result<(), Diagnostic> {
        if st.visited.contains(&h) {
            return Ok(());
        }
        if !st.visiting.insert(h) {
            return Err(Diagnostic::error(
                "VG-S0007",
                Span::default(),
                format!("call cycle reaching {}", hex(&h)),
            ));
        }
        let deps = node_runtime_callees(&node, is_root_pre_needed);
        for dep in deps {
            if st.visited.contains(&dep) {
                continue;
            }
            let dep_node = (st.callee)(&dep).ok_or_else(|| {
                Diagnostic::error(
                    "VG-S0009",
                    Span::default(),
                    format!("unresolved callee hash {}", hex(&dep)),
                )
            })?;
            let dep_pre_needed = dep_node.spec;
            dfs(dep, dep_node, dep_pre_needed, st)?;
        }
        st.visiting.remove(&h);
        st.visited.insert(h);
        st.by_hash.insert(h, node.clone());
        st.order.push((h, node));
        Ok(())
    }

    let root_hash = hashes(root)?.node;
    let mut st = Dfs {
        callee,
        by_hash: BTreeMap::new(),
        visiting: BTreeSet::new(),
        visited: BTreeSet::new(),
        order: Vec::new(),
    };
    dfs(root_hash, root.clone(), true, &mut st)?;

    let prefixes = assign_prefixes(st.by_hash.keys());
    Ok(CallGraph {
        root_hash,
        order: st.order,
        by_hash: st.by_hash,
        prefixes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_and_slice_layouts_match_c_abi() {
        for tgt in [
            Target::Aarch64Darwin,
            Target::X86_64Linux,
            Target::Wasm32Wasi,
        ] {
            assert_eq!(
                size_of(&Type::Slice(Box::new(Type::UInt(8))), tgt).unwrap(),
                16
            );
            assert_eq!(
                align_of(&Type::Slice(Box::new(Type::UInt(8))), tgt).unwrap(),
                8
            );

            // Result<Slice<u8>, u32>: is_ok (1) + 7 pad + 16 union = 24
            let res_slice = Type::Result(
                Box::new(Type::Slice(Box::new(Type::UInt(8)))),
                Box::new(Type::UInt(32)),
            );
            assert_eq!(size_of(&res_slice, tgt).unwrap(), 24);
            assert_eq!(align_of(&res_slice, tgt).unwrap(), 8);

            // Result<u64, u32>: is_ok (1) + 7 pad + 8 union = 16
            let res_u64 = Type::Result(Box::new(Type::UInt(64)), Box::new(Type::UInt(32)));
            assert_eq!(size_of(&res_u64, tgt).unwrap(), 16);

            // Result<u32, u8>: is_ok (1) + 3 pad + 4 union = 8
            let res_u32 = Type::Result(Box::new(Type::UInt(32)), Box::new(Type::UInt(8)));
            assert_eq!(size_of(&res_u32, tgt).unwrap(), 8);

            // Result<u8, u8>: is_ok (1) + 1 union = 2
            let res_u8 = Type::Result(Box::new(Type::UInt(8)), Box::new(Type::UInt(8)));
            assert_eq!(size_of(&res_u8, tgt).unwrap(), 2);
        }
    }

    #[test]
    fn prefix_lengthening_resolves_collisions_deterministically() {
        let mut h1 = [0u8; 32];
        let mut h2 = [0u8; 32];
        let mut h3 = [0u8; 32];
        h1[0..6].copy_from_slice(&[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
        h2[0..6].copy_from_slice(&[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
        h2[7] = 1; // differs at hex digit 14..16
        h3[0] = 0xFF; // distinct at digit 0

        let p = assign_prefixes([&h1, &h2, &h3]);
        assert_eq!(p[&h1].len(), 16);
        assert_eq!(p[&h2].len(), 16);
        assert_eq!(p[&h3].len(), 12);
        assert_ne!(p[&h1], p[&h2]);

        // Also test 16-char collision expanding to 24
        let mut h4 = h1;
        h4[8] = 0xAA; // differs at byte 8 (hex index 16..18)
        let p2 = assign_prefixes([&h1, &h4]);
        assert_eq!(p2[&h1].len(), 24);
        assert_eq!(p2[&h4].len(), 24);
        assert_ne!(p2[&h1], p2[&h4]);
    }
}
