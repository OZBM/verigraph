//! P2.5 (docs/16, "Interpreter conformance"): every `// run:` vector of tests/conformance runs in
//! vg-interp with exactly the expected outputs, bit for bit (NaN as NaN), or traps where the vector
//! says so. Expected values come from tools/gen_conformance.py, which computes them in exact
//! arithmetic and never calls vg-interp.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value as J;
use vg_core::ast::Item;
use vg_core::ir::{Hash, Node};
use vg_core::types::Type;
use vg_core::value::Value;
use vg_interp::{NoEffectsHost, TraceHost, run, value_from_json};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// One case against one node: Ok(()) when the outputs (or the trap) are as expected.
fn check_case(
    node: &Node,
    names: &[String],
    case: &J,
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Result<(), String> {
    let inputs = case["inputs"].as_object().ok_or("case without inputs")?;
    let mut args = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let t = node.input_type(i as u32).ok_or("input type")?;
        args.push(match t {
            Type::Eff => Value::Eff,
            t => {
                let j = inputs.get(name).ok_or(format!("missing input {name}"))?;
                value_from_json(&j.to_string(), t).map_err(|e| format!("input {name}: {e}"))?
            }
        });
    }
    let got = match case.get("trace") {
        Some(trace) => {
            let mut host = TraceHost::from_json(&trace.to_string())?;
            run(node, &args, callee, &mut host)
        }
        None => run(node, &args, callee, &mut NoEffectsHost),
    };
    match (case.get("trap"), got) {
        (Some(_), Err(_)) => Ok(()),
        (Some(kind), Ok(r)) => Err(format!("expected a {kind} trap, got {r:?}")),
        (None, Err(t)) => Err(format!("unexpected trap: {}", t.message)),
        (None, Ok(results)) => {
            let want = case["outputs"].as_array().ok_or("case without outputs")?;
            let values: Vec<(&Value, &Type)> = results
                .iter()
                .zip(&node.results)
                .filter(|(_, t)| **t != Type::Eff)
                .collect();
            if values.len() != want.len() {
                return Err(format!("{} outputs, expected {}", values.len(), want.len()));
            }
            for ((v, t), w) in values.into_iter().zip(want) {
                let w = value_from_json(&w.to_string(), t)
                    .map_err(|e| format!("expected output: {e}"))?;
                if *v != w {
                    return Err(format!("got {v:?}, expected {w:?}"));
                }
            }
            Ok(())
        }
    }
}

/// The cases of one run line: a docs/16 vector file, or one inline `{inputs} => [outputs]` case.
fn cases_of(rest: &str, dir: &Path) -> Vec<J> {
    if let Some(path) = rest.strip_prefix("file:") {
        let v: J = serde_json::from_str(&fs::read_to_string(dir.join(path)).unwrap()).unwrap();
        return v["cases"].as_array().expect("cases").clone();
    }
    let (inputs, outputs) = rest
        .split_once(" => ")
        .expect("inline run line: {inputs} => [outputs]");
    let mut case = serde_json::json!({ "inputs": serde_json::from_str::<J>(inputs).unwrap() });
    match outputs.trim() {
        o if o.starts_with("\"trap") => case["trap"] = J::String(o.trim_matches('"').into()),
        o => case["outputs"] = serde_json::from_str(o).unwrap(),
    }
    vec![case]
}

fn param_names(ast: &vg_core::ast::Module, label: &str) -> Vec<String> {
    ast.items
        .iter()
        .find_map(|i| match i {
            Item::Node(n) if n.label == label => Some(
                n.params
                    .iter()
                    .map(|p| p.name.text.clone())
                    .chain(n.learnable.iter().flatten().map(|l| l.name.text.clone()))
                    .collect(),
            ),
            _ => None,
        })
        .expect("node definition")
}

#[test]
fn conformance_vectors_hold() {
    let dir = repo().join("tests/conformance");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "vgs"))
        .collect();
    files.sort();
    let (mut cases, mut traps, mut failures) = (0, 0, Vec::new());
    let mut opcodes = BTreeSet::new();
    for f in &files {
        let src = fs::read_to_string(f).unwrap();
        let nodes = vg_core::asm_local(&src).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        let ast = vg_core::parser::parse(&src).unwrap();
        let callee = |h: &Hash| {
            nodes
                .iter()
                .find(|a| a.hashes.node == *h)
                .map(|a| a.node.clone())
        };
        for line in src.lines().filter_map(|l| l.strip_prefix("// run: ")) {
            let (label, rest) = line.split_once(' ').expect("run line: @node, then vectors");
            let label = label.trim_start_matches('@');
            let node = &nodes
                .iter()
                .find(|a| a.label == label)
                .unwrap_or_else(|| panic!("{}: no @{label}", f.display()))
                .node;
            opcodes.extend(node.ops.iter().map(|o| (o.opcode, o.mode)));
            let names = param_names(&ast, label);
            for case in cases_of(rest, &dir) {
                cases += 1;
                traps += usize::from(case.get("trap").is_some());
                if let Err(e) = check_case(node, &names, &case, &callee) {
                    let file = f.file_name().unwrap().to_string_lossy();
                    failures.push(format!("{file} @{label} {}: {e}", case["inputs"]));
                }
            }
        }
    }
    eprintln!(
        "{cases} conformance cases ({traps} traps) over {} files and {} opcode/mode pairs; {} failures",
        files.len(),
        opcodes.len(),
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "{} failures, first 20:\n{}",
        failures.len(),
        failures[..failures.len().min(20)].join("\n")
    );
    assert!(cases > 1000, "only {cases} conformance cases");
}
