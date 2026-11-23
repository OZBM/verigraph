//! Verification conditions, SMT-LIB2 emission, solver drivers, counterexamples, proof cache.

pub mod formula;
pub mod model;
pub mod sexpr;
pub mod solver;
pub mod sort;
mod verify;

use std::path::PathBuf;

use vg_core::ir::{Hash, Node};
use vg_core::value::Value;

#[derive(Clone, Debug)]
pub struct Options {
    pub z3: PathBuf,
    /// cvc5 and Bitwuzla, when installed. Bitwuzla takes the float queries whose formats it
    /// supports, and in release mode a second solver must agree (docs/05, P3.3).
    pub cvc5: Option<PathBuf>,
    pub bitwuzla: Option<PathBuf>,
    pub timeout_ms: u64,
    /// Release mode: an obligation is proven only when two solvers agree (`two-solvers`).
    pub release: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            z3: find_z3(),
            cvc5: find_solver("cvc5"),
            bitwuzla: find_solver("bitwuzla"),
            timeout_ms: 10_000,
            release: false,
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
    /// docs/15: `re-solved` for one solver's answer, `two-solvers` when a second agreed.
    pub evidence: Option<&'static str>,
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

/// Finds solver `name` in `/opt/homebrew/bin`, `/usr/local/bin`, or on `PATH`.
pub fn find_solver(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    ["/opt/homebrew/bin", "/usr/local/bin"]
        .into_iter()
        .map(PathBuf::from)
        .chain(std::env::split_paths(&path))
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// Finds `z3` as `find_solver` does, or names it bare for the OS to find.
pub fn find_z3() -> PathBuf {
    find_solver("z3").unwrap_or_else(|| PathBuf::from("z3"))
}

/// Every obligation of `node` (docs/05, docs/14), each with its verdict; see verify.rs.
pub fn verify(
    node: &Node,
    label: &str,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    opts: &Options,
) -> Vec<Outcome> {
    verify::verify(node, label, callee, opts)
}
