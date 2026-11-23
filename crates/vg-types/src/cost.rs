//! Static resource accounting (P2.3, docs/05, docs/14) and ABI/static type sizing (docs/02, docs/06).

use vg_core::ir::{Node, Operand};
use vg_core::types::Type;

/// Maximum static byte size of any fixed-size type (16 MiB, docs/02).
pub const MAX_STATIC_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum static element count of any fixed-size Array or Tensor (2^32 - 1, docs/02).
pub const MAX_STATIC_ELEMS: u64 = u32::MAX as u64;
/// Maximum type nesting depth (ADR 0001).
pub const MAX_TYPE_DEPTH: usize = 64;

/// A symbolic static cost expression over node inputs and intermediate slice lengths (docs/05, docs/14).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CostExpr {
    Const(u64),
    /// Runtime length `len(xs)` of a slice operand.
    Len(Operand),
    /// Runtime `u64` value of an operand (e.g. `GENERATE` count or `FILE_READ` max).
    Val(Operand),
    /// Abstract op cost from a callee whose internal cost depends on non-parameter values.
    CalleeOps(u32),
    /// Abstract arena byte cost from a callee whose internal allocation depends on non-parameter values.
    CalleeArenaBytes(u32),
    Add(Box<CostExpr>, Box<CostExpr>),
    Mul(Box<CostExpr>, Box<CostExpr>),
    Max(Box<CostExpr>, Box<CostExpr>),
    /// Round up to the next multiple of 16 bytes (0 stays 0).
    Align16(Box<CostExpr>),
}

impl CostExpr {
    pub fn zero() -> Self {
        Self::Const(0)
    }

    #[allow(clippy::should_implement_trait)]
    pub fn add(a: Self, b: Self) -> Self {
        match (a, b) {
            (Self::Const(0), x) | (x, Self::Const(0)) => x,
            (Self::Const(x), Self::Const(y)) => Self::Const(x.saturating_add(y)),
            (x, y) => Self::Add(Box::new(x), Box::new(y)),
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn mul(a: Self, b: Self) -> Self {
        match (a, b) {
            (Self::Const(0), _) | (_, Self::Const(0)) => Self::Const(0),
            (Self::Const(1), x) | (x, Self::Const(1)) => x,
            (Self::Const(x), Self::Const(y)) => Self::Const(x.saturating_mul(y)),
            (x, y) => Self::Mul(Box::new(x), Box::new(y)),
        }
    }

    pub fn max(a: Self, b: Self) -> Self {
        match (a, b) {
            (Self::Const(0), x) | (x, Self::Const(0)) => x,
            (Self::Const(x), Self::Const(y)) => Self::Const(x.max(y)),
            (x, y) if x == y => x,
            (x, y) => Self::Max(Box::new(x), Box::new(y)),
        }
    }

    pub fn align16(a: Self) -> Self {
        match a {
            Self::Const(0) => Self::Const(0),
            Self::Const(c) => Self::Const(align_up_saturating(c, 16)),
            Self::Align16(inner) => Self::Align16(inner),
            other => Self::Align16(Box::new(other)),
        }
    }

    pub fn as_const(&self) -> Option<u64> {
        match self {
            Self::Const(c) => Some(*c),
            _ => None,
        }
    }

    /// Sound static lower bound obtained by evaluating symbolic runtime terms at 0.
    pub fn lower_bound(&self) -> u64 {
        match self {
            Self::Const(c) => *c,
            Self::Len(_) | Self::Val(_) | Self::CalleeOps(_) | Self::CalleeArenaBytes(_) => 0,
            Self::Add(a, b) => a.lower_bound().saturating_add(b.lower_bound()),
            Self::Mul(a, b) => a.lower_bound().saturating_mul(b.lower_bound()),
            Self::Max(a, b) => a.lower_bound().max(b.lower_bound()),
            Self::Align16(a) => align_up_saturating(a.lower_bound(), 16),
        }
    }

    /// Instantiate a callee's cost expression at a call site with caller `args`.
    pub fn substitute_for_call(
        &self,
        args: &[Operand],
        caller: &Node,
        call_idx: u32,
        is_arena: bool,
    ) -> CostExpr {
        let placeholder = || {
            if is_arena {
                CostExpr::CalleeArenaBytes(call_idx)
            } else {
                CostExpr::CalleeOps(call_idx)
            }
        };
        match self {
            Self::Const(c) => Self::Const(*c),
            Self::Len(op) => match *op {
                Operand::Input(p) => match args.get(p as usize).copied() {
                    Some(arg) => {
                        if let Some(cnt) = caller.operand_type(arg).and_then(static_element_count) {
                            Self::Const(cnt)
                        } else {
                            Self::Len(arg)
                        }
                    }
                    None => placeholder(),
                },
                _ => placeholder(),
            },
            Self::Val(op) => match *op {
                Operand::Input(p) => match args.get(p as usize).copied() {
                    Some(arg) => {
                        if let Some(v) = const_u64(caller, arg) {
                            Self::Const(v)
                        } else {
                            Self::Val(arg)
                        }
                    }
                    None => placeholder(),
                },
                _ => placeholder(),
            },
            Self::CalleeOps(_) | Self::CalleeArenaBytes(_) => placeholder(),
            Self::Add(a, b) => Self::add(
                a.substitute_for_call(args, caller, call_idx, is_arena),
                b.substitute_for_call(args, caller, call_idx, is_arena),
            ),
            Self::Mul(a, b) => Self::mul(
                a.substitute_for_call(args, caller, call_idx, is_arena),
                b.substitute_for_call(args, caller, call_idx, is_arena),
            ),
            Self::Max(a, b) => Self::max(
                a.substitute_for_call(args, caller, call_idx, is_arena),
                b.substitute_for_call(args, caller, call_idx, is_arena),
            ),
            Self::Align16(a) => {
                Self::align16(a.substitute_for_call(args, caller, call_idx, is_arena))
            }
        }
    }
}

fn align_up_checked(x: u64, align: u64) -> Option<u64> {
    if align <= 1 {
        return Some(x);
    }
    let rem = x % align;
    if rem == 0 {
        Some(x)
    } else {
        x.checked_add(align - rem)
    }
}

fn align_up_saturating(x: u64, align: u64) -> u64 {
    if align <= 1 {
        return x;
    }
    let rem = x % align;
    if rem == 0 {
        x
    } else {
        x.saturating_add(align - rem)
    }
}

/// Extract a constant `u64` value if `op` is `Operand::Const` of type `u64`.
pub fn const_u64(node: &Node, op: Operand) -> Option<u64> {
    let Operand::Const(ci) = op else {
        return None;
    };
    let c = node.consts.get(ci as usize)?;
    if c.ty == Type::UInt(64) && c.bytes.len() == 8 {
        Some(u64::from_le_bytes(c.bytes[..8].try_into().ok()?))
    } else {
        None
    }
}

/// Static element count for `Array<T, N>` and `Tensor<T, [d...]>`.
pub fn static_element_count(ty: &Type) -> Option<u64> {
    match ty {
        Type::Array(_, n) => Some(u64::from(*n)),
        Type::Tensor(_, dims) if !dims.is_empty() => dims
            .iter()
            .try_fold(1u64, |acc, &d| acc.checked_mul(u64::from(d))),
        _ => None,
    }
}

/// Returns `true` for fixed-size value types (docs/13): Bool, scalars, and Array/Tensor/Tuple/Result
/// built only from fixed-size types.
pub fn is_fixed_size(ty: &Type) -> bool {
    match ty {
        Type::Bool | Type::UInt(_) | Type::SInt(_) | Type::Float(_) | Type::BFloat16 => true,
        Type::Array(e, _) | Type::Tensor(e, _) => is_fixed_size(e),
        Type::Tuple(ms) => !ms.is_empty() && ms.iter().all(is_fixed_size),
        Type::Result(a, b) => is_fixed_size(a) && is_fixed_size(b),
        Type::Slice(_) | Type::Eff | Type::Handle(_) | Type::Arena(_) | Type::Int => false,
    }
}

/// C ABI `(byte_size, alignment)` per docs/06 for value types representable in memory/structs.
pub fn abi_size_align(ty: &Type) -> Option<(u64, u64)> {
    match ty {
        Type::Bool => Some((1, 1)),
        Type::UInt(b) | Type::SInt(b) if matches!(b, 8 | 16 | 32 | 64) => {
            let bytes = u64::from(*b) / 8;
            Some((bytes, bytes))
        }
        Type::Float(b) if matches!(b, 16 | 32 | 64) => {
            let bytes = u64::from(*b) / 8;
            Some((bytes, bytes))
        }
        Type::BFloat16 => Some((2, 2)),
        Type::Slice(_) => Some((16, 8)),
        Type::Array(e, n) => {
            let (es, ea) = abi_size_align(e)?;
            Some((es.checked_mul(u64::from(*n))?, ea))
        }
        Type::Tensor(e, _) => {
            let (es, ea) = abi_size_align(e)?;
            let count = static_element_count(ty)?;
            Some((es.checked_mul(count)?, ea))
        }
        Type::Tuple(ms) => {
            if ms.is_empty() {
                return None;
            }
            let mut off = 0u64;
            let mut max_a = 1u64;
            for m in ms {
                let (msz, ma) = abi_size_align(m)?;
                off = align_up_checked(off, ma)?;
                off = off.checked_add(msz)?;
                max_a = max_a.max(ma);
            }
            Some((align_up_checked(off, max_a)?, max_a))
        }
        Type::Result(ok, err) => {
            let (os, oa) = abi_size_align(ok)?;
            let (es, ea) = abi_size_align(err)?;
            let union_size = os.max(es);
            let union_align = oa.max(ea);
            let struct_align = 1u64.max(union_align);
            let union_off = align_up_checked(1, union_align)?;
            let total = align_up_checked(union_off.checked_add(union_size)?, struct_align)?;
            Some((total, struct_align))
        }
        Type::UInt(_)
        | Type::SInt(_)
        | Type::Float(_)
        | Type::Eff
        | Type::Handle(_)
        | Type::Arena(_)
        | Type::Int => None,
    }
}

/// Exact static byte size for fixed-size types.
pub fn fixed_byte_size(ty: &Type) -> Option<u64> {
    if is_fixed_size(ty) {
        abi_size_align(ty).map(|(sz, _)| sz)
    } else {
        None
    }
}

/// Symbolic element count of an Array, Tensor, or Slice operand.
pub fn collection_len_expr(node: &Node, op: Operand) -> CostExpr {
    if let Some(cnt) = node.operand_type(op).and_then(static_element_count) {
        CostExpr::Const(cnt)
    } else {
        CostExpr::Len(op)
    }
}

/// Symbolic `u64` value expression of an operand.
pub fn val_expr(node: &Node, op: Operand) -> CostExpr {
    if let Some(v) = const_u64(node, op) {
        CostExpr::Const(v)
    } else {
        CostExpr::Val(op)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vg_core::ir::{Const, Contract};
    use vg_core::types::EffectMask;

    #[test]
    fn abi_size_and_fixed_size_match_docs_06() {
        assert_eq!(abi_size_align(&Type::Bool), Some((1, 1)));
        assert_eq!(abi_size_align(&Type::UInt(32)), Some((4, 4)));
        assert_eq!(abi_size_align(&Type::Float(64)), Some((8, 8)));
        assert_eq!(
            abi_size_align(&Type::Tuple(vec![Type::Bool, Type::UInt(32)])),
            Some((8, 4))
        );
        assert_eq!(
            abi_size_align(&Type::Result(
                Box::new(Type::Slice(Box::new(Type::UInt(8)))),
                Box::new(Type::UInt(32))
            )),
            Some((24, 8))
        );
        assert!(is_fixed_size(&Type::Result(
            Box::new(Type::UInt(32)),
            Box::new(Type::Bool)
        )));
        assert!(!is_fixed_size(&Type::Result(
            Box::new(Type::Slice(Box::new(Type::UInt(8)))),
            Box::new(Type::UInt(32))
        )));
        assert_eq!(fixed_byte_size(&Type::Slice(Box::new(Type::UInt(8)))), None);
    }

    #[test]
    fn cost_expr_algebra_and_call_substitution() {
        let caller = Node {
            spec: false,
            effects: EffectMask(0),
            params: vec![
                Type::Array(Box::new(Type::UInt(32)), 8),
                Type::Slice(Box::new(Type::UInt(8))),
            ],
            results: vec![Type::UInt(32)],
            learnable: vec![],
            consts: vec![Const {
                ty: Type::UInt(64),
                bytes: 5u64.to_le_bytes().to_vec(),
            }],
            calls: vec![],
            ops: vec![],
            outputs: vec![Operand::Input(0)],
            contract: Contract::default(),
            debug: None,
        };

        let e = CostExpr::add(
            CostExpr::Const(1),
            CostExpr::mul(CostExpr::Len(Operand::Input(0)), CostExpr::Const(2)),
        );
        // Substituting Input(0) with Array<u32, 8> folds to Const(17).
        assert_eq!(
            e.substitute_for_call(&[Operand::Input(0)], &caller, 0, false),
            CostExpr::Const(17)
        );
        // Substituting Input(0) with Slice<u8> keeps Len(Input(1)).
        let s = e.substitute_for_call(&[Operand::Input(1)], &caller, 0, false);
        assert_eq!(s.lower_bound(), 1);
        assert_eq!(CostExpr::align16(CostExpr::Const(0)), CostExpr::Const(0));
        assert_eq!(CostExpr::align16(CostExpr::Const(1)), CostExpr::Const(16));
        assert_eq!(
            CostExpr::align16(CostExpr::Val(Operand::Const(0))),
            CostExpr::Align16(Box::new(CostExpr::Val(Operand::Const(0))))
        );
    }
}
