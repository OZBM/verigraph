//! `vg mesh publish|query|resolve|serve` and the vg_mesh_publish and vg_mesh_query MCP tools: the
//! project's local registry at `<root>/.vg/mesh` (docs/09, P7.5).

use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use serde_json::{Value as J, json};
use vg_core::ast::{BinOp, CallPrefix, FKind, Item, MeshQuery};
use vg_core::codec;
use vg_core::ir::{Hash, hex};
use vg_mesh::{Mesh, MeshContractQuery};
use vg_store::{Entry, Lock, Store};

/// The name submissions from this project are signed under.
const SIGNER: &str = "local";

pub fn dir(root: &Path) -> PathBuf {
    root.join(".vg/mesh")
}

/// The project's signing key, created from the operating system's generator on first use.
fn signing_key(root: &Path) -> Result<SigningKey, String> {
    let path = dir(root).join("signer.key");
    let seed = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            use std::io::Read;
            let mut seed = vec![0u8; 32];
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut seed))
                .map_err(|e| format!("/dev/urandom: {e}"))?;
            std::fs::create_dir_all(dir(root)).map_err(|e| format!("{}: {e}", path.display()))?;
            std::fs::write(&path, &seed).map_err(|e| format!("{}: {e}", path.display()))?;
            seed
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let seed: [u8; 32] = seed
        .try_into()
        .map_err(|_| format!("{} must hold 32 bytes", path.display()))?;
    Ok(SigningKey::from_bytes(&seed))
}

fn open(root: &Path) -> Result<Mesh, String> {
    let mut m = Mesh::open(&dir(root)).map_err(|e| e.to_string())?;
    m.set_smt_options(crate::tools::solver_options(60_000));
    Ok(m)
}

/// vg_mesh_publish: every NODE and SPEC of the module, in module order (callees come first,
/// ADR 0005), through the six-check ingestion gate. Returns docs/15 PublishOut.
pub fn publish(src: &str, env: &crate::tools::Env, root: &Path) -> J {
    let m = match crate::tools::load(src, env) {
        Ok(m) => m,
        Err(d) => return json!({ "ok": false, "diagnostics": [crate::tools::diag(&d)] }),
    };
    let (mut mesh, sk) = match open(root).and_then(|mesh| Ok((mesh, signing_key(root)?))) {
        Ok(x) => x,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let (mut accepted, mut rejected) = (vec![], vec![]);
    for a in &m.nodes {
        let mut sub = vg_mesh::sign_submission(a.bytes.clone(), SIGNER, &sk);
        sub.label = Some(a.label.clone());
        sub.intent = m.intent(&a.label);
        if a.node.spec {
            sub.spec_name = Some(a.label.clone());
        }
        let out = mesh.publish(sub);
        accepted.extend(out.accepted.iter().map(|h| json!(hex(h))));
        rejected.extend(out.rejected.iter().map(|r| {
            let mut j = r.to_json();
            j["node"] = json!(format!("@{}", a.label));
            j
        }));
    }
    json!({ "ok": rejected.is_empty(), "accepted": accepted, "rejected": rejected })
}

/// vg_mesh_query: a docs/09 MeshContractQuery against the local registry. Returns MeshQueryOut.
pub fn query(q: &J, root: &Path) -> J {
    let parsed = match MeshContractQuery::from_json(q) {
        Ok(p) => p,
        Err(e) => return json!({ "ok": false, "error": e.to_string() }),
    };
    let limit = q.get("limit").and_then(J::as_u64).unwrap_or(10) as usize;
    let mesh = match open(root) {
        Ok(m) => m,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    match mesh.query(&parsed, limit) {
        Ok(cands) => json!({
            "ok": true,
            "candidates": cands.iter().map(|c| c.to_json()).collect::<Vec<_>>(),
        }),
        Err(e) => json!({ "ok": false, "error": e.to_string() }),
    }
}

/// The spec class a query names: `post: result == spec:X(arg0, …, argn)` with X the query's own
/// label asks for any member of the class named X (docs/07, "Mesh first").
fn class_of(q: &MeshQuery) -> Option<String> {
    let FKind::Binary(BinOp::Eq, l, r) = &q.post.kind else {
        return None;
    };
    let (FKind::Result, FKind::Call { prefix, name, args }) = (&l.kind, &r.kind) else {
        return None;
    };
    let in_order = args.len() == q.params.len()
        && args
            .iter()
            .enumerate()
            .all(|(i, a)| matches!(&a.kind, FKind::Value(vg_core::ast::Value::Ident(n)) if n.text == format!("arg{i}")));
    (*prefix == Some(CallPrefix::Spec) && name.text == q.label && in_order && q.pre.is_none())
        .then(|| q.label.clone())
}

fn text_at(src: &str, span: vg_core::diag::Span) -> String {
    src.chars()
        .skip(span.offset as usize)
        .take(span.len as usize)
        .collect()
}

/// `vg mesh resolve`: binds each @MESH_QUERY of the module to the best registry candidate, pins
/// it in vg.lock (source "mesh") and copies it, its callees and its SPEC into the project store,
/// where vg verify, run and build find them.
pub fn resolve(src: &str, root: &Path) -> J {
    let ast = match vg_core::parser::parse(src) {
        Ok(a) => a,
        Err(d) => return json!({ "ok": false, "diagnostics": [crate::tools::diag(&d)] }),
    };
    let mesh = match open(root) {
        Ok(m) => m,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let lock_path = root.join("vg.lock");
    let mut lock = match Lock::read(&lock_path) {
        Ok(l) => l,
        Err(e) => return json!({ "ok": false, "error": e.to_string() }),
    };
    let store = match Store::open(root) {
        Ok(s) => s,
        Err(e) => return json!({ "ok": false, "error": e.to_string() }),
    };
    let (mut bound, mut unbound) = (vec![], vec![]);
    for item in &ast.items {
        let Item::Query(q) = item else { continue };
        let class = class_of(q);
        let query = json!({
            "input_types": q.params.iter().map(vg_core::printer::ty).collect::<Vec<_>>(),
            "output_types": q.results.iter().map(vg_core::printer::ty).collect::<Vec<_>>(),
            "max_effects": q.effects.iter().map(|e| e.text.clone()).collect::<Vec<_>>(),
            "intent": q.intent.as_deref().map(|s| s.trim_matches('"')),
            "spec": class,
            // A class query is answered by class membership; any other post by the behavioral
            // check against the query's own contract.
            "pre": if class.is_some() { None } else { q.pre.as_ref().map(|f| text_at(src, f.span)) },
            "post": if class.is_some() { None } else { Some(text_at(src, q.post.span)) },
        });
        let parsed = match MeshContractQuery::from_json(&query) {
            Ok(p) => p,
            Err(e) => return json!({ "ok": false, "error": e.to_string() }),
        };
        let cands = match mesh.query(&parsed, 1) {
            Ok(c) => c,
            Err(e) => return json!({ "ok": false, "error": e.to_string() }),
        };
        let Some(c) = cands.first() else {
            unbound.push(json!({ "label": format!("@{}", q.label), "query": query }));
            continue;
        };
        if class.is_some() && c.spec_hash.is_none() {
            unbound.push(json!({ "label": format!("@{}", q.label), "query": query }));
            continue;
        }
        let mut want: Vec<Hash> = vec![c.node_hash];
        want.extend(c.spec_hash);
        let mut k = 0;
        while k < want.len() {
            match mesh.get_node(&want[k]) {
                Ok(Some(n)) => {
                    for h in &n.calls {
                        if !want.contains(h) {
                            want.push(*h);
                        }
                    }
                    if let Err(e) = store.put(&codec::encode(&n).0) {
                        return json!({ "ok": false, "error": e.to_string() });
                    }
                }
                _ => {
                    return json!({ "ok": false, "error": format!("registry lacks {}", hex(&want[k])) });
                }
            }
            k += 1;
        }
        lock.resolved.insert(
            q.label.clone(),
            Entry {
                node: c.node_hash,
                spec: c.spec_hash,
                source: "mesh".into(),
            },
        );
        bound.push(json!({
            "label": format!("@{}", q.label),
            "node_hash": hex(&c.node_hash),
            "spec_hash": c.spec_hash.map(|h| hex(&h)),
            "evidence": c.evidence,
            "objects": want.len(),
        }));
    }
    if let Err(e) = lock.write(&lock_path) {
        return json!({ "ok": false, "error": e.to_string() });
    }
    json!({ "ok": unbound.is_empty(), "bound": bound, "unbound": unbound })
}

/// `vg mesh serve`: the registry's HTTP API (docs/09) on `addr`.
pub fn serve(root: &Path, addr: &str) -> Result<(), String> {
    let mesh = open(root)?;
    let listener = std::net::TcpListener::bind(addr).map_err(|e| format!("{addr}: {e}"))?;
    eprintln!("vg mesh: serving {} on http://{addr}", dir(root).display());
    vg_mesh::serve(mesh, listener).map_err(|e| e.to_string())
}
