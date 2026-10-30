//! The opcode table of docs/14: numbers, modes, argument shapes and commutativity.

/// Which modes an op takes, and their mode bytes (docs/14, "Modes").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Modes {
    /// No mode; mode byte 0.
    None,
    /// checked 0, wrap 1, sat 2; required.
    IntArith,
    /// checked 0, wrap 1; required.
    Shift,
    /// s 1, u 2, f 3; required.
    Compare,
    /// plain 0, `.branch` 1.
    Guard,
    /// The target scalar type; mode byte 0, because the target is the result type.
    Cast,
}

/// How an op's VG-Sym arguments become operands. Each shape fixes the operand order of docs/02:
/// positional arguments in order, named arguments last in the order listed here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Exactly n value arguments.
    Fixed(u8),
    /// At least n value arguments (TUPLE).
    AtLeast(u8),
    /// A kernel label, then at least one value: SIMD_MAP(@f, xs, b…), GENERATE(@f, n, b…).
    Kernel,
    /// FOLD_BOUNDED(@f, xs, b…, init=a, invariant=@I).
    Fold,
    /// ASSERT_GUARD(c, @f, a…, else=@g).
    Guard,
    /// CALL_HASH(@f, a…).
    Call,
}

/// Commutativity, for canonicalization step 4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Comm {
    No,
    /// Both operands are sorted by hash.
    All,
    /// Only the first two operands are sorted (FMA).
    FirstTwo,
}

#[derive(Debug)]
pub struct OpInfo {
    pub code: u16,
    pub name: &'static str,
    pub modes: Modes,
    pub shape: Shape,
    pub comm: Comm,
    /// False for ops the MVP rejects (VG-S0104).
    pub mvp: bool,
}

pub const IDENTITY: u16 = 0x0002;
pub const TUPLE: u16 = 0x0003;
pub const PROJ: u16 = 0x0004;
pub const CAST: u16 = 0x0006;
pub const OK: u16 = 0x0008;
pub const ERR: u16 = 0x0009;
pub const FOLD_BOUNDED: u16 = 0x0101;
pub const GENERATE: u16 = 0x0107;

macro_rules! ops {
    ($($code:literal $name:literal $modes:ident $shape:expr, $comm:ident, $mvp:literal;)*) => {
        pub static OPS: &[OpInfo] = &[$(OpInfo {
            code: $code, name: $name, modes: Modes::$modes, shape: $shape, comm: Comm::$comm, mvp: $mvp,
        }),*];
    };
}

use Shape::*;
ops! {
    0x0001 "CONST" None Fixed(1), No, false;
    0x0002 "IDENTITY" None Fixed(1), No, true;
    0x0003 "TUPLE" None AtLeast(1), No, true;
    0x0004 "PROJ" None Fixed(2), No, true;
    0x0005 "SELECT" None Fixed(3), No, true;
    0x0006 "CAST" Cast Fixed(1), No, true;
    0x0007 "AS_BYTES" None Fixed(1), No, true;
    0x0008 "OK" None Fixed(1), No, true;
    0x0009 "ERR" None Fixed(1), No, true;
    0x000A "IS_OK" None Fixed(1), No, true;
    0x000B "GET_OK" None Fixed(1), No, true;
    0x000C "GET_ERR" None Fixed(1), No, true;
    0x0010 "ADD" IntArith Fixed(2), All, true;
    0x0011 "SUB" IntArith Fixed(2), No, true;
    0x0012 "MUL" IntArith Fixed(2), All, true;
    0x0013 "DIV" IntArith Fixed(2), No, true;
    0x0014 "MOD" IntArith Fixed(2), No, true;
    0x0015 "NEG" IntArith Fixed(1), No, true;
    0x0020 "TENSOR_DOT" None Fixed(2), No, false;
    0x0021 "TENSOR_CONV" None Fixed(2), No, false;
    0x0022 "TENSOR_RESHAPE" None Fixed(1), No, false;
    0x0030 "AND" None Fixed(2), All, true;
    0x0031 "OR" None Fixed(2), All, true;
    0x0032 "XOR" None Fixed(2), All, true;
    0x0033 "NOT" None Fixed(1), No, true;
    0x0034 "SHL" Shift Fixed(2), No, true;
    0x0035 "SHR" Shift Fixed(2), No, true;
    0x0036 "ROTL" None Fixed(2), No, true;
    0x0037 "ROTR" None Fixed(2), No, true;
    0x0040 "FADD" None Fixed(2), All, true;
    0x0041 "FSUB" None Fixed(2), No, true;
    0x0042 "FMUL" None Fixed(2), All, true;
    0x0043 "FDIV" None Fixed(2), No, true;
    0x0044 "FMA" None Fixed(3), FirstTwo, true;
    0x0045 "FNEG" None Fixed(1), No, true;
    0x0046 "FABS" None Fixed(1), No, true;
    0x0047 "FMIN" None Fixed(2), All, true;
    0x0048 "FMAX" None Fixed(2), All, true;
    0x0049 "FCLAMP" None Fixed(3), No, true;
    0x004A "FCOPYSIGN" None Fixed(2), No, true;
    0x004B "FSQRT" None Fixed(1), No, true;
    0x0050 "EQ" None Fixed(2), All, true;
    0x0051 "NE" None Fixed(2), All, true;
    0x0052 "LT" Compare Fixed(2), No, true;
    0x0053 "LE" Compare Fixed(2), No, true;
    0x0054 "GT" Compare Fixed(2), No, true;
    0x0055 "GE" Compare Fixed(2), No, true;
    0x0056 "BAND" None Fixed(2), All, true;
    0x0057 "BOR" None Fixed(2), All, true;
    0x0058 "BNOT" None Fixed(1), No, true;
    0x0060 "INDEX" None Fixed(2), No, true;
    0x0061 "UPDATE" None Fixed(3), No, true;
    0x0062 "LEN" None Fixed(1), No, true;
    0x0063 "SLICE" None Fixed(3), No, true;
    0x0064 "CONCAT" None Fixed(2), No, true;
    0x0100 "SIMD_MAP" None Kernel, No, true;
    0x0101 "FOLD_BOUNDED" None Fold, No, true;
    0x0102 "PARTITION_BY" None Kernel, No, false;
    0x0103 "SCAN_PREFIX" None Kernel, No, false;
    0x0104 "MATCH_DISJOINT" None Fixed(1), No, false;
    0x0105 "ASSERT_GUARD" Guard Guard, No, true;
    0x0106 "REC_CALL" None Fixed(1), No, false;
    0x0107 "GENERATE" None Kernel, No, true;
    0x0200 "ARENA_ALLOC" None Fixed(1), No, false;
    0x0300 "CALL_HASH" None Call, No, true;
    0x0400 "FILE_READ" None Fixed(3), No, true;
    0x0401 "FILE_WRITE" None Fixed(3), No, true;
    0x0402 "CLOCK_READ" None Fixed(2), No, true;
    0x0403 "NET_READ" None Fixed(3), No, false;
    0x0404 "NET_WRITE" None Fixed(3), No, false;
    0x0405 "GPU_DISPATCH" None Fixed(2), No, false;
    0x0406 "RANDOM" None Fixed(1), No, false;
    0x0407 "PARAM_UPDATE" None Fixed(2), No, false;
    0x0408 "EMIT" None Fixed(2), No, false;
    0x0409 "HOST_CALL" None Fixed(2), No, false;
    0x040A "SPAWN" None Fixed(2), No, false;
    0x0500 "SAMPLE" None Fixed(1), No, false;
}

pub fn by_name(name: &str) -> Option<&'static OpInfo> {
    OPS.iter().find(|o| o.name == name)
}

pub fn by_code(code: u16) -> Option<&'static OpInfo> {
    OPS.iter().find(|o| o.code == code)
}

impl OpInfo {
    /// Effect ops (0x0400–0x04FF) act on the environment; they are never merged or dropped.
    pub fn is_effect(&self) -> bool {
        self.code & 0xFF00 == 0x0400
    }

    /// The mode byte of a mode written in VG-Sym, or None when this op does not take that mode.
    /// CAST's mode is its target type and is checked by the caller.
    pub fn mode_byte(&self, mode: Option<&str>) -> Option<u8> {
        match (self.modes, mode) {
            (Modes::None, None) | (Modes::Guard, None) => Some(0),
            (Modes::IntArith | Modes::Shift, Some("checked")) => Some(0),
            (Modes::IntArith | Modes::Shift, Some("wrap")) => Some(1),
            (Modes::IntArith, Some("sat")) => Some(2),
            (Modes::Compare, Some("s")) => Some(1),
            (Modes::Compare, Some("u")) => Some(2),
            (Modes::Compare, Some("f")) => Some(3),
            (Modes::Guard, Some("branch")) => Some(1),
            (Modes::Cast, Some(_)) => Some(0),
            _ => None,
        }
    }

    /// The VG-Sym mode of a mode byte, for disassembly. CAST is printed by the caller.
    pub fn mode_name(&self, byte: u8) -> Option<Option<&'static str>> {
        match (self.modes, byte) {
            (Modes::None, 0) | (Modes::Guard, 0) => Some(None),
            (Modes::IntArith | Modes::Shift, 0) => Some(Some("checked")),
            (Modes::IntArith | Modes::Shift, 1) => Some(Some("wrap")),
            (Modes::IntArith, 2) => Some(Some("sat")),
            (Modes::Compare, 1) => Some(Some("s")),
            (Modes::Compare, 2) => Some(Some("u")),
            (Modes::Compare, 3) => Some(Some("f")),
            (Modes::Guard, 1) => Some(Some("branch")),
            (Modes::Cast, 0) => Some(None),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_consistent() {
        for (i, a) in OPS.iter().enumerate() {
            assert_eq!(by_code(a.code).map(|o| o.name), Some(a.name));
            assert!(
                OPS[i + 1..]
                    .iter()
                    .all(|b| b.code > a.code && b.name != a.name)
            );
        }
        let add = by_name("ADD").unwrap();
        assert_eq!(add.mode_byte(Some("sat")), Some(2));
        assert_eq!(add.mode_byte(None), None);
        assert_eq!(by_name("LT").unwrap().mode_byte(Some("f")), Some(3));
        assert_eq!(by_name("FADD").unwrap().mode_byte(Some("wrap")), None);
        assert!(by_name("CLOCK_READ").unwrap().is_effect());
        assert_eq!(by_name("FMA").unwrap().comm, Comm::FirstTwo);
    }
}
