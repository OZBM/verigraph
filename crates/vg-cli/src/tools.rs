//! The operations behind `vg check`, `vg verify`, `vg run` and `vg build`, and the MCP tools of the
//! same names (docs/15). Each returns its docs/15 result as JSON, so the CLI and `vg mcp` agree.

use std::path::Path;

use serde_json::{Map, Value as J, json};
use vg_core::ast::{Item, NodeDef};
use vg_core::diag::{Diagnostic, Severity};
use vg_core::ir::{Hash, Node, hex};
use vg_core::value::Value;
use vg_core::{Assembled, MeshLookup, parser};

use crate::json;

/// A module assembled from VG-Sym, with its syntax tree for names.
pub struct Loaded {
    pub nodes: Vec<Assembled>,
    defs: Vec<NodeDef>,
}

pub fn load(src: &str, mesh: MeshLookup) -> Result<Loaded, Diagnostic> {
    let ast = parser::parse(src)?;
    let nodes = vg_core::asm_module(&ast, src, mesh, false)?;
    let defs = ast
        .items
        .into_iter()
        .filter_map(|i| match i {
            Item::Node(n) => Some(n),
            Item::Query(_) => None,
        })
        .collect();
    Ok(Loaded { nodes, defs })
}

impl Loaded {
    /// Resolves a CALLS hash among the module's own nodes.
    pub fn callee(&self) -> impl Fn(&Hash) -> Option<Node> + '_ {
        |h| {
            self.nodes
                .iter()
                .find(|a| a.hashes.node == *h)
                .map(|a| a.node.clone())
        }
    }

    fn find(&self, label: &str) -> Result<(&Assembled, &NodeDef), String> {
        let label = label.trim_start_matches('@');
        let a = self.nodes.iter().find(|a| a.label == label);
        let d = self.defs.iter().find(|d| d.label == label);
        a.zip(d)
            .ok_or_else(|| format!("no node @{label} in this module"))
    }
}

pub fn diag(d: &Diagnostic) -> J {
    json!({
        "code": d.code,
        "severity": if d.severity == Severity::Error { "error" } else { "warning" },
        "node": d.node.as_ref().map(|n| format!("@{}", n.trim_start_matches('@'))),
        "op": d.op,
        "line": d.span.line,
        "col": d.span.col,
        "message": d.message,
    })
}

fn failed(d: &Diagnostic) -> J {
    json!({ "ok": false, "diagnostics": [diag(d)] })
}

/// vg_check: parse, validate, canonicalize, type- and effect-check every node.
pub fn check(src: &str, mesh: MeshLookup) -> J {
    let m = match load(src, mesh) {
        Ok(m) => m,
        Err(d) => return failed(&d),
    };
    let callee = m.callee();
    let (mut nodes, mut diags) = (vec![], vec![]);
    for a in &m.nodes {
        diags.extend(a.warnings.iter().map(diag));
        match vg_types::check_node(&a.node, &callee) {
            Ok(s) => nodes.push(json!({
                "label": format!("@{}", a.label),
                "kind": if a.node.spec { "spec" } else { "node" },
                "node_hash": hex(&a.hashes.node),
                "effects": s.effects.names(),
                "cost_ops": s.cost_ops,
            })),
            Err(ds) => diags.extend(ds.into_iter().map(|d| diag(&d.at_node(&a.label)))),
        }
    }
    let ok = !diags.iter().any(|d| d["severity"] == "error");
    json!({ "ok": ok, "nodes": nodes, "diagnostics": diags })
}

// shortcut: vg-smt and vg-codegen are not merged yet; verify and build answer "not available" until
// they land (the full versions are on branch w5-cli).
const PENDING: &str = "not available yet: vg-smt is not merged";

/// vg_verify: prove every obligation of every node in the module.
pub fn verify(_src: &str, _mesh: MeshLookup, _timeout_ms: u64) -> J {
    json!({ "ok": false, "error": PENDING })
}

/// vg_run: run a node in vg-interp. `inputs` maps parameter (and LEARNABLE) names to docs/15 values;
/// Eff parameters are supplied by the tool.
pub fn run(src: &str, mesh: MeshLookup, node: &str, inputs: &Map<String, J>) -> J {
    let m = match load(src, mesh) {
        Ok(m) => m,
        Err(d) => return failed(&d),
    };
    let (a, d) = match m.find(node) {
        Ok(x) => x,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let names: Vec<String> = d
        .params
        .iter()
        .map(|p| p.name.text.clone())
        .chain(d.learnable.iter().flatten().map(|l| l.name.text.clone()))
        .collect();
    let mut values = vec![];
    for (i, name) in names.iter().enumerate() {
        let t = a.node.input_type(i as u32).expect("one type per input");
        let v = match (t, inputs.get(name)) {
            (vg_core::types::Type::Eff, _) => Ok(Value::Eff),
            (t, Some(j)) => json::value(j, t),
            (_, None) => Err(format!("missing input {name}")),
        };
        match v {
            Ok(v) => values.push(v),
            Err(e) => return json!({ "ok": false, "error": format!("{name}: {e}") }),
        }
    }
    let callee = m.callee();
    let pre = vg_interp::holds(&a.node, vg_interp::Clause::Pre, &values, &[], &callee);
    let mut host = vg_interp::NoEffectsHost;
    match vg_interp::run(&a.node, &values, &callee, &mut host) {
        Ok(results) => {
            let post =
                vg_interp::holds(&a.node, vg_interp::Clause::Post, &values, &results, &callee);
            let outputs: Vec<J> = results
                .iter()
                .zip(&a.node.results)
                .filter(|(_, t)| **t != vg_core::types::Type::Eff)
                .map(|(v, t)| json::out(v, t))
                .collect();
            json!({ "ok": true, "outputs": outputs, "pre": pre.ok(), "post": post.ok() })
        }
        Err(t) => json!({
            "ok": false,
            "pre": pre.ok(),
            "trap": { "op": t.op, "opcode": t.opcode, "message": t.message },
        }),
    }
}

fn target(name: &str) -> Option<vg_codegen::Target> {
    Some(match name {
        "aarch64-apple-darwin" => vg_codegen::Target::Aarch64Darwin,
        "x86_64-unknown-linux-gnu" => vg_codegen::Target::X86_64Linux,
        "wasm32-wasip1" => vg_codegen::Target::Wasm32Wasi,
        _ => return None,
    })
}

/// vg_build: compile a root node for each target. Until vg-smt lands, only `unverified` builds are
/// possible: no proof facts, and a runtime check on every partial op (exit code 4 when one fails).
pub fn build(
    src: &str,
    mesh: MeshLookup,
    root: &str,
    targets: &[String],
    out_dir: &Path,
    unverified: bool,
) -> J {
    if !unverified {
        return json!({
            "ok": false,
            "error": "vg verify is not available yet, so only --unverified builds are possible",
        });
    }
    let m = match load(src, mesh) {
        Ok(m) => m,
        Err(d) => return failed(&d),
    };
    let a = match m.find(root) {
        Ok((a, _)) => a,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let callee = m.callee();
    let mut artifacts = vec![];
    for t in targets {
        let Some(target) = target(t) else {
            return json!({ "ok": false, "error": format!("unknown target {t}") });
        };
        let opts = vg_codegen::Options {
            target,
            proof_facts: false,
            runtime_checks: true,
        };
        let out = out_dir.join(format!("{}-{t}", a.label));
        match vg_codegen::build(&a.node, &callee, &opts, &out) {
            Ok(r) => {
                let bytes = std::fs::read(&r.artifact).unwrap_or_default();
                artifacts.push(json!({
                    "target": t,
                    "path": r.artifact.display().to_string(),
                    "bytes": bytes.len(),
                    "blake3": blake3::hash(&bytes).to_hex().to_string(),
                    "linked": r.linked,
                    "residual_guards": r.residual_guards,
                }));
            }
            Err(d) => return json!({ "ok": false, "diagnostics": [diag(&d)] }),
        }
    }
    json!({ "ok": true, "verified": false, "artifacts": artifacts })
}
