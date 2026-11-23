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

    /// The node and every node it calls, transitively, each once, in module order.
    fn closure(&self, root: &Assembled) -> Vec<&Assembled> {
        let mut want = vec![root.hashes.node];
        let mut k = 0;
        while k < want.len() {
            if let Some(a) = self.nodes.iter().find(|a| a.hashes.node == want[k]) {
                for c in &a.node.calls {
                    if !want.contains(c) {
                        want.push(*c);
                    }
                }
            }
            k += 1;
        }
        self.nodes
            .iter()
            .filter(|a| want.contains(&a.hashes.node))
            .collect()
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

/// Concrete replay of a counterexample with the real callees (ADR 0006): Some(true) when the inputs
/// satisfy PRE and the node traps or breaks its POST, Some(false) when it runs correctly (a contract
/// gap), None when it cannot be replayed here (effects) or the inputs break PRE (a verifier bug).
fn replay(a: &Assembled, inputs: &[Value], m: &Loaded) -> Option<bool> {
    if !a.node.effects.is_pure() {
        return None;
    }
    let callee = m.callee();
    let pre = vg_interp::holds(&a.node, vg_interp::Clause::Pre, inputs, &[], &callee).ok()?;
    if !pre {
        return None;
    }
    let mut host = vg_interp::NoEffectsHost;
    Some(match vg_interp::run(&a.node, inputs, &callee, &mut host) {
        Err(_) => true,
        Ok(results) => {
            vg_interp::holds(&a.node, vg_interp::Clause::Post, inputs, &results, &callee)
                != Ok(true)
        }
    })
}

fn named_inputs(a: &Assembled, d: &NodeDef, inputs: &[Value]) -> J {
    let names = d
        .params
        .iter()
        .map(|p| p.name.text.clone())
        .chain(d.learnable.iter().flatten().map(|l| l.name.text.clone()));
    let mut o = Map::new();
    for ((name, v), i) in names.zip(inputs).zip(0u32..) {
        if let Some(t) = a.node.input_type(i) {
            o.insert(name, json::out(v, t));
        }
    }
    J::Object(o)
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

/// vg_build: verify the root and everything it calls, then compile it for each target.
pub fn build(
    _src: &str,
    _mesh: MeshLookup,
    _root: &str,
    _targets: &[String],
    _out_dir: &Path,
    _unverified: bool,
) -> J {
    json!({ "ok": false, "error": "not available yet: vg-codegen is not merged" })
}
