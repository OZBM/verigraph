//! Proof erasure, LLVM IR emission, clang driver, OS sandbox manifests.

pub mod analysis;
pub mod driver;
pub mod emit;
pub mod formula;
pub mod ir;
pub mod layout;

use vg_core::diag::Diagnostic;
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
    root: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    opts: &Options,
) -> Result<String, Diagnostic> {
    Ok(emit::emit_module(root, callee, opts)?.ir)
}

pub fn build(
    root: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    opts: &Options,
    out: &std::path::Path,
) -> Result<BuildReport, Diagnostic> {
    driver::build_artifact(root, callee, opts, out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use vg_core::asm_local;

    #[test]
    fn emit_llvm_is_deterministic_and_honors_proof_facts_and_no_fast_math() {
        let src = r#"
module m;
NODE @k {
  SIGNATURE: (x: u32, y: u32) -> (u32, f32);
  EFFECTS: [];
  CONTRACT {
    PRE: x < 1000 && y < 1000;
  }
  BODY {
    %0 = ADD.checked(x, y);
    %1 = CAST.f32(%0);
    %2 = FMUL(%1, 2.0:f32);
    %3 = FADD(%2, 1.0:f32);
    %4 = FMIN(%3, 100.0:f32);
    RETURN(%0, %4);
  }
}
"#;
        let nodes = asm_local(src).unwrap();
        let root = &nodes[0].node;

        let with_facts = emit_llvm(
            root,
            &|_| None,
            &Options {
                target: Target::Aarch64Darwin,
                proof_facts: true,
                runtime_checks: false,
            },
        )
        .unwrap();
        let with_facts_2 = emit_llvm(
            root,
            &|_| None,
            &Options {
                target: Target::Aarch64Darwin,
                proof_facts: true,
                runtime_checks: false,
            },
        )
        .unwrap();
        assert_eq!(with_facts, with_facts_2);

        let no_facts = emit_llvm(
            root,
            &|_| None,
            &Options {
                target: Target::Aarch64Darwin,
                proof_facts: false,
                runtime_checks: false,
            },
        )
        .unwrap();

        assert!(with_facts.contains("add nuw"));
        assert!(with_facts.contains("noalias"));
        assert!(with_facts.contains("llvm.assume"));
        assert!(!no_facts.contains("add nuw"));
        assert!(!no_facts.contains("noalias"));
        assert!(!no_facts.contains("llvm.assume"));
        assert!(!with_facts.contains("fast"));
        assert!(!with_facts.contains("fmuladd"));
        assert!(with_facts.contains("llvm.minimum.f32"));
    }

    #[test]
    fn emit_llvm_works_without_post_only_spec_callee() {
        let src = r#"
module m;
SPEC @s {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {}
  BODY { RETURN(x); }
}
NODE @f {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    POST: result == spec:s(x);
  }
  BODY { RETURN(x); }
}
"#;
        let nodes = asm_local(src).unwrap();
        let empty: HashMap<Hash, Node> = HashMap::new();
        let ir = emit_llvm(
            &nodes[1].node,
            &|h| empty.get(h).cloned(),
            &Options {
                target: Target::Aarch64Darwin,
                proof_facts: true,
                runtime_checks: true,
            },
        )
        .unwrap();
        assert!(ir.contains("define i32 @vg_"));
    }
}
