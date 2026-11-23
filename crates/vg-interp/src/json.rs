//! Minimal RFC 8259 JSON parser and docs/15 typed Value decoder.

use vg_core::float;
use vg_core::types::Type;
use vg_core::value::{Value, int_fits};

use crate::float_ops;

/// A parsed JSON value preserving object key order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

pub(crate) fn parse_json(src: &str) -> Result<Json, String> {
    let mut p = Parser {
        b: src.as_bytes(),
        at: 0,
    };
    p.ws();
    let val = p.value()?;
    p.ws();
    if p.at != p.b.len() {
        return Err(format!("trailing JSON input at byte {}", p.at));
    }
    Ok(val)
}

/// Decodes a `Value` from its docs/15 JSON encoding at type `ty`.
pub fn value_from_json(json: &str, ty: &Type) -> Result<Value, String> {
    let j = parse_json(json)?;
    value_from_json_val(&j, ty)
}

pub(crate) fn value_from_json_val(j: &Json, ty: &Type) -> Result<Value, String> {
    match ty {
        Type::Bool => match j {
            Json::Bool(b) => Ok(Value::Bool(*b)),
            _ => Err(format!("expected JSON boolean for {ty}, got {j:?}")),
        },
        Type::UInt(w) | Type::SInt(w) if *w < 64 => {
            let Json::Num(s) = j else {
                return Err(format!("expected JSON integer for {ty}, got {j:?}"));
            };
            if s.contains(['.', 'e', 'E']) {
                return Err(format!(
                    "expected integer without exponent for {ty}, got {s}"
                ));
            }
            let v: i128 = s
                .parse()
                .map_err(|_| format!("invalid integer `{s}` for {ty}"))?;
            if !int_fits(v, ty) {
                return Err(format!("integer {v} does not fit {ty}"));
            }
            Ok(Value::Int(v))
        }
        Type::UInt(64) | Type::SInt(64) => {
            let s = match j {
                Json::Str(s) => s.as_str(),
                // Also tolerate raw JSON numbers if small enough, though docs/15 writes strings.
                Json::Num(s) if !s.contains(['.', 'e', 'E']) => s.as_str(),
                _ => return Err(format!("expected decimal string for {ty}, got {j:?}")),
            };
            let v: i128 = s
                .parse()
                .map_err(|_| format!("invalid 64-bit integer `{s}` for {ty}"))?;
            if !int_fits(v, ty) {
                return Err(format!("integer {v} does not fit {ty}"));
            }
            Ok(Value::Int(v))
        }
        t if t.is_float() => {
            let Json::Str(s) = j else {
                return Err(format!("expected float string for {t}, got {j:?}"));
            };
            let bits = parse_float_str(s, t)?;
            Ok(Value::Float(bits))
        }
        Type::Array(elem, n) => {
            let Json::Arr(xs) = j else {
                return Err(format!("expected JSON array for {ty}, got {j:?}"));
            };
            if xs.len() != *n as usize {
                return Err(format!(
                    "expected array of length {n} for {ty}, got {}",
                    xs.len()
                ));
            }
            let out = xs
                .iter()
                .map(|x| value_from_json_val(x, elem))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Seq(out))
        }
        Type::Slice(elem) => {
            let Json::Arr(xs) = j else {
                return Err(format!("expected JSON array for {ty}, got {j:?}"));
            };
            let out = xs
                .iter()
                .map(|x| value_from_json_val(x, elem))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Seq(out))
        }
        Type::Tensor(elem, dims) => {
            let mut flat = Vec::new();
            parse_tensor(j, elem, dims, &mut flat)?;
            Ok(Value::Seq(flat))
        }
        Type::Tuple(ts) => {
            let Json::Arr(xs) = j else {
                return Err(format!("expected JSON array for {ty}, got {j:?}"));
            };
            if xs.len() != ts.len() {
                return Err(format!(
                    "expected tuple of length {}, got {}",
                    ts.len(),
                    xs.len()
                ));
            }
            let out = ts
                .iter()
                .zip(xs)
                .map(|(t, x)| value_from_json_val(x, t))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Tuple(out))
        }
        Type::Result(ok_t, err_t) => {
            let Json::Obj(fields) = j else {
                return Err(format!("expected JSON object for {ty}, got {j:?}"));
            };
            if fields.len() != 1 {
                return Err(format!("expected single-key Result object, got {j:?}"));
            }
            match fields[0].0.as_str() {
                "ok" => Ok(Value::Ok(Box::new(value_from_json_val(
                    &fields[0].1,
                    ok_t,
                )?))),
                "err" => Ok(Value::Err(Box::new(value_from_json_val(
                    &fields[0].1,
                    err_t,
                )?))),
                k => Err(format!("expected `ok` or `err` key, found `{k}`")),
            }
        }
        Type::Handle(expected) => {
            let Json::Obj(fields) = j else {
                return Err(format!("expected Handle JSON object, got {j:?}"));
            };
            let kind = fields
                .iter()
                .find(|(k, _)| k == "handle")
                .and_then(|(_, v)| match v {
                    Json::Str(s) => Some(s.as_str()),
                    _ => None,
                })
                .ok_or_else(|| format!("missing string `handle` field in {j:?}"))?;
            if kind != expected.name() {
                return Err(format!(
                    "expected handle kind `{}`, found `{kind}`",
                    expected.name()
                ));
            }
            let id = fields
                .iter()
                .find(|(k, _)| k == "id")
                .and_then(|(_, v)| match v {
                    Json::Num(s) if !s.contains(['.', 'e', 'E']) => s.parse::<u32>().ok(),
                    _ => None,
                })
                .ok_or_else(|| format!("missing u32 `id` field in {j:?}"))?;
            Ok(Value::Handle(id))
        }
        _ => Err(format!("type {ty} has no JSON value encoding")),
    }
}

fn parse_tensor(j: &Json, elem: &Type, dims: &[u32], out: &mut Vec<Value>) -> Result<(), String> {
    match dims {
        [] => out.push(value_from_json_val(j, elem)?),
        [d] => {
            let Json::Arr(xs) = j else {
                return Err(format!("expected JSON array of length {d}, got {j:?}"));
            };
            if xs.len() != *d as usize {
                return Err(format!("expected dimension {d}, got {}", xs.len()));
            }
            for x in xs {
                out.push(value_from_json_val(x, elem)?);
            }
        }
        [d, rest @ ..] => {
            let Json::Arr(rows) = j else {
                return Err(format!(
                    "expected nested JSON array of length {d}, got {j:?}"
                ));
            };
            if rows.len() != *d as usize {
                return Err(format!("expected dimension {d}, got {}", rows.len()));
            }
            for row in rows {
                parse_tensor(row, elem, rest, out)?;
            }
        }
    }
    Ok(())
}

fn parse_float_str(s: &str, ty: &Type) -> Result<u64, String> {
    match s {
        "nan" => Ok(float::canonical_nan(ty)),
        "inf" | "+inf" => Ok(float_ops::pos_inf(ty)),
        "-inf" => Ok(float_ops::neg_inf(ty)),
        _ => {
            let (neg, rest) = match s.strip_prefix('-') {
                Some(r) => (true, r),
                None => (false, s.strip_prefix('+').unwrap_or(s)),
            };
            if rest.is_empty() || !rest.bytes().next().is_some_and(|c| c.is_ascii_digit()) {
                return Err(format!("invalid float literal `{s}` for {ty}"));
            }
            // Validate decimal grammar before vg_core::float::parse so malformed strings fail cleanly.
            let (mant, exp) = match rest.find(['e', 'E']) {
                Some(i) => (&rest[..i], Some(&rest[i + 1..])),
                None => (rest, None),
            };
            let valid_mant = match mant.split_once('.') {
                Some((a, b)) => {
                    !a.is_empty()
                        && a.bytes().all(|c| c.is_ascii_digit())
                        && !b.is_empty()
                        && b.bytes().all(|c| c.is_ascii_digit())
                }
                None => !mant.is_empty() && mant.bytes().all(|c| c.is_ascii_digit()),
            };
            let valid_exp = match exp {
                Some(e) => {
                    let e = e.strip_prefix(['+', '-']).unwrap_or(e);
                    !e.is_empty() && e.bytes().all(|c| c.is_ascii_digit())
                }
                None => true,
            };
            if !valid_mant || !valid_exp {
                return Err(format!("invalid float literal `{s}` for {ty}"));
            }
            float::parse(rest, neg, ty).ok_or_else(|| format!("float literal `{s}` overflows {ty}"))
        }
    }
}

struct Parser<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.at < self.b.len() && self.b[self.at].is_ascii_whitespace() {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.at).copied()
    }

    fn next(&mut self) -> Result<u8, String> {
        let ch = self
            .peek()
            .ok_or_else(|| format!("unexpected end of JSON at byte {}", self.at))?;
        self.at += 1;
        Ok(ch)
    }

    fn expect(&mut self, lit: &[u8]) -> Result<(), String> {
        if self.b[self.at..].starts_with(lit) {
            self.at += lit.len();
            Ok(())
        } else {
            Err(format!("expected {:?} at byte {}", lit, self.at))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.peek() {
            Some(b'n') => {
                self.expect(b"null")?;
                Ok(Json::Null)
            }
            Some(b't') => {
                self.expect(b"true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.expect(b"false")?;
                Ok(Json::Bool(false))
            }
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!(
                "unexpected byte '{}' at JSON offset {}",
                c as char, self.at
            )),
            None => Err("unexpected end of JSON input".to_string()),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.next()? != b'"' {
            return Err("expected '\"'".to_string());
        }
        let mut out = String::new();
        loop {
            let ch = self.next()?;
            match ch {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = self.next()?;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\x08'),
                        b'f' => out.push('\x0c'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let code = self.hex4()?;
                            if (0xD800..=0xDBFF).contains(&code) {
                                self.expect(b"\\u")?;
                                let low = self.hex4()?;
                                if !(0xDC00..=0xDFFF).contains(&low) {
                                    return Err("invalid UTF-16 surrogate pair in JSON".to_string());
                                }
                                let cp = 0x10000
                                    + (((u32::from(code) - 0xD800) << 10)
                                        | (u32::from(low) - 0xDC00));
                                let c = char::from_u32(cp)
                                    .ok_or_else(|| "invalid Unicode scalar in JSON".to_string())?;
                                out.push(c);
                            } else {
                                let c = char::from_u32(u32::from(code))
                                    .ok_or_else(|| "invalid Unicode scalar in JSON".to_string())?;
                                out.push(c);
                            }
                        }
                        other => return Err(format!("invalid JSON escape \\{}", other as char)),
                    }
                }
                b if b < 0x20 => return Err("unescaped control char in JSON string".to_string()),
                b if b < 0x80 => out.push(b as char),
                _ => {
                    self.at -= 1;
                    let s = std::str::from_utf8(&self.b[self.at..])
                        .map_err(|_| "invalid UTF-8 in JSON string".to_string())?;
                    let c = s
                        .chars()
                        .next()
                        .ok_or_else(|| "unexpected end in UTF-8".to_string())?;
                    self.at += c.len_utf8();
                    out.push(c);
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u16, String> {
        let mut val = 0u16;
        for _ in 0..4 {
            let c = self.next()?;
            let d = match c {
                b'0'..=b'9' => u16::from(c - b'0'),
                b'a'..=b'f' => u16::from(c - b'a' + 10),
                b'A'..=b'F' => u16::from(c - b'A' + 10),
                _ => return Err("invalid \\uXXXX hex digit in JSON".to_string()),
            };
            val = (val << 4) | d;
        }
        Ok(val)
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(c) if (b'1'..=b'9').contains(&c) => {
                self.at += 1;
                while self.peek().is_some_and(|d| d.is_ascii_digit()) {
                    self.at += 1;
                }
            }
            _ => return Err(format!("invalid JSON number at byte {start}")),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if !self.peek().is_some_and(|d| d.is_ascii_digit()) {
                return Err(format!("invalid JSON fraction at byte {}", self.at));
            }
            while self.peek().is_some_and(|d| d.is_ascii_digit()) {
                self.at += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !self.peek().is_some_and(|d| d.is_ascii_digit()) {
                return Err(format!("invalid JSON exponent at byte {}", self.at));
            }
            while self.peek().is_some_and(|d| d.is_ascii_digit()) {
                self.at += 1;
            }
        }
        let s = std::str::from_utf8(&self.b[start..self.at])
            .expect("ASCII number bytes")
            .to_string();
        Ok(Json::Num(s))
    }

    fn array(&mut self) -> Result<Json, String> {
        self.next()?;
        self.ws();
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            match self.next()? {
                b',' => {}
                b']' => return Ok(Json::Arr(items)),
                c => {
                    return Err(format!(
                        "expected ',' or ']' in array, found '{}'",
                        c as char
                    ));
                }
            }
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.next()?;
        self.ws();
        let mut fields = Vec::new();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Json::Obj(fields));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.ws();
            if self.next()? != b':' {
                return Err("expected ':' in JSON object".to_string());
            }
            let v = self.value()?;
            fields.push((k, v));
            self.ws();
            match self.next()? {
                b',' => {}
                b'}' => return Ok(Json::Obj(fields)),
                c => {
                    return Err(format!(
                        "expected ',' or '}}' in object, found '{}'",
                        c as char
                    ));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vg_core::types::Handle;

    #[test]
    fn json_round_trip_all_types() {
        let cases: Vec<(Value, Type)> = vec![
            (Value::Bool(true), Type::Bool),
            (Value::Bool(false), Type::Bool),
            (Value::Int(0), Type::UInt(8)),
            (Value::Int(255), Type::UInt(8)),
            (Value::Int(-128), Type::SInt(8)),
            (Value::Int(i128::from(i32::MIN)), Type::SInt(32)),
            (Value::Int(i128::from(u32::MAX)), Type::UInt(32)),
            (Value::Int(i128::from(u64::MAX)), Type::UInt(64)),
            (Value::Int(i128::from(i64::MIN)), Type::SInt(64)),
            (Value::Float(0.1f32.to_bits().into()), Type::Float(32)),
            (Value::Float(3e38f32.to_bits().into()), Type::Float(32)),
            (Value::Float(0x8000_0000), Type::Float(32)),
            (Value::Float(0x0000_0001), Type::Float(32)),
            (Value::Float(0x7F80_0000), Type::Float(32)),
            (Value::Float(0xFF80_0000), Type::Float(32)),
            (Value::Float(0x7FC0_0000), Type::Float(32)),
            (Value::Float((-0.0f64).to_bits()), Type::Float(64)),
            (Value::Float(1u64), Type::Float(64)),
            (Value::Float(f64::MAX.to_bits()), Type::Float(64)),
            (Value::Float(f64::INFINITY.to_bits()), Type::Float(64)),
            (Value::Float(f64::NEG_INFINITY.to_bits()), Type::Float(64)),
            (Value::Float(0x7FF8_0000_0000_0000), Type::Float(64)),
            (
                Value::Seq((0..6).map(Value::Int).collect()),
                Type::Tensor(Box::new(Type::UInt(8)), vec![2, 3]),
            ),
            (
                Value::Seq(vec![Value::Int(1), Value::Int(2)]),
                Type::Array(Box::new(Type::SInt(16)), 2),
            ),
            (Value::Seq(vec![]), Type::Slice(Box::new(Type::UInt(8)))),
            (
                Value::Tuple(vec![Value::Bool(false), Value::Int(7)]),
                Type::Tuple(vec![Type::Bool, Type::SInt(32)]),
            ),
            (
                Value::Ok(Box::new(Value::Seq(vec![Value::Int(104), Value::Int(105)]))),
                Type::Result(
                    Box::new(Type::Slice(Box::new(Type::UInt(8)))),
                    Box::new(Type::UInt(32)),
                ),
            ),
            (
                Value::Err(Box::new(Value::Int(2))),
                Type::Result(Box::new(Type::Bool), Box::new(Type::UInt(16))),
            ),
            (Value::Handle(3), Type::Handle(Handle::Clock)),
        ];
        for (v, t) in cases {
            let s = v.to_json(&t).expect("to_json");
            let back = value_from_json(&s, &t).unwrap_or_else(|e| panic!("{s} at {t}: {e}"));
            assert_eq!(back, v, "round trip {s} at {t}");
        }
    }

    #[test]
    fn exhaustive_f16_bf16_json_round_trip() {
        for ty in [Type::Float(16), Type::BFloat16] {
            for bits in 0u64..=0xFFFF {
                let canon = float_ops::canonicalize(&ty, bits);
                let v = Value::Float(canon);
                let s = v.to_json(&ty).expect("to_json");
                let back = value_from_json(&s, &ty)
                    .unwrap_or_else(|e| panic!("{ty} {bits:#06x} -> {s}: {e}"));
                assert_eq!(back, v, "{ty} {bits:#06x} -> {s}");
            }
        }
    }

    #[test]
    fn json_parser_rejects_malformed() {
        assert!(value_from_json("true false", &Type::Bool).is_err());
        assert!(value_from_json("1.0", &Type::SInt(32)).is_err());
        assert!(value_from_json("256", &Type::UInt(8)).is_err());
        assert!(value_from_json("\"bad\"", &Type::Float(32)).is_err());
    }
}
