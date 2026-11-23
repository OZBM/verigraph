//! P3.6 and P3.8 (docs/16, "Verifier suites"): every file of tests/corpus/good verifies; every file
//! of tests/corpus/bad is refuted at the obligation its `// expect: reject <ID>` line names, with a
//! counterexample that reproduces concretely in vg-interp (ADR 0006); the examples tell the
//! soft-knee story of docs/03.

use std::fs;
use std::path::{Path, PathBuf};

use vg_core::ir::{Hash, Node};
use vg_core::value::Value;
use vg_interp::{Clause, NoEffectsHost, holds, run};
use vg_smt::{Options, Verdict, verify};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn opts() -> Options {
    Options {
        z3: vg_smt::find_z3(),
        timeout_ms: 20_000,
    }
}

/// The inputs satisfy PRE, and the node traps or breaks POST on them, with the real callees.
fn reproduces(node: &Node, inputs: &[Value], callee: &dyn Fn(&Hash) -> Option<Node>) -> bool {
    if holds(node, Clause::Pre, inputs, &[], callee) != Ok(true) {
        return false;
    }
    match run(node, inputs, callee, &mut NoEffectsHost) {
        Err(_) => true,
        Ok(r) => holds(node, Clause::Post, inputs, &r, callee) != Ok(true),
    }
}

/// Checks one file against its expectation line; Err names what went wrong.
fn check(path: &Path) -> Result<(), String> {
    let src = fs::read_to_string(path).unwrap();
    let expect = src
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("// expect: "))
        .ok_or("no expect line")?;
    let nodes = vg_core::asm_local(&src).map_err(|e| e.to_string())?;
    let callee = |h: &Hash| {
        nodes
            .iter()
            .find(|a| a.hashes.node == *h)
            .map(|a| a.node.clone())
    };
    if expect == "verify" {
        for a in &nodes {
            for o in verify(&a.node, &a.label, &callee, &opts()) {
                if o.verdict != Verdict::Proven {
                    return Err(format!("{} is {:?}: {}", o.id, o.verdict, o.message));
                }
            }
        }
        return Ok(());
    }
    let id = expect
        .strip_prefix("reject ")
        .ok_or("expect: verify or reject <ID>")?;
    let label = id.trim_start_matches('@').split('/').next().unwrap();
    let a = nodes
        .iter()
        .find(|a| a.label == label)
        .ok_or("no such node")?;
    let o = verify(&a.node, &a.label, &callee, &opts())
        .into_iter()
        .find(|o| o.id == id)
        .ok_or(format!("no obligation {id}"))?;
    match (&o.verdict, &o.inputs) {
        (Verdict::Counterexample, Some(inputs)) if reproduces(&a.node, inputs, &callee) => Ok(()),
        (Verdict::Counterexample, Some(inputs)) => {
            Err(format!("{id}: {inputs:?} does not reproduce"))
        }
        (v, _) => Err(format!(
            "{id} is {v:?}, not a counterexample: {}",
            o.message
        )),
    }
}

fn files(dir: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(repo().join(dir))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "vgs"))
        .collect();
    v.sort();
    v
}

#[test]
fn known_good_verify_and_known_bad_are_refuted() {
    let good = files("tests/corpus/good");
    let bad = files("tests/corpus/bad");
    assert!(good.len() >= 12 && bad.len() >= 12, "corpus too small");
    let failures: Vec<String> = good
        .iter()
        .chain(&bad)
        .filter_map(|f| check(f).err().map(|e| format!("{}: {e}", f.display())))
        .collect();
    eprintln!(
        "{} good and {} bad files checked, {} failures",
        good.len(),
        bad.len(),
        failures.len()
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_soft_knee_story_holds() {
    let root = repo();
    assert_eq!(check(&root.join("examples/quickstart.vgs")), Ok(()));
    assert_eq!(check(&root.join("examples/soft_knee_draft.vgs")), Ok(()));
}
