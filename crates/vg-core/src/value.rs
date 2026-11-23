//! Runtime values (docs/13, "Values") and their JSON encoding (docs/15, "Values in JSON"). A value
//! carries no type: the node's types say how to read it. vg-interp, vg-smt, vg-codegen and vg-cli
//! all exchange inputs, outputs and counterexamples as these values.

use crate::float;
use crate::types::Type;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Value {
    Bool(bool),
    /// A uN or iN value, exactly.
    Int(i128),
    /// The IEEE bits of an f16, bf16, f32 or f64, right-aligned; NaN only as the canonical NaN.
    Float(u64),
    /// An Array, Slice or Tensor; a tensor's elements row-major and flat.
    Seq(Vec<Value>),
    Tuple(Vec<Value>),
    Ok(Box<Value>),
    Err(Box<Value>),
    /// The Eff token. It has no JSON form: callers leave Eff results out of what they print.
    Eff,
    /// A capability, by the id the manifest or the effect trace issued.
    Handle(u32),
}

/// True when integer v is a value of integer type t.
pub fn int_fits(v: i128, t: &Type) -> bool {
    match t {
        Type::UInt(w) => v >= 0 && v < 1i128 << w,
        Type::SInt(w) => v >= -(1i128 << (w - 1)) && v < 1i128 << (w - 1),
        _ => false,
    }
}

impl Value {
    /// The docs/15 JSON text of this value at type `ty`, or None when the value does not have that
    /// type (or is Eff).
    pub fn to_json(&self, ty: &Type) -> Option<String> {
        Some(match (self, ty) {
            (Value::Bool(b), Type::Bool) => b.to_string(),
            (Value::Int(v), Type::UInt(64) | Type::SInt(64)) if int_fits(*v, ty) => {
                format!("\"{v}\"")
            }
            (Value::Int(v), Type::UInt(_) | Type::SInt(_)) if int_fits(*v, ty) => v.to_string(),
            (Value::Float(bits), t)
                if t.is_float() && (t == &Type::Float(64) || bits >> (8 * t.byte_size()?) == 0) =>
            {
                let neg = float::to_f64(t, *bits).is_sign_negative();
                let text = if float::is_nan(t, *bits) {
                    "nan".to_string()
                } else if float::is_inf(t, *bits) {
                    if neg { "-inf" } else { "inf" }.to_string()
                } else {
                    let (neg, digits) = float::shortest(t, *bits)?;
                    if neg { format!("-{digits}") } else { digits }
                };
                format!("\"{text}\"")
            }
            (Value::Seq(xs), Type::Array(t, n)) if xs.len() == *n as usize => list(xs, t)?,
            (Value::Seq(xs), Type::Slice(t)) => list(xs, t)?,
            (Value::Seq(xs), Type::Tensor(t, dims)) => {
                let count = dims
                    .iter()
                    .try_fold(1usize, |a, d| a.checked_mul(*d as usize))?;
                if xs.len() != count {
                    return None;
                }
                tensor(xs, t, dims)?
            }
            (Value::Tuple(xs), Type::Tuple(ts)) if xs.len() == ts.len() => {
                let parts: Option<Vec<_>> = xs.iter().zip(ts).map(|(x, t)| x.to_json(t)).collect();
                format!("[{}]", parts?.join(", "))
            }
            (Value::Ok(v), Type::Result(t, _)) => format!("{{\"ok\": {}}}", v.to_json(t)?),
            (Value::Err(e), Type::Result(_, t)) => format!("{{\"err\": {}}}", e.to_json(t)?),
            (Value::Handle(id), Type::Handle(h)) => {
                format!("{{\"handle\": \"{}\", \"id\": {id}}}", h.name())
            }
            _ => return None,
        })
    }
}

fn list(xs: &[Value], t: &Type) -> Option<String> {
    let parts: Option<Vec<_>> = xs.iter().map(|x| x.to_json(t)).collect();
    Some(format!("[{}]", parts?.join(", ")))
}

/// Row-major nesting: the first dimension is the outermost JSON array.
fn tensor(xs: &[Value], t: &Type, dims: &[u32]) -> Option<String> {
    match dims {
        [] => xs.first()?.to_json(t),
        [_] => list(xs, t),
        [d, rest @ ..] => {
            let step = xs.len() / (*d as usize).max(1);
            let rows: Option<Vec<_>> = (0..*d as usize)
                .map(|i| tensor(&xs[i * step..(i + 1) * step], t, rest))
                .collect();
            Some(format!("[{}]", rows?.join(", ")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Handle;

    #[test]
    fn json_matches_docs_15() {
        let f32 = Type::Float(32);
        let cases: Vec<(Value, Type, &str)> = vec![
            (Value::Bool(true), Type::Bool, "true"),
            (Value::Int(255), Type::UInt(8), "255"),
            (Value::Int(-128), Type::SInt(8), "-128"),
            (
                Value::Int(u64::MAX.into()),
                Type::UInt(64),
                "\"18446744073709551615\"",
            ),
            (
                Value::Int(i64::MIN.into()),
                Type::SInt(64),
                "\"-9223372036854775808\"",
            ),
            (
                Value::Float(0.1f32.to_bits().into()),
                f32.clone(),
                "\"0.1\"",
            ),
            (
                Value::Float(3e38f32.to_bits().into()),
                f32.clone(),
                "\"3e38\"",
            ),
            (Value::Float(0x8000_0000), f32.clone(), "\"-0.0\""),
            (Value::Float(0xFF80_0000), f32.clone(), "\"-inf\""),
            (Value::Float(0x7FC0_0000), f32.clone(), "\"nan\""),
            (Value::Float(0x3C00), Type::Float(16), "\"1.0\""),
            (Value::Float(1.5f64.to_bits()), Type::Float(64), "\"1.5\""),
            (
                Value::Seq((0..6).map(Value::Int).collect()),
                Type::Tensor(Box::new(Type::UInt(8)), vec![2, 3]),
                "[[0, 1, 2], [3, 4, 5]]",
            ),
            (
                Value::Tuple(vec![Value::Bool(false), Value::Int(7)]),
                Type::Tuple(vec![Type::Bool, Type::SInt(32)]),
                "[false, 7]",
            ),
            (
                Value::Err(Box::new(Value::Int(2))),
                Type::Result(Box::new(Type::Bool), Box::new(Type::UInt(16))),
                "{\"err\": 2}",
            ),
            (
                Value::Handle(0),
                Type::Handle(Handle::FileIn),
                "{\"handle\": \"FileIn\", \"id\": 0}",
            ),
        ];
        for (v, t, want) in cases {
            assert_eq!(v.to_json(&t).as_deref(), Some(want), "{v:?} at {t}");
        }
    }

    #[test]
    fn ill_typed_values_have_no_json() {
        assert_eq!(Value::Int(256).to_json(&Type::UInt(8)), None);
        assert_eq!(Value::Int(-1).to_json(&Type::UInt(32)), None);
        assert_eq!(Value::Float(0x1_0000).to_json(&Type::Float(16)), None);
        assert_eq!(Value::Bool(true).to_json(&Type::SInt(8)), None);
        assert_eq!(Value::Eff.to_json(&Type::Eff), None);
        let arr = Type::Array(Box::new(Type::Bool), 2);
        assert_eq!(Value::Seq(vec![Value::Bool(true)]).to_json(&arr), None);
    }
}
