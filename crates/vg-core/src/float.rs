//! Decimal literals to IEEE bits with round-to-nearest-even (docs/03, "Literals"), and back.

use crate::types::Type;

/// Canonical NaN bits (ADR 0002).
pub fn canonical_nan(t: &Type) -> u64 {
    match t {
        Type::Float(16) => 0x7E00,
        Type::BFloat16 => 0x7FC0,
        Type::Float(32) => 0x7FC0_0000,
        _ => 0x7FF8_0000_0000_0000,
    }
}

/// (exponent bits, fraction bits) of a float type.
fn format(t: &Type) -> (u32, u32) {
    match t {
        Type::Float(16) => (5, 10),
        Type::BFloat16 => (8, 7),
        Type::Float(32) => (8, 23),
        _ => (11, 52),
    }
}

/// True when the bits are a NaN of type t.
pub fn is_nan(t: &Type, bits: u64) -> bool {
    let (e, m) = format(t);
    let exp = (bits >> m) & ((1 << e) - 1);
    exp == (1 << e) - 1 && bits & ((1 << m) - 1) != 0
}

/// True when the bits are ±∞ of type t.
pub fn is_inf(t: &Type, bits: u64) -> bool {
    let (e, m) = format(t);
    (bits >> m) & ((1 << e) - 1) == (1 << e) - 1 && bits & ((1 << m) - 1) == 0
}

/// The value of an unsigned decimal or integer literal in float type t, or None when it overflows
/// to infinity (VG-P0006). `neg` applies the sign afterwards, so `-0.0` is negative zero.
pub fn parse(text: &str, neg: bool, t: &Type) -> Option<u64> {
    let (e, m) = format(t);
    let bits = match t {
        Type::Float(32) => u64::from(text.parse::<f32>().ok()?.to_bits()),
        Type::Float(64) => text.parse::<f64>().ok()?.to_bits(),
        _ => {
            // f16 and bf16: round the correctly rounded f64 once more. Only a value that lands exactly
            // on a midpoint of the narrow format can round wrongly twice; settle those exactly.
            let d = text.parse::<f64>().ok()?;
            if d.is_infinite() {
                return None;
            }
            use std::cmp::Ordering::{Greater, Less};
            match round_from_f64(d, e, m) {
                (bits, Some(up)) => match (compare_decimal(text, d), up) {
                    (Greater, false) => bits + 1,
                    (Less, true) => bits - 1,
                    _ => bits,
                },
                (bits, None) => bits,
            }
        }
    };
    if is_inf(t, bits) {
        return None;
    }
    Some(if neg { bits | 1 << (e + m) } else { bits })
}

/// Rounds a finite, non-negative f64 to the format (e, m), ties to even. Returns the bits and, when
/// d lay exactly halfway between two values of the format, whether that tie rounded up. Positive
/// floats' bit patterns are consecutive, so the other neighbour is one pattern away.
fn round_from_f64(d: f64, e: u32, m: u32) -> (u64, Option<bool>) {
    let bias = (1i32 << (e - 1)) - 1;
    let emin = 1 - bias;
    if d == 0.0 {
        return (0, None);
    }
    let exp = d.log2().floor() as i32;
    // Correct log2's rounding near powers of two.
    let exp = if 2f64.powi(exp) > d {
        exp - 1
    } else if 2f64.powi(exp + 1) <= d {
        exp + 1
    } else {
        exp
    };
    if exp > bias {
        return (inf_bits(e, m), None);
    }
    let q = exp.max(emin) - m as i32;
    let n = d * 2f64.powi(-q);
    let r0 = n.round_ties_even();
    let tie = (n.fract() == 0.5).then_some(r0 > n);
    let (mut r, mut qe) = (r0 as u64, q);
    if r == 1 << (m + 1) {
        r >>= 1;
        qe += 1;
    }
    if r < 1 << m {
        // Subnormal or zero: exponent field 0.
        return (r, tie);
    }
    let field = qe + m as i32 + bias;
    if field >= (1 << e) - 1 {
        return (inf_bits(e, m), tie);
    }
    ((field as u64) << m | (r - (1 << m)), tie)
}

fn inf_bits(e: u32, m: u32) -> u64 {
    ((1 << e) - 1) << m
}

/// Compares a decimal literal with an f64 exactly, through the f64's exact decimal expansion.
fn compare_decimal(text: &str, d: f64) -> std::cmp::Ordering {
    let exact = format!("{d:.1100e}");
    let a = digits(text);
    let b = digits(&exact);
    // Both are non-zero here (a tie needs a non-zero midpoint).
    a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0))
}

/// A positive decimal as (significant digits without leading or trailing zeros, exponent of the
/// leading digit), so that comparing the pairs compares the values.
fn digits(s: &str) -> (Vec<u8>, i64) {
    let (mant, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], s[i + 1..].parse::<i64>().unwrap_or(0)),
        None => (s, 0),
    };
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    let all: Vec<u8> = int.bytes().chain(frac.bytes()).map(|c| c - b'0').collect();
    let lead = all.iter().position(|&c| c != 0).unwrap_or(all.len());
    let mut sig = all[lead..].to_vec();
    while sig.last() == Some(&0) {
        sig.pop();
    }
    (sig, int.len() as i64 - lead as i64 + exp)
}

/// The value of float bits, exactly, as an f64 (every f16, bf16 and f32 value is one).
pub fn to_f64(t: &Type, bits: u64) -> f64 {
    match t {
        Type::Float(64) => f64::from_bits(bits),
        Type::Float(32) => f64::from(f32::from_bits(bits as u32)),
        Type::BFloat16 => f64::from(f32::from_bits((bits as u32) << 16)),
        _ => {
            let sign = if bits & 0x8000 != 0 { -1.0 } else { 1.0 };
            let exp = ((bits >> 10) & 0x1F) as i32;
            let frac = (bits & 0x3FF) as f64;
            sign * match exp {
                0 => frac * 2f64.powi(-24),
                0x1F if frac == 0.0 => f64::INFINITY,
                0x1F => f64::NAN,
                _ => (1.0 + frac / 1024.0) * 2f64.powi(exp - 15),
            }
        }
    }
}

/// The shortest decimal that reads back as these bits, and its sign, for printing a literal.
/// None for NaN and ±∞, which VG-Sym cannot write.
pub fn shortest(t: &Type, bits: u64) -> Option<(bool, String)> {
    if is_nan(t, bits) || is_inf(t, bits) {
        return None;
    }
    let v = to_f64(t, bits);
    let text = match t {
        Type::Float(64) => format!("{:?}", v.abs()),
        // f16, bf16 and f32 values are f32 values; f32's shortest form rounds back through f64.
        _ => format!("{:?}", (v as f32).abs()),
    };
    Some((v.is_sign_negative(), text))
}

#[cfg(test)]
mod tests {
    use super::*;

    const F16: Type = Type::Float(16);

    #[test]
    fn f32_rounding_matches_docs_03() {
        let b = parse("0.99", false, &Type::Float(32)).unwrap();
        let exact = format!("{:.22}", f64::from(f32::from_bits(b as u32)));
        assert_eq!(exact, "0.9900000095367431640625");
        assert_eq!(parse("3.5e38", false, &Type::Float(32)), None);
        assert_eq!(parse("0.0", true, &Type::Float(32)), Some(0x8000_0000));
    }

    #[test]
    fn f16_and_bf16() {
        assert_eq!(parse("1", false, &F16), Some(0x3C00));
        assert_eq!(parse("65504", false, &F16), Some(0x7BFF));
        assert_eq!(parse("65519.99", false, &F16), Some(0x7BFF));
        assert_eq!(parse("65520", false, &F16), None);
        assert_eq!(parse("5.960464477539063e-8", false, &F16), Some(1));
        assert_eq!(parse("2.9e-8", false, &F16), Some(0));
        assert_eq!(parse("3.0e-8", false, &F16), Some(1));
        assert_eq!(parse("1", true, &Type::BFloat16), Some(0xBF80));
        // 1 + 2^-11 is the f16 midpoint between 1 and 1 + 2^-10: ties to even, down to 1.
        assert_eq!(parse("1.00048828125", false, &F16), Some(0x3C00));
        // Just above the midpoint, by less than f64 can see: f64 rounds it onto the midpoint, and
        // only the exact comparison rounds it up.
        assert_eq!(
            parse("1.0004882812500000000000000001", false, &F16),
            Some(0x3C01)
        );
        // 1 + 3·2^-11 is the midpoint between odd 0x3C01 and even 0x3C02: ties up to even.
        assert_eq!(parse("1.00146484375", false, &F16), Some(0x3C02));
        assert_eq!(
            parse("1.0014648437499999999999999", false, &F16),
            Some(0x3C01)
        );
        assert_eq!(parse("1.00390625", false, &Type::BFloat16), Some(0x3F80));
        assert_eq!(
            parse("1.00390625000000000000001", false, &Type::BFloat16),
            Some(0x3F81)
        );
    }

    #[test]
    fn printing_round_trips() {
        for (t, bits) in [
            (F16, 0x3C01u64),
            (F16, 0x0001),
            (F16, 0x7BFF),
            (Type::BFloat16, 0x3F81),
            (Type::Float(32), 0x7F7F_FFFF),
            (Type::Float(64), 0x0000_0000_0000_0001),
            (Type::Float(32), 0x8000_0000),
        ] {
            let (neg, text) = shortest(&t, bits).unwrap();
            assert_eq!(parse(&text, neg, &t), Some(bits), "{t} {text}");
        }
        assert_eq!(shortest(&F16, 0x7E00), None);
        assert_eq!(shortest(&Type::Float(32), 0xFF80_0000), None);
        assert!(is_nan(&Type::Float(32), 0x7FC0_0001) && !is_nan(&F16, 0x7C00));
    }
}
