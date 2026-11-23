//! P4.7 (docs/16, "Differential testing"): builds agree with vg-interp, byte for byte, on generated
//! programs: aarch64-apple-darwin natively and wasm32-wasip1 under wasmtime (when its toolchain and
//! wasmtime are installed), each with runtime checks and, for programs vg-smt proves, again with
//! proof facts and no checks, where a disagreement means a wrong fact reached LLVM. The
//! x86_64-unknown-linux-gnu build links here but runs only on a Linux host. VG_DIFF_PROGRAMS sets
//! the number of programs (default 40; the G4 gate runs 1,000) and VG_DIFF_INPUTS the inputs per
//! program (default 20; the gate runs 100).

// The native builds are aarch64-apple-darwin executables: only an Apple Silicon host runs them.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use vg_core::generate::{Class, Rng, program};
use vg_core::ir::{Hash, Node};
use vg_core::types::Type;
use vg_core::value::Value;

const SEED: u64 = 0xD1FF_2026_1123;

fn env_count(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Boundary values first (docs/16, "Interpreter conformance"), then seeded random ones.
fn boundary(t: &Type) -> Vec<Value> {
    match t {
        Type::Bool => vec![Value::Bool(false), Value::Bool(true)],
        Type::UInt(w) => {
            let max = (1i128 << w) - 1;
            [0, 1, max, max - 1, 1 << (w / 2)].map(Value::Int).to_vec()
        }
        Type::SInt(w) => {
            let (min, max) = (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1);
            [0, 1, -1, max, max - 1, min, min + 1, 1 << (w / 2)]
                .map(Value::Int)
                .to_vec()
        }
        Type::Float(32) => [
            0.0f32,
            -0.0,
            1.0,
            -1.0,
            0.1,
            f32::MAX,
            -f32::MAX,
            f32::MIN_POSITIVE,
            1e-45,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
        ]
        .map(|f| {
            Value::Float(if f.is_nan() {
                0x7FC0_0000
            } else {
                u64::from(f.to_bits())
            })
        })
        .to_vec(),
        Type::Float(64) => [
            0.0f64,
            -0.0,
            1.0,
            -1.0,
            0.1,
            f64::MAX,
            -f64::MAX,
            f64::MIN_POSITIVE,
            5e-324,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
        ]
        .map(|f| {
            Value::Float(if f.is_nan() {
                0x7FF8_0000_0000_0000
            } else {
                f.to_bits()
            })
        })
        .to_vec(),
        _ => vec![],
    }
}

fn random(t: &Type, rng: &mut Rng) -> Value {
    match t {
        Type::Bool => Value::Bool(rng.chance(1, 2)),
        Type::UInt(w) => Value::Int(i128::from(rng.next_u64() >> (64 - w))),
        Type::SInt(w) => Value::Int(i128::from((rng.next_u64() as i64) >> (64 - w))),
        Type::Float(w) => {
            let bits = rng.next_u64() >> (64 - w);
            let nan = vg_core::float::is_nan(t, bits);
            Value::Float(if nan {
                vg_core::float::canonical_nan(t)
            } else {
                bits
            })
        }
        Type::Array(e, n) => Value::Seq((0..*n).map(|_| random(e, rng)).collect()),
        _ => panic!("no generator for {t}"),
    }
}

/// Input vector k: boundary values cycle through each parameter for the first cases.
fn inputs(node: &Node, k: usize, rng: &mut Rng) -> Vec<Value> {
    node.params
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let b = boundary(t);
            if k < 12 && !b.is_empty() {
                b[(k + i) % b.len()].clone()
            } else if let Type::Array(e, n) = t {
                let b = boundary(e);
                Value::Seq(
                    (0..*n as usize)
                        .map(|j| {
                            if k < 24 && !b.is_empty() {
                                b[(k + j) % b.len()].clone()
                            } else {
                                random(e, rng)
                            }
                        })
                        .collect(),
                )
            } else {
                random(t, rng)
            }
        })
        .collect()
}

/// Runs a built binary, directly or under `runner` (wasmtime), with the stdin/stdout protocol:
/// Ok(results) on exit 0, else the exit code.
fn native(
    runner: Option<&str>,
    bin: &Path,
    node: &Node,
    args: &[Value],
) -> Result<Vec<Value>, i32> {
    let mut stdin = Vec::new();
    for (v, t) in args.iter().zip(&node.params) {
        v.to_bytes(t, &mut stdin).expect("fixed-size input");
    }
    let mut cmd = match runner {
        Some(r) => {
            let mut c = Command::new(r);
            c.arg(bin);
            c
        }
        None => Command::new(bin),
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary runs");
    child.stdin.take().unwrap().write_all(&stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    match out.status.code() {
        Some(0) => {
            let mut at = 0;
            let mut results = Vec::new();
            for t in &node.results {
                let (v, n) = Value::from_bytes(&out.stdout[at..], t).expect("well-formed output");
                results.push(v);
                at += n;
            }
            assert_eq!(at, out.stdout.len(), "trailing output bytes");
            Ok(results)
        }
        c => Err(c.unwrap_or(-1)),
    }
}

fn has_wasmtime() -> bool {
    Command::new("wasmtime")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// One build of a program: its target's name, artifact, runner, and whether it has runtime checks.
struct Build {
    name: String,
    path: std::path::PathBuf,
    runner: Option<&'static str>,
    checks: bool,
}

#[test]
fn native_builds_agree_with_vg_interp() {
    let programs = env_count("VG_DIFF_PROGRAMS", 40);
    let per = env_count("VG_DIFF_INPUTS", 20);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("differential");
    std::fs::create_dir_all(&dir).unwrap();
    let wasm = has_wasmtime();
    let smt = vg_smt::Options::default();
    let mut rng = Rng(SEED);
    let (mut runs, mut traps, mut proven) = (0, 0, 0);
    let mut per_build: std::collections::BTreeMap<String, usize> = Default::default();
    for s in 0..programs as u64 {
        let class = if s % 2 == 0 { Class::A } else { Class::B };
        let src = program(SEED ^ s, class, 5 + (s as usize % 56));
        let nodes = vg_core::asm_local(&src).unwrap_or_else(|e| panic!("seed {s}: {e}\n{src}"));
        let callee = |h: &Hash| {
            nodes
                .iter()
                .find(|a| a.hashes.node == *h)
                .map(|a| a.node.clone())
        };
        let root = &nodes
            .iter()
            .find(|a| a.label == "root")
            .expect("@root")
            .node;
        let verified = nodes.iter().filter(|a| !a.node.spec).all(|a| {
            vg_smt::verify(&a.node, &a.label, &callee, &smt)
                .iter()
                .all(|o| o.verdict == vg_smt::Verdict::Proven)
        });
        proven += usize::from(verified);
        let mut builds = Vec::new();
        let targets: &[(vg_codegen::Target, Option<&'static str>)] = if wasm {
            &[
                (vg_codegen::Target::Aarch64Darwin, None),
                (vg_codegen::Target::Wasm32Wasi, Some("wasmtime")),
            ]
        } else {
            &[(vg_codegen::Target::Aarch64Darwin, None)]
        };
        for &(target, runner) in targets {
            for facts in [false, true] {
                if facts && !verified {
                    continue;
                }
                let opts = vg_codegen::Options {
                    target,
                    proof_facts: facts,
                    runtime_checks: !facts,
                };
                let name = format!(
                    "{}{}",
                    target.clang_target(),
                    if facts { "+facts" } else { "" }
                );
                let out = dir.join(format!("p{s}-{name}"));
                let built = vg_codegen::build(root, &callee, &opts, &out)
                    .unwrap_or_else(|e| panic!("seed {s}: {name} build failed: {e}\n{src}"));
                if !built.linked {
                    continue;
                }
                builds.push(Build {
                    name,
                    path: built.artifact,
                    runner,
                    checks: !facts,
                });
            }
        }
        let mut k = 0;
        let mut tried = 0;
        while k < per && tried < 20 * per {
            tried += 1;
            let args = inputs(root, tried - 1, &mut rng);
            let pre = vg_interp::holds(root, vg_interp::Clause::Pre, &args, &[], &callee);
            if pre != Ok(true) {
                continue;
            }
            k += 1;
            let mut host = vg_interp::NoEffectsHost;
            let want = vg_interp::run(root, &args, &callee, &mut host);
            for b in &builds {
                let got = native(b.runner, &b.path, root, &args);
                match (&want, &got) {
                    (Ok(w), Ok(g)) => {
                        assert_eq!(w, g, "seed {s}, {}, inputs {args:?}\n{src}", b.name)
                    }
                    (Err(_), Err(4)) if b.checks => traps += 1,
                    _ => panic!(
                        "seed {s}, {}, inputs {args:?}: interp {want:?}, build {got:?}\n{src}",
                        b.name
                    ),
                }
                *per_build.entry(b.name.clone()).or_default() += 1;
                runs += 1;
            }
        }
    }
    eprintln!(
        "{programs} programs ({proven} proven), {runs} runs agree with vg-interp ({traps} traps on both sides); runs per build: {per_build:?}"
    );
    assert!(!per_build.is_empty(), "no build ran");
}
