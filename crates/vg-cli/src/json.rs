//! docs/15 JSON values in: the inverse of `Value::to_json`, for `vg run` inputs and MCP calls.

use serde_json::Value as J;
use vg_core::float;
use vg_core::types::{Handle, Type};
use vg_core::value::{Value, int_fits};

/// Reads a docs/15 JSON value of type `t`. Integers may be JSON numbers or decimal strings.
pub fn value(j: &J, t: &Type) -> Result<Value, String> {
    let bad = || format!("{j} is not a value of type {t}");
    Ok(match t {
        Type::Bool => Value::Bool(j.as_bool().ok_or_else(bad)?),
        Type::UInt(_) | Type::SInt(_) => {
            let v: i128 = match j {
                J::Number(n) => n.as_i64().map(i128::from).or(n.as_u64().map(i128::from)),
                J::String(s) => s.parse().ok(),
                _ => None,
            }
            .ok_or_else(bad)?;
            if !int_fits(v, t) {
                return Err(format!("{v} is out of range for {t}"));
            }
            Value::Int(v)
        }
        Type::Float(_) | Type::BFloat16 => {
            let s = j
                .as_str()
                .ok_or_else(|| format!("{j}: a float is a string such as \"0.5\""))?;
            let (neg, body) = s.strip_prefix('-').map_or((false, s), |b| (true, b));
            let sign = |bits: u64| {
                if neg {
                    bits | 1 << (8 * t.byte_size().unwrap_or(8) - 1)
                } else {
                    bits
                }
            };
            Value::Float(match body {
                "nan" if !neg => float::canonical_nan(t),
                "inf" => sign(float::canonical_nan(t) & !(1u64 << (float_frac_bits(t) - 1))),
                _ if body.bytes().next().is_some_and(|c| c.is_ascii_digit()) => {
                    float::parse(body, neg, t).ok_or_else(|| format!("{s} overflows {t}"))?
                }
                _ => return Err(bad()),
            })
        }
        Type::Array(e, n) => {
            let xs = j
                .as_array()
                .filter(|xs| xs.len() == *n as usize)
                .ok_or_else(bad)?;
            Value::Seq(xs.iter().map(|x| value(x, e)).collect::<Result<_, _>>()?)
        }
        Type::Slice(e) => {
            let xs = j.as_array().ok_or_else(bad)?;
            Value::Seq(xs.iter().map(|x| value(x, e)).collect::<Result<_, _>>()?)
        }
        Type::Tensor(e, dims) => {
            let mut flat = Vec::new();
            tensor(j, e, dims, &mut flat)?;
            Value::Seq(flat)
        }
        Type::Tuple(ts) => {
            let xs = j
                .as_array()
                .filter(|xs| xs.len() == ts.len())
                .ok_or_else(bad)?;
            Value::Tuple(
                xs.iter()
                    .zip(ts)
                    .map(|(x, t)| value(x, t))
                    .collect::<Result<_, _>>()?,
            )
        }
        Type::Result(ok, err) => {
            match j.as_object().map(|o| (o.len(), o.get("ok"), o.get("err"))) {
                Some((1, Some(v), None)) => Value::Ok(Box::new(value(v, ok)?)),
                Some((1, None, Some(e))) => Value::Err(Box::new(value(e, err)?)),
                _ => return Err(bad()),
            }
        }
        Type::Handle(h) => {
            let o = j.as_object().ok_or_else(bad)?;
            let kind = o.get("handle").and_then(J::as_str);
            let id = o
                .get("id")
                .and_then(J::as_u64)
                .and_then(|i| u32::try_from(i).ok());
            match (kind, id) {
                (Some(k), Some(id)) if Handle::ALL.iter().any(|x| x == h && x.name() == k) => {
                    Value::Handle(id)
                }
                _ => return Err(bad()),
            }
        }
        Type::Eff => Value::Eff,
        Type::Int | Type::Arena(_) => return Err(format!("no value of type {t} can be passed in")),
    })
}

/// Fraction bits of a float type.
fn float_frac_bits(t: &Type) -> u32 {
    match t {
        Type::Float(16) => 10,
        Type::BFloat16 => 7,
        Type::Float(32) => 23,
        _ => 52,
    }
}

fn tensor(j: &J, e: &Type, dims: &[u32], out: &mut Vec<Value>) -> Result<(), String> {
    match dims.split_first() {
        None => out.push(value(j, e)?),
        Some((d, rest)) => {
            let xs = j
                .as_array()
                .filter(|xs| xs.len() == *d as usize)
                .ok_or_else(|| format!("{j} does not have {d} rows"))?;
            for x in xs {
                tensor(x, e, rest, out)?;
            }
        }
    }
    Ok(())
}

/// A value as JSON, for building tool results.
pub fn out(v: &Value, t: &Type) -> J {
    v.to_json(t)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(J::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_back_what_to_json_writes() {
        let f32 = Type::Float(32);
        let tensor = Type::Tensor(Box::new(Type::UInt(8)), vec![2, 3]);
        let res = Type::Result(Box::new(Type::Bool), Box::new(Type::SInt(16)));
        let cases: Vec<(Value, Type)> = vec![
            (Value::Bool(true), Type::Bool),
            (Value::Int(-5), Type::SInt(8)),
            (Value::Int(u64::MAX.into()), Type::UInt(64)),
            (Value::Float(0.1f32.to_bits().into()), f32.clone()),
            (Value::Float(0x8000_0000), f32.clone()),
            (Value::Float(0x7F80_0000), f32.clone()),
            (Value::Float(0xFF80_0000), f32.clone()),
            (Value::Float(0x7FC0_0000), f32.clone()),
            (Value::Float(0xFC00), Type::Float(16)),
            (Value::Float(0x0001), Type::Float(16)),
            (Value::Float(f64::MIN_POSITIVE.to_bits()), Type::Float(64)),
            (Value::Seq((0..6).map(Value::Int).collect()), tensor),
            (Value::Err(Box::new(Value::Int(-3))), res),
            (Value::Handle(4), Type::Handle(Handle::Clock)),
        ];
        for (v, t) in cases {
            let j = out(&v, &t);
            assert_eq!(value(&j, &t), Ok(v.clone()), "{j} at {t}");
        }
    }

    #[test]
    fn rejects_values_of_the_wrong_type() {
        let parse = |s: &str| serde_json::from_str::<J>(s).unwrap();
        assert!(value(&parse("256"), &Type::UInt(8)).is_err());
        assert!(
            value(&parse("0.5"), &Type::Float(32)).is_err(),
            "floats are strings"
        );
        assert!(
            value(&parse("\"3.5e38\""), &Type::Float(32)).is_err(),
            "overflow"
        );
        assert!(value(&parse("\"-nan\""), &Type::Float(32)).is_err());
        assert!(value(&parse("[1, 2]"), &Type::Array(Box::new(Type::UInt(8)), 3)).is_err());
        assert!(
            value(
                &parse("{\"ok\": true, \"err\": 1}"),
                &Type::Result(Box::new(Type::Bool), Box::new(Type::UInt(8)))
            )
            .is_err()
        );
        assert_eq!(value(&parse("\"7\""), &Type::SInt(32)), Ok(Value::Int(7)));
    }
}
