//! Value types and effect masks (docs/04, docs/13).

use std::fmt;

/// A capability handle kind (docs/04). The number is its TYPES immediate (ADR 0001).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Handle {
    FileIn = 0,
    FileOut = 1,
    Socket = 2,
    Clock = 3,
    GpuQueue = 4,
}

impl Handle {
    pub const ALL: [Handle; 5] = [
        Handle::FileIn,
        Handle::FileOut,
        Handle::Socket,
        Handle::Clock,
        Handle::GpuQueue,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Handle::FileIn => "FileIn",
            Handle::FileOut => "FileOut",
            Handle::Socket => "Socket",
            Handle::Clock => "Clock",
            Handle::GpuQueue => "GpuQueue",
        }
    }

    pub fn from_u32(v: u32) -> Option<Handle> {
        Handle::ALL.get(v as usize).copied()
    }
}

/// A VG type. Lengths and dimensions are u32: a fixed-size type holds at most 2^32 − 1 elements
/// (docs/02, static limits), so they are TYPES immediates (ADR 0001).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Type {
    Bool,
    UInt(u8),
    SInt(u8),
    /// f16, f32 or f64, by bit width.
    Float(u8),
    BFloat16,
    /// The unbounded integers of formulas; only CONTRACT constants have this type.
    Int,
    Array(Box<Type>, u32),
    Slice(Box<Type>),
    Tensor(Box<Type>, Vec<u32>),
    Tuple(Vec<Type>),
    Result(Box<Type>, Box<Type>),
    Arena(u32),
    Eff,
    Handle(Handle),
}

/// TYPES tags (docs/02, ADR 0001). 0x18 (refinement) is reserved: the MVP stores refinements as
/// PRE and POST conjuncts.
pub mod tag {
    pub const BOOL: u8 = 0x01;
    pub const UINT: u8 = 0x02;
    pub const SINT: u8 = 0x03;
    pub const FLOAT: u8 = 0x04;
    pub const BFLOAT16: u8 = 0x05;
    pub const INT: u8 = 0x06;
    pub const ARRAY: u8 = 0x10;
    pub const SLICE: u8 = 0x11;
    pub const TENSOR: u8 = 0x12;
    pub const TUPLE: u8 = 0x13;
    pub const RESULT: u8 = 0x14;
    pub const ARENA: u8 = 0x15;
    pub const EFF: u8 = 0x16;
    pub const HANDLE: u8 = 0x17;
}

impl Type {
    pub fn is_int(&self) -> bool {
        matches!(self, Type::UInt(_) | Type::SInt(_))
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Type::Float(_) | Type::BFloat16)
    }

    /// Bool, an integer or a float.
    pub fn is_scalar_or_bool(&self) -> bool {
        matches!(self, Type::Bool) || self.is_int() || self.is_float()
    }

    /// The element type of an Array, Slice or Tensor.
    pub fn elem(&self) -> Option<&Type> {
        match self {
            Type::Array(t, _) | Type::Slice(t) | Type::Tensor(t, _) => Some(t),
            _ => None,
        }
    }

    /// Bytes of one value in AS_BYTES and CONSTS (docs/13), for fixed-size byte-representable types.
    pub fn byte_size(&self) -> Option<u64> {
        Some(match self {
            Type::Bool => 1,
            Type::UInt(b) | Type::SInt(b) | Type::Float(b) => u64::from(*b) / 8,
            Type::BFloat16 => 2,
            Type::Array(t, n) => t.byte_size()?.checked_mul(u64::from(*n))?,
            Type::Tensor(t, dims) => dims
                .iter()
                .try_fold(t.byte_size()?, |acc, d| acc.checked_mul(u64::from(*d)))?,
            _ => return None,
        })
    }

    /// The scalar type named by a VG-Sym keyword such as `u8` or `bf16`.
    pub fn scalar(name: &str) -> Option<Type> {
        Some(match name {
            "u8" => Type::UInt(8),
            "u16" => Type::UInt(16),
            "u32" => Type::UInt(32),
            "u64" => Type::UInt(64),
            "i8" => Type::SInt(8),
            "i16" => Type::SInt(16),
            "i32" => Type::SInt(32),
            "i64" => Type::SInt(64),
            "f16" => Type::Float(16),
            "f32" => Type::Float(32),
            "f64" => Type::Float(64),
            "bf16" => Type::BFloat16,
            _ => return None,
        })
    }

    /// The keyword of a scalar type.
    pub fn scalar_name(&self) -> Option<&'static str> {
        Some(match self {
            Type::UInt(8) => "u8",
            Type::UInt(16) => "u16",
            Type::UInt(32) => "u32",
            Type::UInt(64) => "u64",
            Type::SInt(8) => "i8",
            Type::SInt(16) => "i16",
            Type::SInt(32) => "i32",
            Type::SInt(64) => "i64",
            Type::Float(16) => "f16",
            Type::Float(32) => "f32",
            Type::Float(64) => "f64",
            Type::BFloat16 => "bf16",
            _ => return None,
        })
    }

    /// Structural bytes: `tag u8, argc u8`, then each argument, a nested type's structural bytes or a
    /// u32 immediate. Self-delimiting, so hashes never depend on TYPES numbering (ADR 0001).
    pub fn structural_bytes(&self, out: &mut Vec<u8>) {
        let imm = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
        match self {
            Type::Bool => out.extend([tag::BOOL, 0]),
            Type::UInt(b) => {
                out.extend([tag::UINT, 1]);
                imm(out, u32::from(*b));
            }
            Type::SInt(b) => {
                out.extend([tag::SINT, 1]);
                imm(out, u32::from(*b));
            }
            Type::Float(b) => {
                out.extend([tag::FLOAT, 1]);
                imm(out, u32::from(*b));
            }
            Type::BFloat16 => out.extend([tag::BFLOAT16, 0]),
            Type::Int => out.extend([tag::INT, 0]),
            Type::Array(t, n) => {
                out.extend([tag::ARRAY, 2]);
                t.structural_bytes(out);
                imm(out, *n);
            }
            Type::Slice(t) => {
                out.extend([tag::SLICE, 1]);
                t.structural_bytes(out);
            }
            Type::Tensor(t, dims) => {
                out.extend([tag::TENSOR, (1 + dims.len()) as u8]);
                t.structural_bytes(out);
                for d in dims {
                    imm(out, *d);
                }
            }
            Type::Tuple(ms) => {
                out.extend([tag::TUPLE, ms.len() as u8]);
                for m in ms {
                    m.structural_bytes(out);
                }
            }
            Type::Result(a, b) => {
                out.extend([tag::RESULT, 2]);
                a.structural_bytes(out);
                b.structural_bytes(out);
            }
            Type::Arena(n) => {
                out.extend([tag::ARENA, 1]);
                imm(out, *n);
            }
            Type::Eff => out.extend([tag::EFF, 0]),
            Type::Handle(h) => {
                out.extend([tag::HANDLE, 1]);
                imm(out, *h as u32);
            }
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(n) = self.scalar_name() {
            return f.write_str(n);
        }
        match self {
            Type::Bool => f.write_str("Bool"),
            Type::Int => f.write_str("Int"),
            Type::Array(t, n) => write!(f, "Array<{t}, {n}>"),
            Type::Slice(t) => write!(f, "Slice<{t}>"),
            Type::Tensor(t, dims) => {
                write!(f, "Tensor<{t}, [")?;
                for (i, d) in dims.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{d}")?;
                }
                f.write_str("]>")
            }
            Type::Tuple(ms) => {
                f.write_str("Tuple<")?;
                for (i, m) in ms.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{m}")?;
                }
                f.write_str(">")
            }
            Type::Result(a, b) => write!(f, "Result<{a}, {b}>"),
            Type::Arena(n) => write!(f, "Arena<{n}>"),
            Type::Eff => f.write_str("Eff"),
            Type::Handle(h) => f.write_str(h.name()),
            _ => unreachable!("scalar types are printed above"),
        }
    }
}

/// The u32 effect set of docs/04. PURE is the empty set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EffectMask(pub u32);

impl EffectMask {
    /// Effect names in bit order: bit i is `NAMES[i]`.
    pub const NAMES: [&'static str; 12] = [
        "ARENA",
        "CLOCK",
        "FILE_READ",
        "FILE_WRITE",
        "NET_READ",
        "NET_WRITE",
        "GPU",
        "NONDET",
        "PARAM_UPDATE",
        "TELEMETRY",
        "HOST_CALL",
        "SPAWN",
    ];
    pub const ARENA: u32 = 0x1;
    /// Bits 0x1000 and up are reserved and must be zero.
    pub const RESERVED: u32 = !0xFFF;

    pub fn bit(name: &str) -> Option<u32> {
        Self::NAMES.iter().position(|n| *n == name).map(|i| 1 << i)
    }

    /// The names of the set bits, in bit order.
    pub fn names(self) -> Vec<&'static str> {
        (0..12)
            .filter(|i| self.0 & (1 << i) != 0)
            .map(|i| Self::NAMES[i])
            .collect()
    }

    /// True when the mask has no bit other than ARENA: such ops may be merged and dropped.
    pub fn is_pure(self) -> bool {
        self.0 & !Self::ARENA == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structural_bytes_are_self_delimiting() {
        let t = Type::Tuple(vec![
            Type::Array(Box::new(Type::Float(32)), 4),
            Type::Result(Box::new(Type::UInt(8)), Box::new(Type::Eff)),
        ]);
        let mut b = Vec::new();
        t.structural_bytes(&mut b);
        assert_eq!(
            b,
            [
                0x13, 2, 0x10, 2, 0x04, 1, 32, 0, 0, 0, 4, 0, 0, 0, 0x14, 2, 0x02, 1, 8, 0, 0, 0,
                0x16, 0
            ]
        );
    }

    #[test]
    fn byte_sizes_and_masks() {
        let t = Type::Tensor(Box::new(Type::Float(32)), vec![512, 2]);
        assert_eq!(t.byte_size(), Some(4096));
        assert_eq!(Type::Slice(Box::new(Type::UInt(8))).byte_size(), None);
        assert_eq!(EffectMask(0x5).names(), ["ARENA", "FILE_READ"]);
        assert_eq!(EffectMask::bit("SPAWN"), Some(0x800));
        assert!(EffectMask(1).is_pure() && !EffectMask(2).is_pure());
        assert_eq!(t.to_string(), "Tensor<f32, [512, 2]>");
    }
}
