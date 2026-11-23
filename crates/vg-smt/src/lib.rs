//! Verification conditions, SMT-LIB2 emission, solver drivers, counterexamples, proof cache.

pub mod formula;
pub mod model;
pub mod sexpr;
pub mod solver;
pub mod sort;

use std::path::PathBuf;

use vg_core::ir::{Hash, Node};
use vg_core::value::Value;

#[derive(Clone, Debug)]
pub struct Options {
    pub z3: PathBuf,
    pub timeout_ms: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            z3: find_z3(),
            timeout_ms: 10_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Proven,
    Counterexample,
    ContractGap,
    Vacuous,
    Unknown,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub id: String,
    pub verdict: Verdict,
    pub code: Option<&'static str>,
    pub solver: String,
    pub inputs: Option<Vec<Value>>,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplayVerdict {
    ConcreteCounterexample,
    ContractGap,
    PartialAssumptionViolation,
    VerifierBug,
}

/// ADR 0006 replay hook: W0 replaces this stub with vg-interp model-guided and concrete replay.
pub fn classify_sat_model(
    _node: &Node,
    _obligation_id: &str,
    _inputs: &[Value],
    _callee: &dyn Fn(&Hash) -> Option<Node>,
) -> ReplayVerdict {
    ReplayVerdict::ConcreteCounterexample
}

/// Finds `z3` at `/opt/homebrew/bin/z3`, `/usr/local/bin/z3`, or on `PATH`.
pub fn find_z3() -> PathBuf {
    for candidate in ["/opt/homebrew/bin/z3", "/usr/local/bin/z3"] {
        let p = PathBuf::from(candidate);
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from("z3")
}

pub fn verify(
    _node: &Node,
    _label: &str,
    _callee: &dyn Fn(&Hash) -> Option<Node>,
    _opts: &Options,
) -> Vec<Outcome> {
    Vec::new()
}
