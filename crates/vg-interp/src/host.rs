//! Trap error, Host capability interface, and deterministic effect trace hosts (docs/13, docs/16).

use std::fmt;

use vg_core::ir::{Hash, Node};
use vg_core::types::Type;
use vg_core::value::Value;

use crate::json::{Json, parse_json, value_from_json_val};

/// Runtime domain violation ("trap") in an unverified node or concrete formula check (docs/13).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trap {
    /// Index in `node.ops`, or `None` for formula, host-only, or entry errors.
    pub op: Option<u32>,
    /// Opcode name (e.g. `"DIV"`, `"INDEX"`), when the trap came from an op.
    pub opcode: Option<&'static str>,
    /// Human-readable description naming the op/obligation and the offending input values.
    pub message: String,
}

impl Trap {
    pub fn entry(message: impl Into<String>) -> Self {
        Self {
            op: None,
            opcode: None,
            message: message.into(),
        }
    }

    pub fn formula(message: impl Into<String>) -> Self {
        Self {
            op: None,
            opcode: None,
            message: message.into(),
        }
    }

    pub fn op(index: u32, opcode: &'static str, message: impl Into<String>) -> Self {
        Self {
            op: Some(index),
            opcode: Some(opcode),
            message: message.into(),
        }
    }

    pub(crate) fn with_op(mut self, index: u32, opcode: &'static str) -> Self {
        if self.op.is_none() {
            self.op = Some(index);
        }
        if self.opcode.is_none() {
            self.opcode = Some(opcode);
        }
        self
    }
}

impl fmt::Display for Trap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.op, self.opcode) {
            (Some(i), Some(name)) => write!(f, "trap at op #{i} ({name}): {}", self.message),
            (None, Some(name)) => write!(f, "trap in {name}: {}", self.message),
            _ => write!(f, "trap: {}", self.message),
        }
    }
}

impl std::error::Error for Trap {}

/// Environment interface for MVP effect ops (`FILE_READ`, `FILE_WRITE`, `CLOCK_READ`).
pub trait Host {
    /// Performs `FILE_READ(e, handle, max)`: returns `Ok(bytes)` with `bytes.len() <= max`,
    /// or `Err(1)` on I/O error / `Err(2)` if the file exceeds `max` bytes (docs/14).
    fn file_read(&mut self, handle: u32, max: u64) -> Result<Result<Vec<u8>, u32>, Trap>;

    /// Performs `FILE_WRITE(e, handle, data)`: appends all bytes and returns `Ok(data.len())`,
    /// or `Err(1)` on I/O error (docs/14).
    fn file_write(&mut self, handle: u32, data: &[u8]) -> Result<Result<u64, u32>, Trap>;

    /// Performs `CLOCK_READ(e, handle)`: returns monotonic nanoseconds (docs/14).
    fn clock_read(&mut self, handle: u32) -> Result<u64, Trap>;

    /// Optional frozen learnable parameters θ for a callee invoked via `CALL_HASH` or `ASSERT_GUARD`.
    fn theta(&mut self, _callee: &Hash, _node: &Node) -> Option<Vec<Value>> {
        None
    }
}

/// A host that rejects every effect op with a [`Trap`].
#[derive(Clone, Copy, Debug, Default)]
pub struct NoEffectsHost;

impl Host for NoEffectsHost {
    fn file_read(&mut self, handle: u32, max: u64) -> Result<Result<Vec<u8>, u32>, Trap> {
        Err(Trap {
            op: None,
            opcode: Some("FILE_READ"),
            message: format!("FILE_READ(handle={handle}, max={max}): host forbids effects"),
        })
    }

    fn file_write(&mut self, handle: u32, data: &[u8]) -> Result<Result<u64, u32>, Trap> {
        Err(Trap {
            op: None,
            opcode: Some("FILE_WRITE"),
            message: format!(
                "FILE_WRITE(handle={handle}, len={}): host forbids effects",
                data.len()
            ),
        })
    }

    fn clock_read(&mut self, handle: u32) -> Result<u64, Trap> {
        Err(Trap {
            op: None,
            opcode: Some("CLOCK_READ"),
            message: format!("CLOCK_READ(handle={handle}): host forbids effects"),
        })
    }
}

/// One step in a docs/16 effect trace: `{"op": "FILE_READ", "result": {"ok": [104, 105]}}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TraceEntry {
    pub op: String,
    pub result: Value,
}

impl TraceEntry {
    pub fn result_type(&self) -> Option<Type> {
        match self.op.as_str() {
            "FILE_READ" => Some(Type::Result(
                Box::new(Type::Slice(Box::new(Type::UInt(8)))),
                Box::new(Type::UInt(32)),
            )),
            "FILE_WRITE" => Some(Type::Result(
                Box::new(Type::UInt(64)),
                Box::new(Type::UInt(32)),
            )),
            "CLOCK_READ" => Some(Type::UInt(64)),
            _ => None,
        }
    }

    /// Encodes this trace entry as docs/16 JSON.
    pub fn to_json(&self) -> Option<String> {
        let ty = self.result_type()?;
        let res_json = self.result.to_json(&ty)?;
        Some(format!(
            "{{\"op\": \"{}\", \"result\": {res_json}}}",
            escape_json(&self.op)
        ))
    }
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Serializes a slice of trace entries as a docs/16 JSON array.
pub fn trace_to_json(entries: &[TraceEntry]) -> Option<String> {
    let parts: Option<Vec<String>> = entries.iter().map(TraceEntry::to_json).collect();
    Some(format!("[{}]", parts?.join(", ")))
}

/// Parses a docs/16 JSON effect trace array into [`TraceEntry`] items.
pub fn parse_trace_json(json: &str) -> Result<Vec<TraceEntry>, String> {
    let j = parse_json(json)?;
    let Json::Arr(items) = j else {
        return Err("effect trace must be a JSON array".to_string());
    };
    let mut out = Vec::with_capacity(items.len());
    for (idx, item) in items.into_iter().enumerate() {
        let Json::Obj(fields) = item else {
            return Err(format!("trace entry {idx} must be a JSON object"));
        };
        let op = fields
            .iter()
            .find(|(k, _)| k == "op")
            .and_then(|(_, v)| match v {
                Json::Str(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| format!("trace entry {idx} missing string field `op`"))?;
        let res_json = fields
            .iter()
            .find(|(k, _)| k == "result")
            .map(|(_, v)| v)
            .ok_or_else(|| format!("trace entry {idx} missing field `result`"))?;
        let tmp = TraceEntry {
            op: op.clone(),
            result: Value::Eff,
        };
        let ty = tmp
            .result_type()
            .ok_or_else(|| format!("trace entry {idx}: unsupported effect op `{op}`"))?;
        let result = value_from_json_val(res_json, &ty)
            .map_err(|e| format!("trace entry {idx} ({op}): {e}"))?;
        out.push(TraceEntry { op, result });
    }
    Ok(out)
}

/// A deterministic [`Host`] that replays a scripted docs/16 trace and records executed steps.
#[derive(Clone, Debug)]
pub struct TraceHost {
    script: Vec<TraceEntry>,
    cursor: usize,
    recorded: Vec<TraceEntry>,
}

impl TraceHost {
    pub fn new(script: Vec<TraceEntry>) -> Self {
        Self {
            script,
            cursor: 0,
            recorded: Vec::new(),
        }
    }

    pub fn from_json(json: &str) -> Result<Self, String> {
        Ok(Self::new(parse_trace_json(json)?))
    }

    pub fn recorded(&self) -> &[TraceEntry] {
        &self.recorded
    }

    pub fn recorded_json(&self) -> String {
        trace_to_json(&self.recorded).expect("recorded trace entries are valid")
    }

    pub fn remaining(&self) -> usize {
        self.script.len().saturating_sub(self.cursor)
    }

    pub fn is_exhausted(&self) -> bool {
        self.cursor >= self.script.len()
    }

    fn take_expected(&mut self, op: &'static str) -> Result<TraceEntry, Trap> {
        let Some(entry) = self.script.get(self.cursor).cloned() else {
            return Err(Trap {
                op: None,
                opcode: Some(op),
                message: format!(
                    "effect trace exhausted at step {} while executing {op}",
                    self.cursor
                ),
            });
        };
        if entry.op != op {
            return Err(Trap {
                op: None,
                opcode: Some(op),
                message: format!(
                    "effect trace mismatch at step {}: expected {op}, trace has {}",
                    self.cursor, entry.op
                ),
            });
        }
        self.cursor += 1;
        self.recorded.push(entry.clone());
        Ok(entry)
    }
}

impl Host for TraceHost {
    fn file_read(&mut self, handle: u32, max: u64) -> Result<Result<Vec<u8>, u32>, Trap> {
        let entry = self.take_expected("FILE_READ")?;
        match entry.result {
            Value::Ok(v) => match *v {
                Value::Seq(xs) => {
                    let mut bytes = Vec::with_capacity(xs.len());
                    for x in xs {
                        match x {
                            Value::Int(b) if (0..=255).contains(&b) => bytes.push(b as u8),
                            other => {
                                return Err(Trap {
                                    op: None,
                                    opcode: Some("FILE_READ"),
                                    message: format!(
                                        "FILE_READ(handle={handle}, max={max}): invalid byte {other:?} in trace"
                                    ),
                                });
                            }
                        }
                    }
                    Ok(Ok(bytes))
                }
                other => Err(Trap {
                    op: None,
                    opcode: Some("FILE_READ"),
                    message: format!("FILE_READ trace Ok value {other:?} is not a byte sequence"),
                }),
            },
            Value::Err(e) => match *e {
                Value::Int(code) if (0..=i128::from(u32::MAX)).contains(&code) => {
                    Ok(Err(code as u32))
                }
                other => Err(Trap {
                    op: None,
                    opcode: Some("FILE_READ"),
                    message: format!("FILE_READ trace Err value {other:?} is not a u32"),
                }),
            },
            other => Err(Trap {
                op: None,
                opcode: Some("FILE_READ"),
                message: format!("FILE_READ trace result {other:?} is not Result<Slice<u8>, u32>"),
            }),
        }
    }

    fn file_write(&mut self, handle: u32, data: &[u8]) -> Result<Result<u64, u32>, Trap> {
        let entry = self.take_expected("FILE_WRITE")?;
        match entry.result {
            Value::Ok(v) => match *v {
                Value::Int(n) if (0..=i128::from(u64::MAX)).contains(&n) => Ok(Ok(n as u64)),
                other => Err(Trap {
                    op: None,
                    opcode: Some("FILE_WRITE"),
                    message: format!(
                        "FILE_WRITE(handle={handle}, len={}): trace Ok value {other:?} is not u64",
                        data.len()
                    ),
                }),
            },
            Value::Err(e) => match *e {
                Value::Int(code) if (0..=i128::from(u32::MAX)).contains(&code) => {
                    Ok(Err(code as u32))
                }
                other => Err(Trap {
                    op: None,
                    opcode: Some("FILE_WRITE"),
                    message: format!("FILE_WRITE trace Err value {other:?} is not u32"),
                }),
            },
            other => Err(Trap {
                op: None,
                opcode: Some("FILE_WRITE"),
                message: format!("FILE_WRITE trace result {other:?} is not Result<u64, u32>"),
            }),
        }
    }

    fn clock_read(&mut self, handle: u32) -> Result<u64, Trap> {
        let entry = self.take_expected("CLOCK_READ")?;
        match entry.result {
            Value::Int(ns) if (0..=i128::from(u64::MAX)).contains(&ns) => Ok(ns as u64),
            other => Err(Trap {
                op: None,
                opcode: Some("CLOCK_READ"),
                message: format!("CLOCK_READ(handle={handle}): trace value {other:?} is not u64"),
            }),
        }
    }
}

/// Wraps any [`Host`] and records every effect result in docs/16 trace format.
#[derive(Clone, Debug)]
pub struct RecordingHost<H: Host> {
    inner: H,
    recorded: Vec<TraceEntry>,
}

impl<H: Host> RecordingHost<H> {
    pub fn new(inner: H) -> Self {
        Self {
            inner,
            recorded: Vec::new(),
        }
    }

    pub fn recorded(&self) -> &[TraceEntry] {
        &self.recorded
    }

    pub fn recorded_json(&self) -> String {
        trace_to_json(&self.recorded).expect("recorded trace entries are valid")
    }

    pub fn inner(&self) -> &H {
        &self.inner
    }

    pub fn inner_mut(&mut self) -> &mut H {
        &mut self.inner
    }

    pub fn into_inner(self) -> H {
        self.inner
    }
}

impl<H: Host> Host for RecordingHost<H> {
    fn file_read(&mut self, handle: u32, max: u64) -> Result<Result<Vec<u8>, u32>, Trap> {
        let res = self.inner.file_read(handle, max)?;
        let val = match &res {
            Ok(bytes) => Value::Ok(Box::new(Value::Seq(
                bytes.iter().map(|&b| Value::Int(i128::from(b))).collect(),
            ))),
            Err(code) => Value::Err(Box::new(Value::Int(i128::from(*code)))),
        };
        self.recorded.push(TraceEntry {
            op: "FILE_READ".to_string(),
            result: val,
        });
        Ok(res)
    }

    fn file_write(&mut self, handle: u32, data: &[u8]) -> Result<Result<u64, u32>, Trap> {
        let res = self.inner.file_write(handle, data)?;
        let val = match &res {
            Ok(n) => Value::Ok(Box::new(Value::Int(i128::from(*n)))),
            Err(code) => Value::Err(Box::new(Value::Int(i128::from(*code)))),
        };
        self.recorded.push(TraceEntry {
            op: "FILE_WRITE".to_string(),
            result: val,
        });
        Ok(res)
    }

    fn clock_read(&mut self, handle: u32) -> Result<u64, Trap> {
        let ns = self.inner.clock_read(handle)?;
        self.recorded.push(TraceEntry {
            op: "CLOCK_READ".to_string(),
            result: Value::Int(i128::from(ns)),
        });
        Ok(ns)
    }

    fn theta(&mut self, callee: &Hash, node: &Node) -> Option<Vec<Value>> {
        self.inner.theta(callee, node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_format_trace_round_trip() {
        let src = r#"[
            {"op": "CLOCK_READ", "result": "1000"},
            {"op": "FILE_READ", "result": {"ok": [104, 105]}},
            {"op": "FILE_READ", "result": {"err": 2}},
            {"op": "FILE_WRITE", "result": {"ok": "5"}}
        ]"#;
        let entries = parse_trace_json(src).expect("parse trace");
        assert_eq!(entries.len(), 4);
        assert_eq!(entries[0].op, "CLOCK_READ");
        assert_eq!(entries[0].result, Value::Int(1000));
        assert_eq!(
            entries[1].result,
            Value::Ok(Box::new(Value::Seq(vec![Value::Int(104), Value::Int(105)])))
        );
        let serialized = trace_to_json(&entries).expect("serialize");
        let reparsed = parse_trace_json(&serialized).expect("reparse");
        assert_eq!(entries, reparsed);
    }

    #[test]
    fn trace_host_records_and_detects_mismatch() {
        let src = r#"[{"op": "CLOCK_READ", "result": "42"}]"#;
        let host = TraceHost::from_json(src).unwrap();
        let mut rec = RecordingHost::new(host);
        assert!(rec.file_read(0, 10).is_err());
        assert_eq!(rec.clock_read(0).unwrap(), 42);
        assert!(rec.inner().is_exhausted());
        assert_eq!(rec.recorded_json(), rec.inner().recorded_json());
        assert!(rec.clock_read(0).is_err());
    }

    #[test]
    fn no_effects_host_traps() {
        let mut h = NoEffectsHost;
        assert!(h.file_read(0, 10).is_err());
        assert!(h.file_write(0, b"hi").is_err());
        assert!(h.clock_read(0).is_err());
    }
}
