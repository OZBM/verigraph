//! Value validation, NaN canonicalization, CONSTS decoding, AS_BYTES, and equality (docs/13).

use vg_core::ir::Const;
use vg_core::types::Type;
use vg_core::value::{Value, int_fits};

use crate::float_ops;

/// Checks that `val` has type `ty` and canonicalizes any float NaN patterns.
pub fn validate_and_canonicalize(ty: &Type, val: &Value) -> Result<Value, String> {
    match (ty, val) {
        (Type::Bool, Value::Bool(b)) => Ok(Value::Bool(*b)),
        (Type::UInt(_) | Type::SInt(_), Value::Int(v)) => {
            if int_fits(*v, ty) {
                Ok(Value::Int(*v))
            } else {
                Err(format!("integer {v} out of range for {ty}"))
            }
        }
        (t, Value::Float(bits)) if t.is_float() => {
            let mask = float_ops::type_mask(t);
            if *bits & !mask != 0 {
                return Err(format!("float bits {bits:#x} exceed width of {t}"));
            }
            Ok(Value::Float(float_ops::canonicalize(t, *bits)))
        }
        (Type::Array(elem, n), Value::Seq(xs)) => {
            if xs.len() != *n as usize {
                return Err(format!("expected Array length {n}, got {}", xs.len()));
            }
            let out = xs
                .iter()
                .map(|x| validate_and_canonicalize(elem, x))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Seq(out))
        }
        (Type::Slice(elem), Value::Seq(xs)) => {
            let out = xs
                .iter()
                .map(|x| validate_and_canonicalize(elem, x))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Seq(out))
        }
        (Type::Tensor(elem, dims), Value::Seq(xs)) => {
            let expected = dims
                .iter()
                .try_fold(1usize, |a, &d| a.checked_mul(d as usize))
                .ok_or_else(|| format!("tensor dimensions overflow usize for {ty}"))?;
            if xs.len() != expected {
                return Err(format!(
                    "expected Tensor element count {expected}, got {}",
                    xs.len()
                ));
            }
            let out = xs
                .iter()
                .map(|x| validate_and_canonicalize(elem, x))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Seq(out))
        }
        (Type::Tuple(ts), Value::Tuple(xs)) => {
            if xs.len() != ts.len() {
                return Err(format!(
                    "expected Tuple length {}, got {}",
                    ts.len(),
                    xs.len()
                ));
            }
            let out = ts
                .iter()
                .zip(xs)
                .map(|(t, x)| validate_and_canonicalize(t, x))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Tuple(out))
        }
        (Type::Result(ok_t, _), Value::Ok(v)) => {
            Ok(Value::Ok(Box::new(validate_and_canonicalize(ok_t, v)?)))
        }
        (Type::Result(_, err_t), Value::Err(v)) => {
            Ok(Value::Err(Box::new(validate_and_canonicalize(err_t, v)?)))
        }
        (Type::Eff, Value::Eff) => Ok(Value::Eff),
        (Type::Handle(_), Value::Handle(id)) => Ok(Value::Handle(*id)),
        _ => Err(format!("value {val:?} does not match type {ty}")),
    }
}

/// Decodes a `vg_core::ir::Const` into a runtime `Value`.
pub fn decode_const(c: &Const) -> Result<Value, String> {
    decode_const_bytes(&c.ty, &c.bytes)
}

fn decode_const_bytes(ty: &Type, b: &[u8]) -> Result<Value, String> {
    match ty {
        Type::Bool => {
            if b.len() != 1 || b[0] > 1 {
                return Err(format!("invalid Bool constant bytes {b:?}"));
            }
            Ok(Value::Bool(b[0] == 1))
        }
        Type::UInt(bits) => {
            let nbytes = usize::from(*bits) / 8;
            if b.len() != nbytes {
                return Err(format!("expected {nbytes} bytes for {ty}, got {}", b.len()));
            }
            let mut w = [0u8; 8];
            w[..nbytes].copy_from_slice(b);
            Ok(Value::Int(i128::from(u64::from_le_bytes(w))))
        }
        Type::SInt(bits) => {
            let nbytes = usize::from(*bits) / 8;
            if b.len() != nbytes {
                return Err(format!("expected {nbytes} bytes for {ty}, got {}", b.len()));
            }
            let v = match *bits {
                8 => i128::from(b[0] as i8),
                16 => i128::from(i16::from_le_bytes([b[0], b[1]])),
                32 => i128::from(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                64 => i128::from(i64::from_le_bytes(b.try_into().expect("8 bytes"))),
                _ => return Err(format!("unsupported SInt width {bits}")),
            };
            Ok(Value::Int(v))
        }
        t if t.is_float() => {
            let nbytes = t.byte_size().expect("float size") as usize;
            if b.len() != nbytes {
                return Err(format!("expected {nbytes} bytes for {t}, got {}", b.len()));
            }
            let mut w = [0u8; 8];
            w[..nbytes].copy_from_slice(b);
            let bits = u64::from_le_bytes(w);
            Ok(Value::Float(float_ops::canonicalize(t, bits)))
        }
        Type::Array(elem, n) => {
            let esz = elem
                .byte_size()
                .ok_or_else(|| format!("array constant element {elem} has no byte size"))?
                as usize;
            let expected = esz
                .checked_mul(*n as usize)
                .ok_or_else(|| "array constant byte size overflow".to_string())?;
            if b.len() != expected {
                return Err(format!(
                    "expected {expected} bytes for {ty}, got {}",
                    b.len()
                ));
            }
            let mut elems = Vec::with_capacity(*n as usize);
            for chunk in b.chunks(esz) {
                elems.push(decode_const_bytes(elem, chunk)?);
            }
            Ok(Value::Seq(elems))
        }
        Type::Tensor(elem, dims) => {
            let esz = elem
                .byte_size()
                .ok_or_else(|| format!("tensor constant element {elem} has no byte size"))?
                as usize;
            let count = dims
                .iter()
                .try_fold(1usize, |a, &d| a.checked_mul(d as usize))
                .ok_or_else(|| "tensor constant element count overflow".to_string())?;
            let expected = esz
                .checked_mul(count)
                .ok_or_else(|| "tensor constant byte size overflow".to_string())?;
            if b.len() != expected {
                return Err(format!(
                    "expected {expected} bytes for {ty}, got {}",
                    b.len()
                ));
            }
            let mut elems = Vec::with_capacity(count);
            for chunk in b.chunks(esz) {
                elems.push(decode_const_bytes(elem, chunk)?);
            }
            Ok(Value::Seq(elems))
        }
        _ => Err(format!("unsupported constant type {ty}")),
    }
}

/// Encodes a value into its canonical little-endian, row-major bytes (`AS_BYTES`, docs/13).
pub fn value_to_bytes(ty: &Type, val: &Value) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    write_bytes(ty, val, &mut out)?;
    Ok(out)
}

fn write_bytes(ty: &Type, val: &Value, out: &mut Vec<u8>) -> Result<(), String> {
    match (ty, val) {
        (Type::Bool, Value::Bool(b)) => out.push(u8::from(*b)),
        (Type::UInt(w) | Type::SInt(w), Value::Int(v)) => {
            if !int_fits(*v, ty) {
                return Err(format!("integer {v} does not fit {ty}"));
            }
            let nbytes = usize::from(*w) / 8;
            out.extend_from_slice(&v.to_le_bytes()[..nbytes]);
        }
        (t, Value::Float(bits)) if t.is_float() => {
            let nbytes = t.byte_size().expect("float size") as usize;
            let cbits = float_ops::canonicalize(t, *bits);
            out.extend_from_slice(&cbits.to_le_bytes()[..nbytes]);
        }
        (Type::Array(elem, n), Value::Seq(xs)) => {
            if elem.byte_size().is_none() || xs.len() != *n as usize {
                return Err(format!("AS_BYTES unsupported or wrong length for {ty}"));
            }
            for x in xs {
                write_bytes(elem, x, out)?;
            }
        }
        (Type::Tensor(elem, dims), Value::Seq(xs)) => {
            let count = dims
                .iter()
                .try_fold(1usize, |a, &d| a.checked_mul(d as usize))
                .ok_or_else(|| "tensor dimension overflow".to_string())?;
            if elem.byte_size().is_none() || xs.len() != count {
                return Err(format!("AS_BYTES unsupported or wrong length for {ty}"));
            }
            for x in xs {
                write_bytes(elem, x, out)?;
            }
        }
        (Type::Slice(elem), Value::Seq(xs)) => {
            if elem.byte_size().is_none() {
                return Err(format!("AS_BYTES unsupported for {ty}"));
            }
            for x in xs {
                write_bytes(elem, x, out)?;
            }
        }
        _ => return Err(format!("AS_BYTES has no byte form for {ty}")),
    }
    Ok(())
}

/// IEEE equality at scalars, element-wise on collections, tuples and Results (docs/13).
pub fn val_eq(ty: &Type, a: &Value, b: &Value) -> Result<bool, String> {
    Ok(match (ty, a, b) {
        (Type::Bool, Value::Bool(x), Value::Bool(y)) => x == y,
        (Type::UInt(_) | Type::SInt(_), Value::Int(x), Value::Int(y)) => x == y,
        (t, Value::Float(x), Value::Float(y)) if t.is_float() => float_ops::feq(t, *x, *y),
        (
            Type::Array(elem, _) | Type::Slice(elem) | Type::Tensor(elem, _),
            Value::Seq(xs),
            Value::Seq(ys),
        ) => {
            if xs.len() != ys.len() {
                return Ok(false);
            }
            for (x, y) in xs.iter().zip(ys) {
                if !val_eq(elem, x, y)? {
                    return Ok(false);
                }
            }
            true
        }
        (Type::Tuple(ts), Value::Tuple(xs), Value::Tuple(ys)) => {
            if xs.len() != ts.len() || ys.len() != ts.len() {
                return Err(format!("tuple length mismatch for {ty}"));
            }
            for ((t, x), y) in ts.iter().zip(xs).zip(ys) {
                if !val_eq(t, x, y)? {
                    return Ok(false);
                }
            }
            true
        }
        (Type::Result(ok_t, _), Value::Ok(x), Value::Ok(y)) => val_eq(ok_t, x, y)?,
        (Type::Result(_, err_t), Value::Err(x), Value::Err(y)) => val_eq(err_t, x, y)?,
        (Type::Result(..), Value::Ok(_), Value::Err(_))
        | (Type::Result(..), Value::Err(_), Value::Ok(_)) => false,
        (Type::Eff, Value::Eff, Value::Eff) => true,
        (Type::Handle(_), Value::Handle(x), Value::Handle(y)) => x == y,
        _ => return Err(format!("cannot compare {a:?} and {b:?} at type {ty}")),
    })
}

/// Bit/identity equality `same(a, b)`: both NaN is true, `-0.0` vs `+0.0` is false (docs/13).
pub fn val_same(ty: &Type, a: &Value, b: &Value) -> Result<bool, String> {
    Ok(match (ty, a, b) {
        (Type::Bool, Value::Bool(x), Value::Bool(y)) => x == y,
        (Type::UInt(_) | Type::SInt(_), Value::Int(x), Value::Int(y)) => x == y,
        (t, Value::Float(x), Value::Float(y)) if t.is_float() => {
            float_ops::canonicalize(t, *x) == float_ops::canonicalize(t, *y)
        }
        (
            Type::Array(elem, _) | Type::Slice(elem) | Type::Tensor(elem, _),
            Value::Seq(xs),
            Value::Seq(ys),
        ) => {
            if xs.len() != ys.len() {
                return Ok(false);
            }
            for (x, y) in xs.iter().zip(ys) {
                if !val_same(elem, x, y)? {
                    return Ok(false);
                }
            }
            true
        }
        (Type::Tuple(ts), Value::Tuple(xs), Value::Tuple(ys)) => {
            if xs.len() != ts.len() || ys.len() != ts.len() {
                return Err(format!("tuple length mismatch for {ty}"));
            }
            for ((t, x), y) in ts.iter().zip(xs).zip(ys) {
                if !val_same(t, x, y)? {
                    return Ok(false);
                }
            }
            true
        }
        (Type::Result(ok_t, _), Value::Ok(x), Value::Ok(y)) => val_same(ok_t, x, y)?,
        (Type::Result(_, err_t), Value::Err(x), Value::Err(y)) => val_same(err_t, x, y)?,
        (Type::Result(..), Value::Ok(_), Value::Err(_))
        | (Type::Result(..), Value::Err(_), Value::Ok(_)) => false,
        (Type::Eff, Value::Eff, Value::Eff) => true,
        (Type::Handle(_), Value::Handle(x), Value::Handle(y)) => x == y,
        _ => return Err(format!("cannot check same({a:?}, {b:?}) at type {ty}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_decoding_signed_and_tensor() {
        let c = Const {
            ty: Type::SInt(16),
            bytes: (-1234i16).to_le_bytes().to_vec(),
        };
        assert_eq!(decode_const(&c), Ok(Value::Int(-1234)));

        let t = Const {
            ty: Type::Tensor(Box::new(Type::UInt(8)), vec![2, 2]),
            bytes: vec![1, 2, 3, 4],
        };
        assert_eq!(
            decode_const(&t),
            Ok(Value::Seq(vec![
                Value::Int(1),
                Value::Int(2),
                Value::Int(3),
                Value::Int(4)
            ]))
        );
    }

    #[test]
    fn canonicalizes_nan_in_inputs_and_bytes() {
        let f32_t = Type::Float(32);
        let noncanon = Value::Float(0x7FC0_1234);
        assert_eq!(
            validate_and_canonicalize(&f32_t, &noncanon),
            Ok(Value::Float(0x7FC0_0000))
        );
        assert_eq!(
            value_to_bytes(&f32_t, &noncanon),
            Ok(0x7FC0_0000u32.to_le_bytes().to_vec())
        );
    }

    #[test]
    fn eq_vs_same() {
        let f32_t = Type::Float(32);
        let nan = Value::Float(0x7FC0_0000);
        let pz = Value::Float(0);
        let nz = Value::Float(0x8000_0000);
        assert_eq!(val_eq(&f32_t, &nan, &nan), Ok(false));
        assert_eq!(val_same(&f32_t, &nan, &nan), Ok(true));
        assert_eq!(val_eq(&f32_t, &pz, &nz), Ok(true));
        assert_eq!(val_same(&f32_t, &pz, &nz), Ok(false));
    }
}
