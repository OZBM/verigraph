//! Runtime guard map parsing and guard-failure `FailureReport` synthesis (docs/06, docs/07, P6.4).

use std::collections::BTreeMap;

use serde_json::{Map, Value as J};
use vg_core::ir::{Hash, Node};
use vg_core::opcode;
use vg_core::value::Value;
use vg_smt::{Outcome, Verdict};

use crate::names::Names;
use crate::report::{FailureReport, report};
use crate::trace::parse_obligation_id;

/// One entry in a guard map JSON emitted alongside compiled binaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardMapEntry {
    pub obligation: String,
    pub obligation_hash: Option<String>,
    pub node: Option<String>,
}

/// Maps runtime guard IDs (e.g. `"g0"`) to their obligation metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GuardMap {
    pub entries: BTreeMap<String, GuardMapEntry>,
}

/// One JSON-lines telemetry event written by `vg-rt`'s `vg_emit_guard` / `vg_trap_guard`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuardEvent {
    pub guard: String,
    pub obligation: String,
    pub node: String,
    pub target: String,
}

impl GuardMap {
    /// Parses a guard map JSON string. Accepts both docs/06's object form
    /// `{"g0": {"obligation": "@n/%3/ASSERT_GUARD", "obligation_hash": "..."}}`
    /// and the shorthand string form `{"g0": "@n/%3/ASSERT_GUARD"}`.
    pub fn from_json(json_str: &str) -> Result<Self, String> {
        let root: J =
            serde_json::from_str(json_str).map_err(|e| format!("invalid guard map JSON: {e}"))?;
        let obj = root
            .as_object()
            .ok_or_else(|| "guard map must be a JSON object".to_string())?;
        let mut entries = BTreeMap::new();
        for (guard_id, val) in obj {
            let entry = match val {
                J::String(ob) => GuardMapEntry {
                    obligation: ob.clone(),
                    obligation_hash: None,
                    node: None,
                },
                J::Object(m) => {
                    let obligation = m
                        .get("obligation")
                        .and_then(J::as_str)
                        .ok_or_else(|| format!("guard '{guard_id}' missing string 'obligation'"))?
                        .to_string();
                    let obligation_hash = m
                        .get("obligation_hash")
                        .and_then(J::as_str)
                        .map(str::to_string);
                    let node = m.get("node").and_then(J::as_str).map(str::to_string);
                    GuardMapEntry {
                        obligation,
                        obligation_hash,
                        node,
                    }
                }
                _ => {
                    return Err(format!(
                        "guard '{guard_id}' entry must be a string or object"
                    ));
                }
            };
            entries.insert(guard_id.clone(), entry);
        }
        Ok(Self { entries })
    }

    pub fn to_json(&self) -> J {
        let mut root = Map::new();
        for (gid, entry) in &self.entries {
            let mut m = Map::new();
            m.insert(
                "obligation".to_string(),
                J::String(entry.obligation.clone()),
            );
            if let Some(h) = &entry.obligation_hash {
                m.insert("obligation_hash".to_string(), J::String(h.clone()));
            }
            if let Some(n) = &entry.node {
                m.insert("node".to_string(), J::String(n.clone()));
            }
            root.insert(gid.clone(), J::Object(m));
        }
        J::Object(root)
    }
}

/// Parses one JSON line emitted by `vg_emit_guard` / `vg_trap_guard` in `runtime/vg_rt.c`.
pub fn parse_guard_event(line: &str) -> Result<GuardEvent, String> {
    let val: J =
        serde_json::from_str(line.trim()).map_err(|e| format!("invalid guard event JSON: {e}"))?;
    let obj = val
        .as_object()
        .ok_or_else(|| "guard event must be a JSON object".to_string())?;
    let ev = obj.get("event").and_then(J::as_str);
    if ev != Some("guard") {
        return Err(format!("expected event 'guard', got {ev:?}"));
    }
    let req = |k: &str| -> Result<String, String> {
        obj.get(k)
            .and_then(J::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("guard event missing string '{k}'"))
    };
    Ok(GuardEvent {
        guard: req("guard")?,
        obligation: req("obligation")?,
        node: req("node")?,
        target: req("target")?,
    })
}

/// Produces a `FailureReport` for a runtime guard trigger, populated with `"guard"` and `"target"`.
#[allow(clippy::too_many_arguments)]
pub fn guard_report(
    guard_map: &GuardMap,
    guard_id: &str,
    target: &str,
    node: &Node,
    label: &str,
    names: &Names,
    inputs: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Result<FailureReport, String> {
    let entry = guard_map
        .entries
        .get(guard_id)
        .ok_or_else(|| format!("unknown guard id '{guard_id}'"))?;

    let kind = parse_obligation_id(&entry.obligation)
        .ok_or_else(|| format!("unrecognized obligation ID '{}'", entry.obligation))?;
    if let Some(op_idx) = kind.op_index()
        && let Some(op) = node.ops.get(op_idx as usize)
        && opcode::by_code(op.opcode).is_some_and(|i| i.name == "ASSERT_GUARD")
        && op.mode != 0
    {
        return Err(format!(
            "guard '{guard_id}' refers to ASSERT_GUARD.branch (%{op_idx}), which is normal control flow"
        ));
    }

    let synthetic_outcome = Outcome {
        id: entry.obligation.clone(),
        verdict: Verdict::Counterexample,
        code: Some("VG-V0001"),
        solver: "runtime".to_string(),
        inputs: Some(inputs.to_vec()),
        message: format!("runtime guard {guard_id} failed on {target}"),
        evidence: None,
        hash: None,
        time_ms: 0,
    };

    let mut rep = report(node, label, names, &synthetic_outcome, callee)
        .ok_or_else(|| format!("failed to construct report for guard '{guard_id}'"))?;
    if let Some(h) = &entry.obligation_hash {
        rep.obligation_hash = h.clone();
    }
    rep.guard = Some(guard_id.to_string());
    rep.target = Some(target.to_string());
    Ok(rep)
}
