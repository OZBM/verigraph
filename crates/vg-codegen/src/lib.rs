//! Proof erasure, LLVM IR emission, clang driver, OS sandbox manifests.

pub mod analysis;
pub mod layout;

use vg_core::diag::{Diagnostic, Span};
use vg_core::ir::{Hash, Node};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Aarch64Darwin,
    X86_64Linux,
    Wasm32Wasi,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    pub target: Target,
    /// Facts from proven obligations (nsw, nuw, inbounds); false gives the --no-proof-facts build.
    pub proof_facts: bool,
    /// Keep a runtime check on every partial op (overflow, bounds, division), exiting 4 when one
    /// fails. W0 turns it off only for nodes vg-smt fully proved.
    pub runtime_checks: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildReport {
    pub artifact: std::path::PathBuf,
    pub linked: bool,
    pub residual_guards: u32,
}

pub fn emit_llvm(
    _root: &Node,
    _callee: &dyn Fn(&Hash) -> Option<Node>,
    _opts: &Options,
) -> Result<String, Diagnostic> {
    Err(Diagnostic::error(
        "VG-B0001",
        Span::default(),
        "LLVM emission not yet implemented",
    ))
}

pub fn build(
    _root: &Node,
    _callee: &dyn Fn(&Hash) -> Option<Node>,
    _opts: &Options,
    _out: &std::path::Path,
) -> Result<BuildReport, Diagnostic> {
    Err(Diagnostic::error(
        "VG-B0001",
        Span::default(),
        "build not yet implemented",
    ))
}
