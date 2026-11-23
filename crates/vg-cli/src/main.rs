//! The `vg` command: `vg asm`, `vg disasm`, `vg hash` (P1.10); `vg check`, `vg verify`, `vg run`,
//! `vg build`, `vg explain`, `vg mesh` and `vg mcp`, whose results are the docs/15 JSON of the MCP
//! tools of the same names.

mod json;
mod mcp;
mod mesh;
mod tools;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vg_core::diag::Diagnostic;
use vg_core::ir::{Hash, hex, parse_hex};
use vg_core::{Assembled, codec, disasm, printer};
use vg_store::{Entry, Lock, Store};

const USAGE: &str = "usage:
  vg asm FILE.vgs [--root DIR] [--keep-debug]   assemble into DIR/.vg/objects and pin labels in DIR/vg.lock
  vg disasm (HASH | FILE.vgir) [--root DIR]     print VG-Sym for a node and everything it calls
  vg hash FILE...                               print node, body and contract hashes (.vgs or .vgir)
  vg check FILE.vgs                             parse, canonicalize, type- and effect-check (JSON)
  vg verify FILE.vgs [--mode edit|ci|release] [--timeout MS] [--detail 0|1|2]
                                                prove every obligation; release needs two solvers (JSON)
  vg explain FILE.vgs OBLIGATION [--timeout MS] the failure report of one obligation, and its prose
  vg run FILE.vgs @NODE JSON                    run a node in vg-interp; JSON maps parameter names to values
  vg build FILE.vgs @NODE [--target TRIPLE]... [-o DIR] [--unverified]
                                                verify, then compile (default target: aarch64-apple-darwin)
  vg lift FILE FUNCTION [--lang c|rust] [--root DIR]
                                                lift a C or Rust function with Alive2 and fuzzing (JSON)
  vg mesh publish FILE.vgs [--root DIR]         publish the module's nodes to DIR/.vg/mesh (ingestion gate)
  vg mesh query QUERY.json [--root DIR]         candidates for a docs/09 MeshContractQuery
  vg mesh resolve FILE.vgs [--root DIR]         bind each @MESH_QUERY, pin it in vg.lock, fetch its nodes
  vg mesh serve [--root DIR] [--addr HOST:PORT] the registry's HTTP API (default 127.0.0.1:7878)
  vg mcp                                        serve the eight docs/15 tools over MCP stdio";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fail::Usage(m)) => {
            eprintln!("vg: {m}\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Fail::Error(m)) => {
            if !m.is_empty() {
                eprintln!("{m}");
            }
            ExitCode::from(1)
        }
    }
}

enum Fail {
    Usage(String),
    Error(String),
}

fn error(m: impl ToString) -> Fail {
    Fail::Error(m.to_string())
}

/// `FILE:line:col: error VG-…`, as compilers print it.
fn located(path: &Path, d: &Diagnostic) -> String {
    format!("{}:{d}", path.display())
}

struct Opts {
    files: Vec<String>,
    root: PathBuf,
    keep_debug: bool,
    timeout_ms: Option<u64>,
    mode: Option<String>,
    targets: Vec<String>,
    out: PathBuf,
    unverified: bool,
    detail: u8,
    addr: String,
    lang: Option<String>,
}

fn opts(args: &[String]) -> Result<Opts, Fail> {
    let mut o = Opts {
        files: vec![],
        root: PathBuf::from("."),
        keep_debug: false,
        timeout_ms: None,
        mode: None,
        targets: vec![],
        out: PathBuf::from("target/vg"),
        unverified: false,
        detail: 1,
        addr: "127.0.0.1:7878".into(),
        lang: None,
    };
    let value = |it: &mut std::slice::Iter<String>, what: &str| {
        it.next()
            .cloned()
            .ok_or_else(|| Fail::Usage(format!("{what} needs a value")))
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--root" => {
                o.root = it
                    .next()
                    .ok_or_else(|| Fail::Usage("--root needs a directory".into()))?
                    .into()
            }
            "--keep-debug" => o.keep_debug = true,
            "--timeout" => {
                o.timeout_ms = Some(
                    value(&mut it, "--timeout")?
                        .parse()
                        .map_err(|_| Fail::Usage("--timeout takes milliseconds".into()))?,
                )
            }
            "--mode" => o.mode = Some(value(&mut it, "--mode")?),
            "--target" => o.targets.push(value(&mut it, "--target")?),
            "-o" => o.out = value(&mut it, "-o")?.into(),
            "--unverified" => o.unverified = true,
            "--detail" => {
                o.detail = match value(&mut it, "--detail")?.as_str() {
                    "0" => 0,
                    "1" => 1,
                    "2" => 2,
                    _ => return Err(Fail::Usage("--detail takes 0, 1 or 2".into())),
                }
            }
            "--addr" => o.addr = value(&mut it, "--addr")?,
            "--lang" | "--language" => o.lang = Some(value(&mut it, "--lang")?),
            s if s.starts_with("--") => return Err(Fail::Usage(format!("unknown option {s}"))),
            s => o.files.push(s.to_string()),
        }
    }
    Ok(o)
}

fn read(path: &Path) -> Result<Vec<u8>, Fail> {
    std::fs::read(path).map_err(|e| error(format!("{}: {e}", path.display())))
}

/// Assembles a module, resolving @MESH_QUERY labels through `lock`.
fn assemble(path: &Path, lock: &Lock, keep_debug: bool) -> Result<Vec<Assembled>, Fail> {
    let bytes = read(path)?;
    let src =
        String::from_utf8(bytes).map_err(|_| error(format!("{}: not UTF-8", path.display())))?;
    let mesh = |label: &str| {
        lock.resolved
            .get(label)
            .filter(|e| e.source != "local")
            .map(|e| (e.node, e.spec))
    };
    let out = vg_core::asm(&src, &mesh, keep_debug).map_err(|d| error(located(path, &d)))?;
    for a in &out {
        for w in &a.warnings {
            eprintln!("{}", located(path, w));
        }
    }
    Ok(out)
}

fn run(args: &[String]) -> Result<(), Fail> {
    let Some((cmd, rest)) = args.split_first() else {
        return Err(Fail::Usage("no command".into()));
    };
    let o = opts(rest)?;
    match cmd.as_str() {
        "asm" => {
            let [file] = o.files.as_slice() else {
                return Err(Fail::Usage("asm takes one file".into()));
            };
            let lock_path = o.root.join("vg.lock");
            let mut lock = Lock::read(&lock_path).map_err(error)?;
            let out = assemble(Path::new(file), &lock, o.keep_debug)?;
            let store = Store::open(&o.root).map_err(error)?;
            for a in &out {
                store.put(&a.bytes).map_err(error)?;
                lock.resolved.insert(
                    a.label.clone(),
                    Entry {
                        node: a.hashes.node,
                        spec: a.node.spec.then_some(a.hashes.node),
                        source: "local".into(),
                    },
                );
                println!("@{} {}", a.label, hex(&a.hashes.node));
            }
            lock.write(&lock_path).map_err(error)
        }
        "disasm" => {
            let [what] = o.files.as_slice() else {
                return Err(Fail::Usage("disasm takes one hash or file".into()));
            };
            let store = Store::open(&o.root).map_err(error)?;
            let (root, extra): (Hash, Option<vg_core::ir::Node>) = match parse_hex(what) {
                Some(h) => (h, None),
                None => {
                    let path = Path::new(what);
                    let node = codec::decode(&read(path)?).map_err(|d| error(located(path, &d)))?;
                    let h = codec::encode(&node).1.node;
                    (h, Some(node))
                }
            };
            let get = |h: &Hash| -> Option<vg_core::ir::Node> {
                if *h == root
                    && let Some(n) = &extra
                {
                    return Some(n.clone());
                }
                store.get(h).ok().flatten()
            };
            let m = disasm::disasm("disasm", &[root], &get)
                .map_err(|d| error(format!("{}: {}", d.code, d.message)))?;
            print!("{}", printer::print(&m));
            Ok(())
        }
        "hash" => {
            if o.files.is_empty() {
                return Err(Fail::Usage("hash takes at least one file".into()));
            }
            let lock = Lock::read(&o.root.join("vg.lock")).map_err(error)?;
            let mut failed = None;
            for f in &o.files {
                let path = Path::new(f);
                let rows: Result<Vec<(String, codec::Hashes)>, Fail> = if f.ends_with(".vgir") {
                    read(path).and_then(|b| {
                        let node = codec::decode(&b).map_err(|d| error(located(path, &d)))?;
                        Ok(vec![(String::new(), codec::encode(&node).1)])
                    })
                } else {
                    assemble(path, &lock, false).map(|v| {
                        v.into_iter()
                            .map(|a| (format!(" @{}", a.label), a.hashes))
                            .collect()
                    })
                };
                match rows {
                    Ok(rows) => {
                        for (label, h) in rows {
                            println!(
                                "{f}{label} node {} body {} contract {}",
                                hex(&h.node),
                                hex(&h.body),
                                hex(&h.contract)
                            );
                        }
                    }
                    Err(Fail::Error(m)) => {
                        eprintln!("{m}");
                        failed = Some(Fail::Error(format!("vg hash: {f} failed")));
                    }
                    Err(u) => return Err(u),
                }
            }
            failed.map_or(Ok(()), Err)
        }
        "check" | "verify" | "run" | "build" | "explain" | "mesh" => {
            if cmd == "mesh" && o.files.first().map(String::as_str) == Some("serve") {
                return mesh::serve(&o.root, &o.addr).map_err(error);
            }
            let (cmd, files) = if cmd == "mesh" {
                match o.files.split_first() {
                    Some((sub, rest)) => (format!("mesh {sub}"), rest.to_vec()),
                    None => {
                        return Err(Fail::Usage(
                            "mesh takes publish, query, resolve or serve".into(),
                        ));
                    }
                }
            } else {
                (cmd.to_string(), o.files.clone())
            };
            let Some((file, rest)) = files.split_first() else {
                return Err(Fail::Usage(format!("{cmd} takes a file")));
            };
            let path = Path::new(file);
            let src = String::from_utf8(read(path)?)
                .map_err(|_| error(format!("{}: not UTF-8", path.display())))?;
            if cmd == "mesh query" {
                let q: serde_json::Value = serde_json::from_str(&src)
                    .map_err(|e| Fail::Usage(format!("{file} is not JSON: {e}")))?;
                let out = mesh::query(&q, &o.root);
                println!("{}", serde_json::to_string_pretty(&out).expect("JSON"));
                return if out["ok"] == true {
                    Ok(())
                } else {
                    Err(Fail::Error(String::new()))
                };
            }
            let lock = Lock::read(&o.root.join("vg.lock")).map_err(error)?;
            let mesh = |label: &str| {
                lock.resolved
                    .get(label)
                    .filter(|e| e.source != "local")
                    .map(|e| (e.node, e.spec))
            };
            // Only a project that has a store is read; vg check never creates one.
            let store = if o.root.join(".vg").is_dir() {
                Some(Store::open(&o.root).map_err(error)?)
            } else {
                None
            };
            let fetch = |h: &Hash| store.as_ref().and_then(|s| s.get(h).ok().flatten());
            let env = tools::Env {
                mesh: &mesh,
                fetch: &fetch,
            };
            let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
            let out = match (cmd.as_str(), rest.as_slice()) {
                ("check", []) => tools::check(&src, &env),
                ("verify", []) => {
                    let smt = tools::mode_options(o.mode.as_deref(), o.timeout_ms)
                        .map_err(Fail::Usage)?;
                    tools::verify(&src, &env, &smt, o.detail)
                }
                ("explain", [ob]) => tools::explain(&src, &env, ob, o.timeout_ms.unwrap_or(10_000)),
                ("mesh publish", []) => mesh::publish(&src, &env, &o.root),
                ("mesh resolve", []) => mesh::resolve(&src, &o.root),
                ("run", [node, inputs]) => {
                    let j: serde_json::Value = serde_json::from_str(inputs)
                        .map_err(|e| Fail::Usage(format!("inputs are not JSON: {e}")))?;
                    let map = j
                        .as_object()
                        .ok_or_else(|| Fail::Usage("inputs are a JSON object".into()))?;
                    tools::run(&src, &env, node, map)
                }
                ("build", [node]) => {
                    let targets = if o.targets.is_empty() {
                        vec!["aarch64-apple-darwin".to_string()]
                    } else {
                        o.targets.clone()
                    };
                    tools::build(&src, &env, node, &targets, &o.out, o.unverified)
                }
                _ => return Err(Fail::Usage(format!("wrong arguments for {cmd}"))),
            };
            println!("{}", serde_json::to_string_pretty(&out).expect("JSON"));
            if out["ok"] == true {
                Ok(())
            } else {
                Err(Fail::Error(String::new()))
            }
        }
        "lift" => {
            let [file, function] = o.files.as_slice() else {
                return Err(Fail::Usage("lift takes FILE and FUNCTION".into()));
            };
            let out = tools::lift(Path::new(file), function, o.lang.as_deref(), &o.root);
            println!("{}", serde_json::to_string_pretty(&out).expect("JSON"));
            if out["ok"] == true {
                Ok(())
            } else {
                Err(Fail::Error(String::new()))
            }
        }
        "mcp" => mcp::serve().map_err(error),
        c => Err(Fail::Usage(format!("unknown command {c}"))),
    }
}
