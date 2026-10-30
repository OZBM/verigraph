//! The `vg` command (P1.10): `vg asm`, `vg disasm`, `vg hash`. `vg mcp` and the rest arrive with
//! their phases.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use vg_core::diag::Diagnostic;
use vg_core::ir::{Hash, hex, parse_hex};
use vg_core::{Assembled, codec, disasm, printer};
use vg_store::{Entry, Lock, Store};

const USAGE: &str = "usage:
  vg asm FILE.vgs [--root DIR] [--keep-debug]   assemble into DIR/.vg/objects and pin labels in DIR/vg.lock
  vg disasm (HASH | FILE.vgir) [--root DIR]     print VG-Sym for a node and everything it calls
  vg hash FILE...                               print node, body and contract hashes (.vgs or .vgir)";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Fail::Usage(m)) => {
            eprintln!("vg: {m}\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Fail::Error(m)) => {
            eprintln!("{m}");
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
}

fn opts(args: &[String]) -> Result<Opts, Fail> {
    let mut o = Opts {
        files: vec![],
        root: PathBuf::from("."),
        keep_debug: false,
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
        c => Err(Fail::Usage(format!("unknown command {c}"))),
    }
}
