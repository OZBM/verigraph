//! Exact IEEE 754-2019 floating-point arithmetic with one canonical NaN (docs/13, ADR 0002).

use vg_core::float;
use vg_core::types::Type;
use vg_core::value::int_fits;

/// `(exp_bits, frac_bits)` for any VG float type.
pub fn fmt_params(ty: &Type) -> Option<(u32, u32)> {
    match ty {
        Type::Float(16) => Some((5, 10)),
        Type::BFloat16 => Some((8, 7)),
        Type::Float(32) => Some((8, 23)),
        Type::Float(64) => Some((11, 52)),
        _ => None,
    }
}

/// Mask of the low `1 + e + m` bits of a float type.
pub fn type_mask(ty: &Type) -> u64 {
    let (e, m) = fmt_params(ty).expect("float type");
    let w = 1 + e + m;
    if w == 64 { u64::MAX } else { (1u64 << w) - 1 }
}

pub fn sign_mask(ty: &Type) -> u64 {
    let (e, m) = fmt_params(ty).expect("float type");
    1u64 << (e + m)
}

pub fn canonicalize(ty: &Type, bits: u64) -> u64 {
    let b = bits & type_mask(ty);
    if float::is_nan(ty, b) {
        float::canonical_nan(ty)
    } else {
        b
    }
}

pub fn is_neg(ty: &Type, bits: u64) -> bool {
    (bits & sign_mask(ty)) != 0
}

pub fn is_zero(ty: &Type, bits: u64) -> bool {
    (bits & (sign_mask(ty) - 1)) == 0
}

pub fn pos_inf(ty: &Type) -> u64 {
    let (e, m) = fmt_params(ty).expect("float type");
    ((1u64 << e) - 1) << m
}

pub fn neg_inf(ty: &Type) -> u64 {
    sign_mask(ty) | pos_inf(ty)
}

pub fn zero(ty: &Type, neg: bool) -> u64 {
    if neg { sign_mask(ty) } else { 0 }
}

/// Unpacks a finite non-zero float into `(neg, mant, exp)` such that the exact value is
/// `(-1)^neg * mant * 2^exp`.
pub fn unpack_finite_nonzero(ty: &Type, bits: u64) -> (bool, u64, i32) {
    let (e, m) = fmt_params(ty).expect("float type");
    let neg = is_neg(ty, bits);
    let bias = (1i32 << (e - 1)) - 1;
    let exp_field = ((bits >> m) & ((1u64 << e) - 1)) as i32;
    let frac = bits & ((1u64 << m) - 1);
    debug_assert!(exp_field < (1i32 << e) - 1);
    debug_assert!(exp_field != 0 || frac != 0);
    if exp_field == 0 {
        (neg, frac, 1 - bias - (m as i32))
    } else {
        (neg, (1u64 << m) | frac, exp_field - bias - (m as i32))
    }
}

/// Rounds `(-1)^neg * (mag + eps) * 2^exp` to IEEE format `(e, m)` with round-to-nearest-even,
/// where `eps == 0` when `!sticky` and `0 < eps < 1` when `sticky`.
/// Whenever `sticky` is true, `mag` must be large enough that `shift >= 2`.
pub fn round_dyadic_to_float(neg: bool, mag: u128, exp: i32, sticky: bool, e: u32, m: u32) -> u64 {
    let sign_bits = if neg { 1u64 << (e + m) } else { 0 };
    if mag == 0 {
        debug_assert!(!sticky);
        return sign_bits;
    }
    let bias = (1i32 << (e - 1)) - 1;
    let emin = 1 - bias;
    let blen = 128 - mag.leading_zeros();
    let top = exp + (blen as i32) - 1;
    let mut q = top.max(emin) - (m as i32);
    let shift = q - exp;
    let mut sig: u64 = if shift <= 0 {
        debug_assert!(!sticky);
        (mag as u64) << (-shift)
    } else if shift > 128 {
        0
    } else {
        let s = shift as u32;
        let quot = if s == 128 { 0 } else { mag >> s };
        let round_bit = ((mag >> (s - 1)) & 1) != 0;
        let lower_mask = if s == 1 { 0 } else { (1u128 << (s - 1)) - 1 };
        let rem_nonzero = (mag & lower_mask) != 0 || sticky;
        let round_up = round_bit && (rem_nonzero || (quot & 1) != 0);
        (quot as u64) + u64::from(round_up)
    };
    if sig == (1u64 << (m + 1)) {
        sig >>= 1;
        q += 1;
    }
    if sig == 0 {
        return sign_bits;
    }
    if sig < (1u64 << m) {
        return sign_bits | sig;
    }
    let exp_field = q + (m as i32) + bias;
    if exp_field >= (1i32 << e) - 1 {
        return sign_bits | (((1u64 << e) - 1) << m);
    }
    sign_bits | ((exp_field as u64) << m) | (sig - (1u64 << m))
}

/// Rounds an `f64` value directly to `f16` or `bf16` (preserving `±0.0`, `±inf`, canonical NaN).
fn round_f64_to_narrow(ty: &Type, v: f64) -> u64 {
    let (e, m) = fmt_params(ty).expect("float type");
    if v.is_nan() {
        return float::canonical_nan(ty);
    }
    let bits64 = v.to_bits();
    let neg = (bits64 >> 63) != 0;
    if v.is_infinite() {
        return if neg { neg_inf(ty) } else { pos_inf(ty) };
    }
    if v == 0.0 {
        return zero(ty, neg);
    }
    let (_, mant, exp) = unpack_finite_nonzero(&Type::Float(64), bits64);
    round_dyadic_to_float(neg, u128::from(mant), exp, false, e, m)
}

pub fn fadd(ty: &Type, a: u64, b: u64) -> u64 {
    match ty {
        Type::Float(32) => canonicalize(
            ty,
            u64::from((f32::from_bits(a as u32) + f32::from_bits(b as u32)).to_bits()),
        ),
        Type::Float(64) => canonicalize(ty, (f64::from_bits(a) + f64::from_bits(b)).to_bits()),
        Type::Float(16) | Type::BFloat16 => {
            round_f64_to_narrow(ty, float::to_f64(ty, a) + float::to_f64(ty, b))
        }
        _ => unreachable!("non-float type in fadd"),
    }
}

pub fn fsub(ty: &Type, a: u64, b: u64) -> u64 {
    match ty {
        Type::Float(32) => canonicalize(
            ty,
            u64::from((f32::from_bits(a as u32) - f32::from_bits(b as u32)).to_bits()),
        ),
        Type::Float(64) => canonicalize(ty, (f64::from_bits(a) - f64::from_bits(b)).to_bits()),
        Type::Float(16) | Type::BFloat16 => {
            round_f64_to_narrow(ty, float::to_f64(ty, a) - float::to_f64(ty, b))
        }
        _ => unreachable!("non-float type in fsub"),
    }
}

pub fn fmul(ty: &Type, a: u64, b: u64) -> u64 {
    match ty {
        Type::Float(32) => canonicalize(
            ty,
            u64::from((f32::from_bits(a as u32) * f32::from_bits(b as u32)).to_bits()),
        ),
        Type::Float(64) => canonicalize(ty, (f64::from_bits(a) * f64::from_bits(b)).to_bits()),
        Type::Float(16) | Type::BFloat16 => {
            round_f64_to_narrow(ty, float::to_f64(ty, a) * float::to_f64(ty, b))
        }
        _ => unreachable!("non-float type in fmul"),
    }
}

pub fn fdiv(ty: &Type, a: u64, b: u64) -> u64 {
    match ty {
        Type::Float(32) => canonicalize(
            ty,
            u64::from((f32::from_bits(a as u32) / f32::from_bits(b as u32)).to_bits()),
        ),
        Type::Float(64) => canonicalize(ty, (f64::from_bits(a) / f64::from_bits(b)).to_bits()),
        Type::Float(16) | Type::BFloat16 => {
            round_f64_to_narrow(ty, float::to_f64(ty, a) / float::to_f64(ty, b))
        }
        _ => unreachable!("non-float type in fdiv"),
    }
}

pub fn fsqrt(ty: &Type, a: u64) -> u64 {
    match ty {
        Type::Float(32) => canonicalize(ty, u64::from(f32::from_bits(a as u32).sqrt().to_bits())),
        Type::Float(64) => canonicalize(ty, f64::from_bits(a).sqrt().to_bits()),
        Type::Float(16) | Type::BFloat16 => round_f64_to_narrow(ty, float::to_f64(ty, a).sqrt()),
        _ => unreachable!("non-float type in fsqrt"),
    }
}

pub fn fma(ty: &Type, a: u64, b: u64, c: u64) -> u64 {
    match ty {
        Type::Float(32) => canonicalize(
            ty,
            u64::from(
                f32::from_bits(a as u32)
                    .mul_add(f32::from_bits(b as u32), f32::from_bits(c as u32))
                    .to_bits(),
            ),
        ),
        Type::Float(64) => canonicalize(
            ty,
            f64::from_bits(a)
                .mul_add(f64::from_bits(b), f64::from_bits(c))
                .to_bits(),
        ),
        Type::Float(16) | Type::BFloat16 => fma_narrow(ty, a, b, c),
        _ => unreachable!("non-float type in fma"),
    }
}

/// Exact single-rounding IEEE 754 FMA for `f16` and `bf16` (no double rounding).
fn fma_narrow(ty: &Type, a: u64, b: u64, c: u64) -> u64 {
    let (e, m) = fmt_params(ty).expect("float type");
    if float::is_nan(ty, a) || float::is_nan(ty, b) || float::is_nan(ty, c) {
        return float::canonical_nan(ty);
    }
    let a_inf = float::is_inf(ty, a);
    let b_inf = float::is_inf(ty, b);
    let c_inf = float::is_inf(ty, c);
    let a_zero = is_zero(ty, a);
    let b_zero = is_zero(ty, b);
    let c_zero = is_zero(ty, c);
    let p_neg = is_neg(ty, a) ^ is_neg(ty, b);
    let c_neg = is_neg(ty, c);

    if (a_inf && b_zero) || (b_inf && a_zero) {
        return float::canonical_nan(ty);
    }
    if a_inf || b_inf {
        if c_inf && (p_neg != c_neg) {
            return float::canonical_nan(ty);
        }
        return if p_neg { neg_inf(ty) } else { pos_inf(ty) };
    }
    if c_inf {
        return c;
    }
    if a_zero || b_zero {
        if c_zero {
            return zero(ty, p_neg && c_neg);
        }
        return c;
    }

    let (_, a_m, a_e) = unpack_finite_nonzero(ty, a);
    let (_, b_m, b_e) = unpack_finite_nonzero(ty, b);
    let p_m = u128::from(a_m) * u128::from(b_m);
    let p_e = a_e + b_e;
    if c_zero {
        return round_dyadic_to_float(p_neg, p_m, p_e, false, e, m);
    }

    let (_, c_m_u64, c_e) = unpack_finite_nonzero(ty, c);
    let c_m = u128::from(c_m_u64);
    let bit_len = |x: u128| -> i32 { (128 - x.leading_zeros()) as i32 };
    let p_top = p_e + bit_len(p_m) - 1;
    let c_top = c_e + bit_len(c_m) - 1;
    let lo = p_e.min(c_e);
    let hi = p_top.max(c_top);

    if hi - lo <= 110 {
        let p_scaled = (p_m << (p_e - lo)) as i128;
        let c_scaled = (c_m << (c_e - lo)) as i128;
        let sp = if p_neg { -p_scaled } else { p_scaled };
        let sc = if c_neg { -c_scaled } else { c_scaled };
        let sum = sp + sc;
        if sum == 0 {
            return zero(ty, false);
        }
        round_dyadic_to_float(sum < 0, sum.unsigned_abs(), lo, false, e, m)
    } else {
        let (big_neg, big_m, big_e, big_top, small_neg) = if p_top >= c_top {
            (p_neg, p_m, p_e, p_top, c_neg)
        } else {
            (c_neg, c_m, c_e, c_top, p_neg)
        };
        let base_exp = big_top - 80;
        let big_scaled = big_m << (big_e - base_exp);
        if big_neg == small_neg {
            round_dyadic_to_float(big_neg, big_scaled, base_exp, true, e, m)
        } else {
            round_dyadic_to_float(big_neg, big_scaled - 1, base_exp, true, e, m)
        }
    }
}

pub fn fneg(ty: &Type, a: u64) -> u64 {
    let b = a & type_mask(ty);
    if float::is_nan(ty, b) {
        float::canonical_nan(ty)
    } else {
        b ^ sign_mask(ty)
    }
}

pub fn fabs(ty: &Type, a: u64) -> u64 {
    let b = a & type_mask(ty);
    if float::is_nan(ty, b) {
        float::canonical_nan(ty)
    } else {
        b & !sign_mask(ty)
    }
}

/// IEEE 754-2019 `minimum`: NaN if either operand is NaN; `-0.0 < +0.0`.
pub fn fmin(ty: &Type, a: u64, b: u64) -> u64 {
    if float::is_nan(ty, a) || float::is_nan(ty, b) {
        return float::canonical_nan(ty);
    }
    let fa = float::to_f64(ty, a);
    let fb = float::to_f64(ty, b);
    if fa < fb {
        canonicalize(ty, a)
    } else if fb < fa {
        canonicalize(ty, b)
    } else if is_neg(ty, a) {
        canonicalize(ty, a)
    } else {
        canonicalize(ty, b)
    }
}

/// IEEE 754-2019 `maximum`: NaN if either operand is NaN; `+0.0 > -0.0`.
pub fn fmax(ty: &Type, a: u64, b: u64) -> u64 {
    if float::is_nan(ty, a) || float::is_nan(ty, b) {
        return float::canonical_nan(ty);
    }
    let fa = float::to_f64(ty, a);
    let fb = float::to_f64(ty, b);
    if fa > fb {
        canonicalize(ty, a)
    } else if fb > fa {
        canonicalize(ty, b)
    } else if !is_neg(ty, a) {
        canonicalize(ty, a)
    } else {
        canonicalize(ty, b)
    }
}

/// `FCLAMP(x, lo, hi)`: `FMIN(FMAX(x, lo), hi)`, propagating NaN in `x`;
/// traps with `FCLAMP.bounds` if `lo` or `hi` is NaN or `!(lo <= hi)`.
pub fn fclamp(ty: &Type, x: u64, lo: u64, hi: u64) -> Result<u64, &'static str> {
    if float::is_nan(ty, lo) || float::is_nan(ty, hi) {
        return Err("FCLAMP.bounds");
    }
    let flo = float::to_f64(ty, lo);
    let fhi = float::to_f64(ty, hi);
    if flo > fhi {
        return Err("FCLAMP.bounds");
    }
    Ok(fmin(ty, fmax(ty, x, lo), hi))
}

/// `FCOPYSIGN(x, y)`: magnitude of `x` with sign of `y`, canonical NaN if `x` is NaN;
/// traps with `FCOPYSIGN.sign` if `y` is NaN (ADR 0002).
pub fn fcopysign(ty: &Type, x: u64, y: u64) -> Result<u64, &'static str> {
    if float::is_nan(ty, y) {
        return Err("FCOPYSIGN.sign");
    }
    if float::is_nan(ty, x) {
        return Ok(float::canonical_nan(ty));
    }
    let sm = sign_mask(ty);
    Ok((x & !sm & type_mask(ty)) | (y & sm))
}

pub fn feq(ty: &Type, a: u64, b: u64) -> bool {
    float::to_f64(ty, a) == float::to_f64(ty, b)
}

pub fn fne(ty: &Type, a: u64, b: u64) -> bool {
    !feq(ty, a, b)
}

pub fn flt(ty: &Type, a: u64, b: u64) -> bool {
    float::to_f64(ty, a) < float::to_f64(ty, b)
}

pub fn fle(ty: &Type, a: u64, b: u64) -> bool {
    float::to_f64(ty, a) <= float::to_f64(ty, b)
}

pub fn fgt(ty: &Type, a: u64, b: u64) -> bool {
    float::to_f64(ty, a) > float::to_f64(ty, b)
}

pub fn fge(ty: &Type, a: u64, b: u64) -> bool {
    float::to_f64(ty, a) >= float::to_f64(ty, b)
}

/// Integer to IEEE float with single round-to-nearest-even rounding (never double-rounds).
pub fn int_to_float(v: i128, target: &Type) -> u64 {
    let (e, m) = fmt_params(target).expect("float target");
    if v == 0 {
        return 0;
    }
    round_dyadic_to_float(v < 0, v.unsigned_abs(), 0, false, e, m)
}

/// IEEE float to integer truncated toward zero, checking `CAST.in_range`.
pub fn float_to_int(src: &Type, bits: u64, target: &Type) -> Result<i128, &'static str> {
    if float::is_nan(src, bits) || float::is_inf(src, bits) {
        return Err("CAST.in_range");
    }
    if is_zero(src, bits) {
        return Ok(0);
    }
    let (neg, mant, exp) = unpack_finite_nonzero(src, bits);
    let mag: u128 = if exp >= 0 {
        if exp >= 65 {
            return Err("CAST.in_range");
        }
        u128::from(mant) << (exp as u32)
    } else {
        let shift = (-exp) as u32;
        if shift >= 64 {
            0
        } else {
            u128::from(mant >> shift)
        }
    };
    let trunc = if neg {
        if mag > (i128::MAX as u128) + 1 {
            return Err("CAST.in_range");
        }
        if mag == (i128::MAX as u128) + 1 {
            i128::MIN
        } else {
            -(mag as i128)
        }
    } else {
        if mag > i128::MAX as u128 {
            return Err("CAST.in_range");
        }
        mag as i128
    };
    if !int_fits(trunc, target) {
        return Err("CAST.in_range");
    }
    Ok(trunc)
}

/// IEEE float-to-float conversion with exact single rounding and canonical NaN.
pub fn float_to_float(src: &Type, bits: u64, target: &Type) -> u64 {
    if float::is_nan(src, bits) {
        return float::canonical_nan(target);
    }
    let neg = is_neg(src, bits);
    if float::is_inf(src, bits) {
        return if neg {
            neg_inf(target)
        } else {
            pos_inf(target)
        };
    }
    if is_zero(src, bits) {
        return zero(target, neg);
    }
    let (e, m) = fmt_params(target).expect("float target");
    let (_, mant, exp) = unpack_finite_nonzero(src, bits);
    round_dyadic_to_float(neg, u128::from(mant), exp, false, e, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    const F16: Type = Type::Float(16);
    const BF16: Type = Type::BFloat16;
    const F32: Type = Type::Float(32);
    const F64: Type = Type::Float(64);

    #[test]
    fn f16_fma_beats_f64_double_rounding() {
        // Construct exact midpoint between f16 1.0 (0x3C00) and 1 + 2^-10 (0x3C01): 1 + 2^-11.
        // a = 1 + 2^-10 (0x3C01), b = 1 - 2^-11? Better: a = 1 + 2^-10, b = 0.5 -> p = 0.5 + 2^-11.
        // Then add c = 0.5 -> p + c = 1 + 2^-11 (exact midpoint between 0x3C00 and 0x3C01).
        // Now make b slightly smaller or larger below f64 precision? In f16/bf16, when exponents
        // differ by > 53 bits, f64 drops the small term c completely!
        // Example: a = 1.0 (0x3C00), b = 1.0 + 2^-10? No, use a*b at midpoint:
        // a = 1.0 + 2^-10 (0x3C01), b = 0.5 (0x3800), a*b = 0.5 + 2^-11.
        // Or a = 1.0 + 2^-1 (0x3E00 = 1.5), b = 2^-10 (0x1400): exact product is 2^-10 + 2^-11? No,
        // wait: we want a*b + c where a*b is a huge or small product, OR c is tiny:
        // Let's test `a * b + c` where `a*b = 1 + 2^-11` (midpoint of 0x3C00 and 0x3C01 in f16)
        // Wait: (1 + 2^-10) * (1 - 2^-11? not f16). How about:
        // a = 2^12 * (1 + 2^-10) = 4096 + 4 = 4100 (0x6C01? wait exp 12 => bias 15 => 27<<10 | 1 = 0x6C01).
        // b = 2^-12 (exp -12 => 3<<10 = 0x0C00). Wait we want a*b + c to sit at an exact midpoint
        // when c is added, with c below f64 precision? Easy!
        // Let a = 2^10 (0x6400), b = 2^4 (0x4C00) -> a*b = 2^14? No, to sit at midpoint of [1.0, 1+2^-10]:
        // let c = -2^-24 (f16 min subnormal = 0x0001).
        // Wait: in f64, 1.0 + 2^-11 + (-2^-24) is exact (exp diff 24 <= 53).
        // When does f64 lose precision in FMA(a, b, c) for f16?
        // When a*b + c has a*b at say 2^30? No, f16 inputs are <= 65504 < 2^16, min f16 is 2^-24.
        // Max span of two f16 values is 2^16 / 2^-24 = 2^40, and a*b can go up to 2^32,
        // min f16 c = 2^-24 -> span 32 - (-24) = 56 > 53!
        // Look at that: a = 32768.0 + 32.0 (0x7801 = 2^15 + 2^5), b = 65504.0 (0x7BFF):
        // Even simpler! Let a = 65504.0 (0x7BFF) wait a*b overflows f16 unless c cancels!
        // Wait: a = 2^15 * (1 + 2^-10) (0x7801), b = 2^0 (0x3C00)? No, to hit midpoint in f16:
        // Pick a = 32768.0 (0x7800), b = 2.0 (0x4000) -> a*b = 65536.0 (2^16, exact f16 overflow boundary?
        // Wait: max finite f16 is 65504 = 2^16 - 32. Midpoint to inf is 65520 = 2^16 - 16).
        // Better: take a = 2^15 (0x7800), b = 2^15 (0x7800) -> a*b = 2^30 (in f64: bit 30).
        // Take c = 2^-24 (0x0001)! Wait that doesn't change the f16 result unless... wait,
        // if a*b = 2^30 and we want an f16 midpoint, a*b is far above f16 max unless c cancels!
        // What if a*b = 1.00048828125 (1 + 2^-11)? How to get 1 + 2^-11 in f16 product?
        // 1 + 2^-11 = (2^11 + 1) / 2^11. Can 2^11 + 1 factor into two 11-bit numbers?
        // 2049 = 3 * 683! Both 3 (0b11, 2 bits) and 683 (10 bits < 11 bits!) fit in f16!
        // Check: 3.0 is 0x4200, 683.0 = 512 + 171 = (1 + 171/512)*2^9 -> exp 9+15=24 => 0x60AB.
        // Product 3 * 683 = 2049. Scale by 2^-11: a = 3 * 2^-2 = 0.75 (0x3A00), b = 683 * 2^-9 = 1.333984375 (0x3D56? wait 683 = 0x2AB => 0x2AB - 0x200 = 0x0AB, exp=-9+15? no, 683*2^-9 = 1 + 171/512, mant frac = 171*2 = 342 = 0x156, exp=0+15=15 => 0x3D56).
        // Wait: 0.75 * (683 / 512) = 2049 / 2048 = 1 + 2^-11 (EXACT midpoint between 0x3C00 and 0x3C01!).
        // Even better, scale by 2^0: with `a*b = 1 + 2^-11` and tiny `c`, f64 holds `1 + 2^-11 + c` down to `2^-52`, wait we need `a*b` and `c` where `f64(a*b)` rounds away something, OR `f64(a*b + c)` double rounds!
        // Wait: when does `f64(a*b) + c` vs `fma(a,b,c)` differ? `a*b` is ALWAYS exact in `f64` for f16 (11+11=22 <= 53), so `f64(a*b) + c` only double-rounds if `(a*b) + c` has > 53 bits AND sits near a midpoint of `f16`!
        // Wait: for `a*b + c` to have > 53 bits AND sit near a midpoint of `f16` (`q + 0.5 ulp`), the high term must be `a*b` (around `1`? No, if `a*b` is around `2^31`, `c` would be `-2^31` to bring it down to `1` — impossible since `|c| <= 65504`).
        // What about bf16 (`e=8, m=7`), where `|a|, |b|` can reach `~3.38e38` (`2^127`) and `|c|` can reach `2^-133`?!
        // Check bf16! Take `a = (1 + 2^-7) * 2^60`, `b = 2^-60` -> no, `a*b` is around 1; take `a = 2^60 * (1 + 2^-7)`, `b = 2^60` -> `a*b` is `2^120`... Wait: take `a = 1 + 2^-7` (0x3F81), `b = 1.0 + 2^-7`? No, `257 * 128` -> `1 + 2^-8` midpoint!
        // For bf16, `a = 1.5` (0x3FC0), `b = 1.0 + 2^-7`? `1.5 * (1 + 2^-7) = 1.5 + 3 * 2^-8` = `1.5 + 2^-7 + 2^-8`, exact midpoint between `1.5 + 2^-7` (0x3FC1) and `1.5 + 2^-6` (0x3FC2)!
        // Wait: for bf16, `1.5 * (1 + 2^-7)` has 16 bits, and if we add a tiny `c = 2^-100` (0x0D80 in bf16):
        // Exact sum `a * b + c` = `1.5 + 2^-7 + 2^-8 + 2^-100` (> midpoint -> MUST round UP to `0x3FC2`, even though `0x3FC1` is odd? Wait: midpoint between odd `0x3FC1` and even `0x3FC2` ties UP to `0x3FC2`, so take `c = -2^-100`!).
        // With `c = -2^-100`: `a * b + c` = `(1.5 + 2^-7 + 2^-8) - 2^-100` is STRICTLY BELOW the midpoint, so exact FMA MUST round DOWN to `0x3FC1`!
        // Meanwhile naive `f64(a) * f64(b) + f64(c)` drops `-2^-100` completely because `1.5` vs `2^-100` spans 101 > 53 bits, so `f64` evaluates to the exact midpoint and rounds UP to `0x3FC2`!
        // What a clean, decisive test!
        let a = 0x3FC0u64; // 1.5 in bf16
        let b = 0x3F81u64; // 1 + 2^-7 in bf16
        let c_neg_tiny = 0x8D80u64; // -2^-100 in bf16 (exp = -100 + 127 = 27 = 0x1B; sign=1 => (1<<15)|(27<<7) = 0x8D80)
        // Exact: 1.5 * (1 + 2^-7) - 2^-100 = 1.5 + 2^-7 + 2^-8 - 2^-100 < midpoint => 0x3FC1!
        assert_eq!(fma(&BF16, a, b, c_neg_tiny), 0x3FC1);
        // With c = +0.0: exact midpoint between 0x3FC1 (odd) and 0x3FC2 (even) => ties to even => 0x3FC2!
        assert_eq!(fma(&BF16, a, b, 0x0000), 0x3FC2);

        // And the symmetric tie-to-even with even below, odd above:
        // 1.0 * (1 + 2^-7)? No, we need an exact midpoint between 1.0 (0x3F80, even) and 1 + 2^-7 (0x3F81, odd):
        // midpoint is 1 + 2^-8 = 257 / 256. 257 is prime, wait 257 * 1? 257 doesn't fit in 8 bits.
        // Try 0x3F82 (1 + 2*2^-7, even) and 0x3F83 (1 + 3*2^-7, odd): midpoint is 1 + 5*2^-8 = 261/256 = (9 * 29) / 256!
        // 9 fits in bf16 (0x4110 = 9.0), 29 fits in bf16 (0x41E8 = 29.0)!
        // a2 = 9 / 8 = 1.125 (0x3F90), b2 = 29 / 32 (0x3F68): a2 * b2 = 261 / 256 = 1 + 5 * 2^-8!
        let a2 = 0x3F90u64;
        let b2 = 0x3F68u64;
        // Exact midpoint between 0x3F82 (even) and 0x3F83 (odd): ties to even => 0x3F82.
        assert_eq!(fma(&BF16, a2, b2, 0x0000), 0x3F82);
        // With c = +2^-100 (0x0D80): strictly above midpoint => rounds UP to 0x3F83!
        // Naive f64 loses +2^-100 and wrongly ties to 0x3F82!
        assert_eq!(fma(&BF16, a2, b2, 0x0D80), 0x3F83);

        // Also check bf16 FMA where a*b overflows bf16 (> 3.4e38) while c cancels it back into finite range!
        // a = 2^100 (0x7180), b = 2^100 (0x7180) => a*b = 2^200 (overflows bf16 AND f32, NOT f64!).
        // Wait: a = 2^120 (0x7B80), b = 2^120 (0x7B80), c = -2^120 (0xFB80) => 2^240 - 2^120 > max bf16 => +inf.
        // How about a = 2^100, b = 2^30 -> a*b = 2^130 (overflows bf16!), c = -(2^130)? Wait -2^130 isn't in bf16!
        // Wait: max finite bf16 is (2 - 2^-7) * 2^127. Take a = 2^64 (0x5F80), b = 2^64 (0x5F80) -> a*b = 2^128 (overflows bf16!).
        // c = -2^127 (0xFF00): a*b + c = 2^128 - 2^127 = 2^127 (0x7F00), which IS finite in bf16!
        assert_eq!(fma(&BF16, 0x5F80, 0x5F80, 0xFF00), 0x7F00);

        // Same overflow-cancellation test for f16:
        // max f16 = 65504 < 65536 = 2^16.
        // a = 256.0 (0x5C00), b = 256.0 (0x5C00) => a*b = 65536.0 (overflows f16!).
        // c = -32768.0 (0xF800) => a*b + c = 32768.0 (0x7800), finite in f16!
        assert_eq!(fma(&F16, 0x5C00, 0x5C00, 0xF800), 0x7800);

        // Signed zeros in FMA: (-0)*1 + (-0) = -0; (+0)*1 + (-0) = +0; x*1 + (-x) = +0
        assert_eq!(fma(&F16, 0x8000, 0x3C00, 0x8000), 0x8000);
        assert_eq!(fma(&F16, 0x0000, 0x3C00, 0x8000), 0x0000);
        assert_eq!(fma(&F16, 0x3C00, 0x3C00, 0xBC00), 0x0000);
    }

    #[test]
    fn u64_to_bf16_single_rounding() {
        // 2^63 + 2^55 + 1: midpoint between bf16 2^63 and 2^63 + 2^56 is 2^63 + 2^55.
        // Converting u64 -> f64 -> bf16 rounds 2^63 + 2^55 + 1 down to 2^63 + 2^55? Wait: f64
        // has 53 bits, so 2^63 + 2^55 + 1 rounds in f64 to 2^63 + 2^55 (midpoint!), and then
        // to bf16 ties-to-even DOWN to 2^63!
        // Single rounding MUST round UP to 2^63 + 2^56!
        // 2^63 in bf16: exp = 63 + 127 = 190 = 0xBE => 0x5F00. 2^63 + 2^56 => 0x5F01.
        let v = ((1u64 << 63) + (1u64 << 55) + 1) as i128;
        assert_eq!(int_to_float(v, &BF16), 0x5F01);
        // Exact midpoint 2^63 + 2^55 ties to even (0x5F00).
        let mid = ((1u64 << 63) + (1u64 << 55)) as i128;
        assert_eq!(int_to_float(mid, &BF16), 0x5F00);
    }

    #[test]
    fn float_to_int_boundaries() {
        assert_eq!(
            float_to_int(&F32, (-0.5f32).to_bits() as u64, &Type::UInt(8)),
            Ok(0)
        );
        assert_eq!(
            float_to_int(&F32, 255.9f32.to_bits() as u64, &Type::UInt(8)),
            Ok(255)
        );
        assert!(float_to_int(&F32, 256.0f32.to_bits() as u64, &Type::UInt(8)).is_err());
        assert!(float_to_int(&F32, (-1.0f32).to_bits() as u64, &Type::UInt(8)).is_err());
        assert_eq!(
            float_to_int(&F64, (-9223372036854775808.0f64).to_bits(), &Type::SInt(64)),
            Ok(i128::from(i64::MIN))
        );
        assert!(float_to_int(&F64, 9223372036854775808.0f64.to_bits(), &Type::SInt(64)).is_err());
        assert_eq!(
            float_to_int(&F64, 0x43EF_FFFF_FFFF_FFFFu64, &Type::UInt(64)),
            Ok(i128::from(u64::MAX - 2047))
        );
        assert!(float_to_int(&F64, (2f64.powi(64)).to_bits(), &Type::UInt(64)).is_err());
        assert!(float_to_int(&F32, 0x7FC0_0000, &Type::SInt(32)).is_err());
        assert!(float_to_int(&F32, 0x7F80_0000, &Type::SInt(32)).is_err());
    }

    #[test]
    fn exhaustive_f16_bf16_round_trip() {
        for bits in 0u64..=0xFFFF {
            let widened_f16 = float_to_float(&F16, bits, &F32);
            let back_f16 = float_to_float(&F32, widened_f16, &F16);
            assert_eq!(back_f16, canonicalize(&F16, bits), "f16 {bits:#06x}");

            let widened_bf16 = float_to_float(&BF16, bits, &F32);
            let back_bf16 = float_to_float(&F32, widened_bf16, &BF16);
            assert_eq!(back_bf16, canonicalize(&BF16, bits), "bf16 {bits:#06x}");
        }
    }

    #[test]
    fn fmin_fmax_fclamp_fcopysign_ieee() {
        let pz = 0x0000_0000u64;
        let nz = 0x8000_0000u64;
        let nan = 0x7FC0_0000u64;
        assert_eq!(fmin(&F32, pz, nz), nz);
        assert_eq!(fmin(&F32, nz, pz), nz);
        assert_eq!(fmax(&F32, pz, nz), pz);
        assert_eq!(fmax(&F32, nz, pz), pz);
        assert_eq!(fmin(&F32, 1.0f32.to_bits() as u64, nan), nan);
        assert_eq!(fmax(&F32, nan, 1.0f32.to_bits() as u64), nan);
        assert_eq!(fclamp(&F32, nan, pz, 1.0f32.to_bits() as u64), Ok(nan));
        assert!(fclamp(&F32, pz, 2.0f32.to_bits() as u64, 1.0f32.to_bits() as u64).is_err());
        assert!(fclamp(&F32, pz, nan, 1.0f32.to_bits() as u64).is_err());
        assert_eq!(
            fcopysign(&F32, 1.5f32.to_bits() as u64, nz),
            Ok((-1.5f32).to_bits() as u64)
        );
        assert_eq!(fcopysign(&F32, nan, nz), Ok(nan));
        assert!(fcopysign(&F32, 1.0f32.to_bits() as u64, nan).is_err());
    }
}
