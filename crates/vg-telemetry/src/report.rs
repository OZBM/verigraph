//! `FailureReport` schema (docs/15), `report(...)`, and plain-English `prose(...)` (P6.3).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use serde_json::{Map, Value as J};
use vg_core::ir::{Hash, Node, Operand, hex};
use vg_core::types::Type;
use vg_smt::{Outcome, Verdict};

use crate::diff::{SmtObligationContext, diff};
use crate::names::Names;
use crate::slice::slice_with_trace;
use crate::trace::{check_violation, parse_obligation_id, trace_eval, value_to_report_json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reproduced {
    Concrete,
    Model,
    No,
}

impl Reproduced {
    pub fn as_str(self) -> &'static str {
        match self {
            Reproduced::Concrete => "concrete",
            Reproduced::Model => "model",
            Reproduced::No => "no",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SliceEntry {
    pub port: String,
    pub op: String,
    pub value: J,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleEntry {
    pub site: String,
    pub values: Vec<J>,
}

/// Counterfactual failure report matching docs/15's `FailureReport` schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureReport {
    pub node: String,
    pub obligation: String,
    pub obligation_hash: String,
    pub clause: String,
    pub verdict: String,
    pub code: String,
    pub inputs: BTreeMap<String, J>,
    pub theta: Option<BTreeMap<String, J>>,
    pub step: Option<u64>,
    pub oracle: Option<Vec<OracleEntry>>,
    pub slice: Vec<SliceEntry>,
    pub diff: Option<String>,
    pub reproduced: Reproduced,
    pub guard: Option<String>,
    pub target: Option<String>,
}

impl FailureReport {
    /// Converts the report into a `serde_json::Value` with deterministically sorted object keys.
    pub fn to_json(&self) -> J {
        let mut m = Map::new();
        m.insert("clause".to_string(), J::String(self.clause.clone()));
        m.insert("code".to_string(), J::String(self.code.clone()));
        if let Some(d) = &self.diff {
            m.insert("diff".to_string(), J::String(d.clone()));
        }
        if let Some(g) = &self.guard {
            m.insert("guard".to_string(), J::String(g.clone()));
        }
        let inputs_map: Map<String, J> = self
            .inputs
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        m.insert("inputs".to_string(), J::Object(inputs_map));
        m.insert("node".to_string(), J::String(self.node.clone()));
        m.insert("obligation".to_string(), J::String(self.obligation.clone()));
        m.insert(
            "obligation_hash".to_string(),
            J::String(self.obligation_hash.clone()),
        );
        if let Some(oracles) = &self.oracle {
            let arr = oracles
                .iter()
                .map(|o| {
                    let mut om = Map::new();
                    om.insert("site".to_string(), J::String(o.site.clone()));
                    om.insert("values".to_string(), J::Array(o.values.clone()));
                    J::Object(om)
                })
                .collect();
            m.insert("oracle".to_string(), J::Array(arr));
        }
        m.insert(
            "reproduced".to_string(),
            J::String(self.reproduced.as_str().to_string()),
        );
        let slice_arr = self
            .slice
            .iter()
            .map(|s| {
                let mut sm = Map::new();
                sm.insert("op".to_string(), J::String(s.op.clone()));
                sm.insert("port".to_string(), J::String(s.port.clone()));
                sm.insert("value".to_string(), s.value.clone());
                J::Object(sm)
            })
            .collect();
        m.insert("slice".to_string(), J::Array(slice_arr));
        if let Some(step) = self.step {
            m.insert("step".to_string(), J::from(step));
        }
        if let Some(t) = &self.target {
            m.insert("target".to_string(), J::String(t.clone()));
        }
        if let Some(theta) = &self.theta {
            let tm: Map<String, J> = theta.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            m.insert("theta".to_string(), J::Object(tm));
        }
        m.insert("verdict".to_string(), J::String(self.verdict.clone()));
        J::Object(m)
    }

    /// Deterministic compact JSON string with sorted keys.
    pub fn to_json_string(&self) -> String {
        serde_json::to_string(&self.to_json()).unwrap_or_else(|_| "{}".to_string())
    }
}

fn verdict_str(v: Verdict) -> &'static str {
    match v {
        Verdict::Proven => "proven",
        Verdict::Counterexample => "counterexample",
        Verdict::ContractGap => "contract_gap",
        Verdict::Vacuous => "vacuous",
        Verdict::Unknown => "unknown",
        Verdict::Error => "error",
    }
}

fn default_code(v: Verdict) -> &'static str {
    match v {
        Verdict::Proven => "",
        Verdict::Counterexample => "VG-V0001",
        Verdict::ContractGap => "VG-V0002",
        Verdict::Vacuous => "VG-V0003",
        Verdict::Unknown => "VG-V0005",
        Verdict::Error => "VG-V0008",
    }
}

/// Builds a `FailureReport` for a non-proven `vg_smt::Outcome`. Returns `None` when `outcome`
/// is `Verdict::Proven`.
pub fn report(
    node: &Node,
    label: &str,
    names: &Names,
    outcome: &Outcome,
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Option<FailureReport> {
    if outcome.verdict == Verdict::Proven {
        return None;
    }

    let node_name = format!("@{}", label.trim_start_matches('@'));
    let kind = parse_obligation_id(&outcome.id);

    let obligation_hash = match &kind {
        Some(k) => {
            let ctx = SmtObligationContext::build(node, k, callee, false);
            hex(&ctx.obligation_hash(node, &outcome.id, callee))
        }
        None => {
            let (bytes, _) = vg_core::codec::encode(node);
            let mut data = bytes;
            data.push(0);
            data.extend_from_slice(outcome.id.as_bytes());
            hex(&vg_core::hash::derive(
                vg_core::hash::Context::Obligation,
                &data,
            ))
        }
    };

    let clause = match &kind {
        Some(k) => names.clause_text(node, k, callee),
        None => outcome.id.clone(),
    };

    let mut verdict = outcome.verdict;
    let mut code = outcome
        .code
        .unwrap_or_else(|| default_code(verdict))
        .to_string();

    let mut inputs_map = BTreeMap::new();
    let mut theta_map = if node.learnable.is_empty() {
        None
    } else {
        Some(BTreeMap::new())
    };
    let mut step = None;
    let mut slice_entries = Vec::new();
    let mut diff_text = None;
    let mut reproduced = Reproduced::No;

    if let Some(raw_inputs) = &outcome.inputs {
        let trace = trace_eval(node, raw_inputs, callee);

        let mut all_reads = trace.accessed_indices.clone();
        if let Some(k) = &kind {
            let chk = check_violation(node, k, raw_inputs, &trace, callee);
            for (opnd, idxs) in chk.extra_indices {
                all_reads.entry(opnd).or_default().extend(idxs);
            }
            if chk.pre_holds && chk.violated {
                reproduced = Reproduced::Concrete;
                step = chk.step;
            } else if verdict == Verdict::ContractGap {
                reproduced = Reproduced::Model;
            } else if chk.pre_holds && !chk.violated && verdict == Verdict::Counterexample {
                // Only demote to contract_gap if the node actually has callees/specs/folds whose
                // contracts could explain the gap and the node is pure.
                if node.effects.is_pure() && !node.calls.is_empty() {
                    verdict = Verdict::ContractGap;
                    code = "VG-V0002".to_string();
                    reproduced = Reproduced::Model;
                }
            }
        }

        let empty_reads = BTreeSet::new();
        for (i, val) in raw_inputs.iter().enumerate() {
            let Some(ty) = node.input_type(i as u32) else {
                continue;
            };
            if *ty == Type::Eff {
                continue;
            }
            let name = names.input_name(i as u32);
            let reads = all_reads
                .get(&Operand::Input(i as u32))
                .unwrap_or(&empty_reads);
            let jval = value_to_report_json(val, ty, reads);
            if i < node.params.len() {
                inputs_map.insert(name, jval);
            } else if let Some(tm) = &mut theta_map {
                tm.insert(name, jval);
            }
        }

        let (slice_ops, entries) =
            slice_with_trace(node, &outcome.id, names, raw_inputs, &trace, callee);
        slice_entries = entries;
        if !slice_ops.is_empty() {
            diff_text = diff(node, &outcome.id, names, &slice_ops, callee);
        }
    }

    Some(FailureReport {
        node: node_name,
        obligation: outcome.id.clone(),
        obligation_hash,
        clause,
        verdict: verdict_str(verdict).to_string(),
        code,
        inputs: inputs_map,
        theta: theta_map,
        step,
        oracle: None,
        slice: slice_entries,
        diff: diff_text,
        reproduced,
        guard: None,
        target: None,
    })
}

/// Renders a concise, deterministic plain-English explanation of `r` for `vg explain`.
pub fn prose(r: &FailureReport) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "{} failed obligation {} ({}, {}): clause `{}` did not hold.",
        r.node, r.obligation, r.verdict, r.code, r.clause
    );
    if let (Some(g), Some(t)) = (&r.guard, &r.target) {
        let _ = write!(out, " Triggered by runtime guard {g} on {t}.");
    }
    if let Some(s) = r.step {
        let _ = write!(out, " Failed at fold step {s}.");
    }
    if !r.inputs.is_empty() {
        let pairs: Vec<String> = r.inputs.iter().map(|(k, v)| format!("{k} = {v}")).collect();
        let _ = write!(out, " Counterexample inputs: {}.", pairs.join(", "));
    }
    if let Some(theta) = &r.theta
        && !theta.is_empty()
    {
        let pairs: Vec<String> = theta.iter().map(|(k, v)| format!("{k} = {v}")).collect();
        let _ = write!(out, " Learnable theta: {}.", pairs.join(", "));
    }
    if !r.slice.is_empty() {
        let steps: Vec<String> = r
            .slice
            .iter()
            .map(|s| format!("{} = {} -> {}", s.port, s.op, s.value))
            .collect();
        let _ = write!(out, " Failing slice: {}.", steps.join("; "));
    }
    if let Some(d) = &r.diff {
        let _ = write!(out, " Necessary/sufficient condition: {d}.");
    }
    let _ = write!(out, " Replay: {}.", r.reproduced.as_str());
    out
}
