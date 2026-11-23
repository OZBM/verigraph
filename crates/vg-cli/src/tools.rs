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

/// What a module's labels and hashes resolve to beyond the module itself: vg.lock's @MESH_QUERY
/// bindings, and the project's object store, which holds those bindings, their kernels and SPECs.
pub struct Env<'a> {
    pub mesh: MeshLookup<'a>,
    pub fetch: &'a dyn Fn(&Hash) -> Option<Node>,
}

/// A module assembled from VG-Sym, with its syntax tree for names.
pub struct Loaded<'a> {
    pub nodes: Vec<Assembled>,
    defs: Vec<NodeDef>,
    fetch: &'a dyn Fn(&Hash) -> Option<Node>,
}

pub fn load<'a>(src: &str, env: &Env<'a>) -> Result<Loaded<'a>, Diagnostic> {
    let ast = parser::parse(src)?;
    let nodes = vg_core::asm_module(&ast, src, env.mesh, false)?;
    let defs = ast
        .items
        .into_iter()
        .filter_map(|i| match i {
            Item::Node(n) => Some(n),
            Item::Query(_) => None,
        })
        .collect();
    Ok(Loaded {
        nodes,
        defs,
        fetch: env.fetch,
    })
}

impl Loaded<'_> {
    /// Resolves a CALLS hash among the module's own nodes, then in the object store.
    pub fn callee(&self) -> impl Fn(&Hash) -> Option<Node> + '_ {
        |h| {
            self.nodes
                .iter()
                .find(|a| a.hashes.node == *h)
                .map(|a| a.node.clone())
                .or_else(|| (self.fetch)(h))
        }
    }

    /// A node's `intent:` attribute, without its quotes.
    pub fn intent(&self, label: &str) -> Option<String> {
        self.defs
            .iter()
            .find(|d| d.label == label)?
            .attrs
            .iter()
            .find_map(|a| match a {
                vg_core::ast::Attr::Intent(s) => Some(s.trim_matches('"').to_string()),
                _ => None,
            })
    }

    /// The labels of the module's nodes, by hash, for reports.
    fn labels(&self) -> std::collections::BTreeMap<Hash, String> {
        self.nodes
            .iter()
            .map(|a| (a.hashes.node, format!("@{}", a.label)))
            .collect()
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
pub fn check(src: &str, env: &Env) -> J {
    let m = match load(src, env) {
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

fn verdict_name(v: &vg_smt::Verdict) -> &'static str {
    match v {
        vg_smt::Verdict::Proven => "proven",
        vg_smt::Verdict::Counterexample => "counterexample",
        vg_smt::Verdict::ContractGap => "contract_gap",
        vg_smt::Verdict::Vacuous => "vacuous",
        vg_smt::Verdict::Unknown => "unknown",
        vg_smt::Verdict::Error => "error",
    }
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

/// One obligation's outcome and whether vg-interp reproduced it: vg-smt replays every model
/// (ADR 0006) and reports a counterexample only when the real callees reproduce it.
fn classify(o: &vg_smt::Outcome) -> (vg_smt::Outcome, &'static str) {
    let reproduced = if o.verdict == vg_smt::Verdict::Counterexample {
        "concrete"
    } else {
        "no"
    };
    (o.clone(), reproduced)
}

/// The docs/15 FailureReport of a failed obligation, from vg-telemetry, or the inputs alone when
/// telemetry cannot build one.
fn failure_report(
    m: &Loaded,
    a: &Assembled,
    d: &NodeDef,
    o: &vg_smt::Outcome,
    reproduced: &str,
) -> Option<J> {
    let names = vg_telemetry::Names::from_def(&a.node, d, m.labels());
    if let Some(r) = vg_telemetry::report(&a.node, &a.label, &names, o, &m.callee()) {
        return Some(r.to_json());
    }
    o.inputs.as_ref().map(|inputs| {
        json!({
            "node": format!("@{}", a.label),
            "obligation": o.id,
            "inputs": named_inputs(a, d, inputs),
            "reproduced": reproduced,
        })
    })
}

/// Verifies the nodes `which` selects; returns (all proven, docs/15 VerifyOut). `detail` follows
/// docs/15's budget: 0 attaches no report, 1 reports the first 3 failing obligations in ID order,
/// 2 reports every one.
fn verify_nodes(m: &Loaded, which: &[&Assembled], opts: &vg_smt::Options, detail: u8) -> (bool, J) {
    let callee = m.callee();
    let (mut nodes, mut results, mut all) = (vec![], vec![], true);
    let mut failed = std::collections::BTreeMap::new();
    for a in which {
        if a.node.spec {
            continue;
        }
        let Ok((_, d)) = m.find(&a.label) else {
            continue;
        };
        let mut verified = true;
        for o in vg_smt::verify(&a.node, &a.label, &callee, opts) {
            let (o, reproduced) = classify(&o);
            let verdict = verdict_name(&o.verdict);
            let mut r =
                json!({ "id": o.id, "verdict": verdict, "solver": o.solver, "message": o.message });
            if let Some(code) = o.code {
                r["code"] = json!(code);
            }
            if let Some(e) = o.evidence {
                r["evidence"] = json!(e);
            }
            if let Some(h) = &o.hash {
                r["hash"] = json!(vg_core::ir::hex(h));
            }
            r["time_ms"] = json!(o.time_ms);
            if verdict != "proven" {
                failed.insert(o.id.clone(), (*a, d, o, reproduced));
            }
            verified &= verdict == "proven";
            results.push(r);
        }
        all &= verified;
        nodes.push(json!({ "label": format!("@{}", a.label), "verified": verified }));
    }
    results.sort_by(|x, y| x["id"].as_str().cmp(&y["id"].as_str()));
    let cap = match detail {
        0 => 0,
        1 => 3,
        _ => usize::MAX,
    };
    for r in results
        .iter_mut()
        .filter(|r| r["verdict"] != "proven")
        .take(cap)
    {
        let id = r["id"].as_str().unwrap_or_default().to_string();
        if let Some((a, d, o, reproduced)) = failed.get(&id)
            && let Some(rep) = failure_report(m, a, d, o, reproduced)
        {
            r["report"] = rep;
        }
    }
    (
        all,
        json!({ "ok": all, "nodes": nodes, "results": results }),
    )
}

/// vg_explain: one obligation's FailureReport and its plain-language account (docs/15).
pub fn explain(src: &str, env: &Env, obligation: &str, timeout_ms: u64) -> J {
    let m = match load(src, env) {
        Ok(m) => m,
        Err(d) => return failed(&d),
    };
    let label = obligation.split('/').next().unwrap_or_default();
    let (a, d) = match m.find(label) {
        Ok(x) => x,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let outcomes = vg_smt::verify(&a.node, &a.label, &m.callee(), &solver_options(timeout_ms));
    let Some(o) = outcomes.iter().find(|o| o.id == obligation) else {
        return json!({ "ok": false, "error": format!("{label} has no obligation {obligation}") });
    };
    let (o, reproduced) = classify(o);
    if o.verdict == vg_smt::Verdict::Proven {
        return json!({ "ok": true, "verdict": "proven", "prose": format!("{obligation} is proven.") });
    }
    let names = vg_telemetry::Names::from_def(&a.node, d, m.labels());
    match vg_telemetry::report(&a.node, &a.label, &names, &o, &m.callee()) {
        Some(r) => json!({ "ok": true, "report": r.to_json(), "prose": vg_telemetry::prose(&r) }),
        None => json!({
            "ok": true,
            "verdict": verdict_name(&o.verdict),
            "code": o.code,
            "reproduced": reproduced,
            "prose": format!("{obligation}: {} ({}).", verdict_name(&o.verdict), o.message),
        }),
    }
}

pub fn solver_options(timeout_ms: u64) -> vg_smt::Options {
    vg_smt::Options {
        timeout_ms,
        ..vg_smt::Options::default()
    }
}

/// The solver options of a docs/15 verify `mode`: docs/05 gives a query 2 s in the edit loop and
/// 60 s in CI, and release mode needs two solvers to agree. No mode: 10 s. A timeout given wins.
pub fn mode_options(
    mode: Option<&str>,
    timeout_ms: Option<u64>,
) -> Result<vg_smt::Options, String> {
    let (default_ms, release) = match mode {
        None => (10_000, false),
        Some("edit") => (2_000, false),
        Some("ci") => (60_000, false),
        Some("release") => (60_000, true),
        Some(m) => return Err(format!("mode is edit, ci or release, not {m}")),
    };
    Ok(vg_smt::Options {
        release,
        ..solver_options(timeout_ms.unwrap_or(default_ms))
    })
}

/// vg_verify: prove every obligation of every node in the module.
pub fn verify(src: &str, env: &Env, opts: &vg_smt::Options, detail: u8) -> J {
    let m = match load(src, env) {
        Ok(m) => m,
        Err(d) => return failed(&d),
    };
    let all: Vec<&Assembled> = m.nodes.iter().collect();
    verify_nodes(&m, &all, opts, detail).1
}

/// vg_run: run a node in vg-interp. `inputs` maps parameter (and LEARNABLE) names to docs/15 values;
/// Eff parameters are supplied by the tool.
pub fn run(src: &str, env: &Env, node: &str, inputs: &Map<String, J>) -> J {
    let m = match load(src, env) {
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

/// vg_build: verify the root and everything it calls, then compile it for each target. With
/// `unverified`, a node that is not fully proven still builds, with a runtime check on every partial
/// op (exit code 4 when one fails) and no proof facts.
pub fn build(
    src: &str,
    env: &Env,
    root: &str,
    targets: &[String],
    out_dir: &Path,
    unverified: bool,
) -> J {
    let m = match load(src, env) {
        Ok(m) => m,
        Err(d) => return failed(&d),
    };
    let a = match m.find(root) {
        Ok((a, _)) => a,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let (proven, report) = verify_nodes(&m, &m.closure(a), &solver_options(10_000), 1);
    if !proven && !unverified {
        return json!({
            "ok": false,
            "error": "not every obligation is proven; fix them or build with --unverified",
            "verify": report,
        });
    }
    let callee = m.callee();
    let mut artifacts = vec![];
    for t in targets {
        let Some(target) = target(t) else {
            return json!({ "ok": false, "error": format!("unknown target {t}") });
        };
        let opts = vg_codegen::Options {
            target,
            proof_facts: proven,
            runtime_checks: !proven,
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
    json!({ "ok": true, "verified": proven, "artifacts": artifacts })
}

/// vg_lift: lift a C or Rust function through OmniLift (P8.7, docs/08, docs/15).
pub fn lift(path: &Path, function: &str, language: Option<&str>, root: &Path) -> J {
    let lang = match language {
        Some("c" | "C") => vg_omnilift::ingest::Language::C,
        Some("rust" | "rs" | "Rust") => vg_omnilift::ingest::Language::Rust,
        Some(other) => {
            return json!({
                "ok": false,
                "error": format!("unsupported language {other}; expected \"c\" or \"rust\""),
            });
        }
        None => match path.extension().and_then(|e| e.to_str()) {
            Some("c" | "h") => vg_omnilift::ingest::Language::C,
            Some("rs") => vg_omnilift::ingest::Language::Rust,
            _ => {
                return json!({
                    "ok": false,
                    "error": format!(
                        "cannot infer language for {}; pass --lang c or --lang rust",
                        path.display()
                    ),
                });
            }
        },
    };
    let lifted = match vg_omnilift::lift::lift_source(path, function, lang, &[]) {
        Ok(l) => l,
        Err(d) => return failed(&d),
    };
    if (root != Path::new(".") || root.join(".vg").is_dir())
        && let Ok(store) = vg_store::Store::open(root)
    {
        let lock_path = root.join("vg.lock");
        let mut lock = vg_store::Lock::read(&lock_path).unwrap_or_default();
        for a in &lifted.nodes {
            let _ = store.put(&a.bytes);
            lock.resolved.insert(
                a.label.clone(),
                vg_store::Entry {
                    node: a.hashes.node,
                    spec: a.node.spec.then_some(a.hashes.node),
                    source: "lift".into(),
                },
            );
        }
        let _ = lock.write(&lock_path);
    }
    let root_params = lifted
        .nodes
        .last()
        .map(|a| a.node.params.as_slice())
        .unwrap_or(&[]);
    let ub_found: Vec<J> = lifted
        .ub_found
        .iter()
        .map(|u| {
            let input: Vec<J> = u
                .input
                .iter()
                .enumerate()
                .map(|(i, v)| match root_params.get(i) {
                    Some(t) => json::out(v, t),
                    None => match v {
                        Value::Int(n) => i64::try_from(*n)
                            .map(|x| json!(x))
                            .unwrap_or_else(|_| json!(n.to_string())),
                        Value::Bool(b) => json!(b),
                        other => json!(format!("{other:?}")),
                    },
                })
                .collect();
            json!({ "kind": u.kind, "input": input })
        })
        .collect();
    json!({
        "ok": true,
        "node_hash": hex(&lifted.node_hash),
        "root_label": format!("@{}", lifted.root_label),
        "vgsym": lifted.vgsym,
        "report": {
            "bounded_check": lifted.bounded_check,
            "unroll": lifted.unroll,
            "fuzz_cases": lifted.fuzz_cases,
            "ub_found": ub_found,
        },
        "provenance": {
            "source": format!("{}::{}", lifted.provenance.path, lifted.provenance.function),
            "language": lifted.provenance.language.as_str(),
            "compiler": lifted.provenance.compiler,
            "flags": lifted.provenance.compiler_args.join(" "),
        },
    })
}
