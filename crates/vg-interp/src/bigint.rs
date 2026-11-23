//! Exact arbitrary-precision signed integers for contract formulas (docs/13).

use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, Mul, Neg, Sub};

/// Arbitrary-precision signed integer in little-endian base-2^32 representation.
/// Zero is uniquely represented as `{ neg: false, limbs: [] }`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BigInt {
    neg: bool,
    limbs: Vec<u32>,
}

impl BigInt {
    pub fn zero() -> Self {
        Self {
            neg: false,
            limbs: Vec::new(),
        }
    }

    pub fn one() -> Self {
        Self {
            neg: false,
            limbs: vec![1],
        }
    }

    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    pub fn is_negative(&self) -> bool {
        self.neg && !self.limbs.is_empty()
    }

    pub fn abs(&self) -> Self {
        Self {
            neg: false,
            limbs: self.limbs.clone(),
        }
    }

    pub fn to_i128(&self) -> Option<i128> {
        if self.limbs.len() > 4 {
            return None;
        }
        let mut mag = 0u128;
        for (i, &l) in self.limbs.iter().enumerate() {
            mag |= u128::from(l) << (32 * i);
        }
        if !self.neg {
            i128::try_from(mag).ok()
        } else if mag == (i128::MAX as u128) + 1 {
            Some(i128::MIN)
        } else if mag <= i128::MAX as u128 {
            Some(-(mag as i128))
        } else {
            None
        }
    }

    pub fn to_u128(&self) -> Option<u128> {
        if self.neg || self.limbs.len() > 4 {
            return None;
        }
        let mut mag = 0u128;
        for (i, &l) in self.limbs.iter().enumerate() {
            mag |= u128::from(l) << (32 * i);
        }
        Some(mag)
    }

    pub fn to_usize(&self) -> Option<usize> {
        self.to_u128().and_then(|u| usize::try_from(u).ok())
    }

    fn normalize(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
        if self.limbs.is_empty() {
            self.neg = false;
        }
    }

    fn from_u128_with_sign(neg: bool, mut mag: u128) -> Self {
        let mut limbs = Vec::new();
        while mag != 0 {
            limbs.push(mag as u32);
            mag >>= 32;
        }
        let mut out = Self { neg, limbs };
        out.normalize();
        out
    }

    /// Truncated-toward-zero division and remainder:
    /// `self == q * rhs + r`, `|r| < |rhs|`, and `r` has the sign of `self` (docs/03, docs/14).
    /// Returns `None` when `rhs == 0`.
    pub fn div_rem(&self, rhs: &Self) -> Option<(Self, Self)> {
        if rhs.is_zero() {
            return None;
        }
        let (q_limbs, r_limbs) = div_rem_mag(&self.limbs, &rhs.limbs);
        let mut q = Self {
            neg: self.neg ^ rhs.neg,
            limbs: q_limbs,
        };
        q.normalize();
        let mut r = Self {
            neg: self.neg,
            limbs: r_limbs,
        };
        r.normalize();
        Some((q, r))
    }
}

impl From<i128> for BigInt {
    fn from(v: i128) -> Self {
        Self::from_u128_with_sign(v < 0, v.unsigned_abs())
    }
}

impl From<u128> for BigInt {
    fn from(v: u128) -> Self {
        Self::from_u128_with_sign(false, v)
    }
}

impl From<i64> for BigInt {
    fn from(v: i64) -> Self {
        Self::from(i128::from(v))
    }
}

impl From<u64> for BigInt {
    fn from(v: u64) -> Self {
        Self::from(u128::from(v))
    }
}

impl From<usize> for BigInt {
    fn from(v: usize) -> Self {
        Self::from(v as u128)
    }
}

impl From<i32> for BigInt {
    fn from(v: i32) -> Self {
        Self::from(i128::from(v))
    }
}

impl From<u32> for BigInt {
    fn from(v: u32) -> Self {
        Self::from(u128::from(v))
    }
}

impl Ord for BigInt {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.is_negative(), other.is_negative()) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            (false, false) => cmp_mag(&self.limbs, &other.limbs),
            (true, true) => cmp_mag(&other.limbs, &self.limbs),
        }
    }
}

impl PartialOrd for BigInt {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Neg for &BigInt {
    type Output = BigInt;

    fn neg(self) -> BigInt {
        if self.is_zero() {
            BigInt::zero()
        } else {
            BigInt {
                neg: !self.neg,
                limbs: self.limbs.clone(),
            }
        }
    }
}

impl Neg for BigInt {
    type Output = BigInt;

    fn neg(mut self) -> BigInt {
        if !self.is_zero() {
            self.neg = !self.neg;
        }
        self
    }
}

impl Add for &BigInt {
    type Output = BigInt;

    fn add(self, rhs: &BigInt) -> BigInt {
        if self.neg == rhs.neg {
            let mut out = BigInt {
                neg: self.neg,
                limbs: add_mag(&self.limbs, &rhs.limbs),
            };
            out.normalize();
            out
        } else {
            match cmp_mag(&self.limbs, &rhs.limbs) {
                Ordering::Equal => BigInt::zero(),
                Ordering::Greater => {
                    let mut out = BigInt {
                        neg: self.neg,
                        limbs: sub_mag(&self.limbs, &rhs.limbs),
                    };
                    out.normalize();
                    out
                }
                Ordering::Less => {
                    let mut out = BigInt {
                        neg: rhs.neg,
                        limbs: sub_mag(&rhs.limbs, &self.limbs),
                    };
                    out.normalize();
                    out
                }
            }
        }
    }
}

impl Add for BigInt {
    type Output = BigInt;

    fn add(self, rhs: BigInt) -> BigInt {
        &self + &rhs
    }
}

impl Sub for &BigInt {
    type Output = BigInt;

    fn sub(self, rhs: &BigInt) -> BigInt {
        self + &(-rhs)
    }
}

impl Sub for BigInt {
    type Output = BigInt;

    fn sub(self, rhs: BigInt) -> BigInt {
        &self - &rhs
    }
}

impl Mul for &BigInt {
    type Output = BigInt;

    fn mul(self, rhs: &BigInt) -> BigInt {
        if self.is_zero() || rhs.is_zero() {
            return BigInt::zero();
        }
        let mut out = BigInt {
            neg: self.neg ^ rhs.neg,
            limbs: mul_mag(&self.limbs, &rhs.limbs),
        };
        out.normalize();
        out
    }
}

impl Mul for BigInt {
    type Output = BigInt;

    fn mul(self, rhs: BigInt) -> BigInt {
        &self * &rhs
    }
}

impl fmt::Display for BigInt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_zero() {
            return f.write_str("0");
        }
        if self.neg {
            f.write_str("-")?;
        }
        const BASE: u32 = 1_000_000_000;
        let mut tmp = self.limbs.clone();
        let mut chunks = Vec::new();
        while !tmp.is_empty() {
            chunks.push(div_u32_mag(&mut tmp, BASE));
        }
        if let Some((last, rest)) = chunks.split_last() {
            write!(f, "{last}")?;
            for c in rest.iter().rev() {
                write!(f, "{c:09}")?;
            }
        }
        Ok(())
    }
}

fn trim(v: &mut Vec<u32>) {
    while v.last() == Some(&0) {
        v.pop();
    }
}

fn cmp_mag(a: &[u32], b: &[u32]) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| {
        for i in (0..a.len()).rev() {
            let ord = a[i].cmp(&b[i]);
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    })
}

fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    let n = a.len().max(b.len());
    let mut out = Vec::with_capacity(n + 1);
    let mut carry = 0u64;
    for i in 0..n {
        let av = u64::from(a.get(i).copied().unwrap_or(0));
        let bv = u64::from(b.get(i).copied().unwrap_or(0));
        let sum = av + bv + carry;
        out.push(sum as u32);
        carry = sum >> 32;
    }
    if carry != 0 {
        out.push(carry as u32);
    }
    trim(&mut out);
    out
}

/// Computes `a - b` assuming `a >= b`.
fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len());
    let mut borrow = 0i64;
    for (i, &limb_a) in a.iter().enumerate() {
        let av = i64::from(limb_a);
        let bv = i64::from(b.get(i).copied().unwrap_or(0));
        let mut diff = av - bv - borrow;
        if diff < 0 {
            diff += 1i64 << 32;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out.push(diff as u32);
    }
    debug_assert_eq!(borrow, 0);
    trim(&mut out);
    out
}

fn mul_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0u32; a.len() + b.len()];
    for (i, &ai) in a.iter().enumerate() {
        let mut carry = 0u64;
        for (j, &bj) in b.iter().enumerate() {
            let prod = u64::from(ai) * u64::from(bj) + u64::from(out[i + j]) + carry;
            out[i + j] = prod as u32;
            carry = prod >> 32;
        }
        out[i + b.len()] = carry as u32;
    }
    trim(&mut out);
    out
}

fn shl1_add_bit(v: &mut Vec<u32>, bit: u32) {
    let mut carry = bit;
    for limb in v.iter_mut() {
        let next_carry = *limb >> 31;
        *limb = (*limb << 1) | carry;
        carry = next_carry;
    }
    if carry != 0 {
        v.push(carry);
    }
    trim(v);
}

fn div_rem_mag(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
    debug_assert!(!b.is_empty());
    if cmp_mag(a, b) == Ordering::Less {
        return (Vec::new(), a.to_vec());
    }
    let top_bits = 32 - a.last().expect("non-empty").leading_zeros() as usize;
    let nbits = (a.len() - 1) * 32 + top_bits;
    let mut q = vec![0u32; a.len()];
    let mut r = Vec::new();
    for bit_idx in (0..nbits).rev() {
        let a_bit = (a[bit_idx / 32] >> (bit_idx % 32)) & 1;
        shl1_add_bit(&mut r, a_bit);
        if cmp_mag(&r, b) != Ordering::Less {
            r = sub_mag(&r, b);
            q[bit_idx / 32] |= 1u32 << (bit_idx % 32);
        }
    }
    trim(&mut q);
    trim(&mut r);
    (q, r)
}

fn div_u32_mag(a: &mut Vec<u32>, d: u32) -> u32 {
    let d64 = u64::from(d);
    let mut rem = 0u64;
    for limb in a.iter_mut().rev() {
        let cur = (rem << 32) | u64::from(*limb);
        *limb = (cur / d64) as u32;
        rem = cur % d64;
    }
    trim(a);
    rem as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_normalization_and_bounds() {
        let z = BigInt::from(0i128);
        assert!(z.is_zero());
        assert!(!z.is_negative());
        assert_eq!(-z.clone(), z);
        assert_eq!(BigInt::from(5) - BigInt::from(5), z);
        assert_eq!(BigInt::from(-5) + BigInt::from(5), z);
        assert_eq!(BigInt::from(0) * BigInt::from(-999), z);
        assert_eq!(z.to_string(), "0");

        let min = BigInt::from(i128::MIN);
        assert_eq!(min.to_i128(), Some(i128::MIN));
        assert_eq!(min.to_string(), "-170141183460469231731687303715884105728");

        let umax = BigInt::from(u128::MAX);
        assert_eq!(umax.to_i128(), None);
        assert_eq!(umax.to_u128(), Some(u128::MAX));
        assert_eq!(umax.to_string(), "340282366920938463463374607431768211455");
    }

    #[test]
    fn large_mul_exceeds_i128() {
        let a = BigInt::from(u64::MAX);
        let prod = &a * &a;
        assert_eq!(prod.to_i128(), None);
        // (2^64 - 1)^2 = 2^128 - 2^65 + 1 = 340282366920938463426481119284349108225
        assert_eq!(prod.to_string(), "340282366920938463426481119284349108225");
        let (q, r) = prod.div_rem(&a).unwrap();
        assert_eq!(q, a);
        assert!(r.is_zero());

        let (q2, r2) = (&prod + &BigInt::from(12345u64))
            .div_rem(&(&a + &BigInt::one()))
            .unwrap();
        // ((2^64 - 1)^2 + 12345) = (2^64 - 2) * 2^64 + 12346
        assert_eq!(q2, &a - &BigInt::one());
        assert_eq!(r2, BigInt::from(12346u64));
    }

    #[test]
    fn signed_truncating_div_rem() {
        assert_eq!(
            BigInt::from(7).div_rem(&BigInt::from(3)),
            Some((BigInt::from(2), BigInt::from(1)))
        );
        assert_eq!(
            BigInt::from(-7).div_rem(&BigInt::from(3)),
            Some((BigInt::from(-2), BigInt::from(-1)))
        );
        assert_eq!(
            BigInt::from(7).div_rem(&BigInt::from(-3)),
            Some((BigInt::from(-2), BigInt::from(1)))
        );
        assert_eq!(
            BigInt::from(-7).div_rem(&BigInt::from(-3)),
            Some((BigInt::from(2), BigInt::from(-1)))
        );
        assert_eq!(BigInt::from(7).div_rem(&BigInt::zero()), None);
    }

    #[test]
    fn ordering_signed() {
        let mut xs = [
            BigInt::from(100i128),
            BigInt::from(-1i128),
            BigInt::from(i128::MIN),
            BigInt::zero(),
            &BigInt::from(i128::MIN) * &BigInt::from(2),
            &BigInt::from(i128::MAX) * &BigInt::from(2),
        ];
        xs.sort();
        assert!(xs[0] < xs[1] && xs[1] < xs[2] && xs[2] < xs[3] && xs[3] < xs[4] && xs[4] < xs[5]);
        assert_eq!(xs[3], BigInt::zero());
    }
}
