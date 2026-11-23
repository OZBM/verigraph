//! P4.7 (docs/16, "Differential testing"): native builds agree with vg-interp, byte for byte, on
//! generated programs. VG_DIFF_PROGRAMS sets the number of programs (default 40; the G4 gate runs
//! 1,000) and VG_DIFF_INPUTS the inputs per program (default 20; the gate runs 100).

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

/// Runs a built binary with the sprint's stdin/stdout protocol: Ok(results) on exit 0, else the
/// exit code.
fn native(bin: &Path, node: &Node, args: &[Value]) -> Result<Vec<Value>, i32> {
    let mut stdin = Vec::new();
    for (v, t) in args.iter().zip(&node.params) {
        v.to_bytes(t, &mut stdin).expect("fixed-size input");
    }
    let mut child = Command::new(bin)
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

#[test]
fn native_builds_agree_with_vg_interp() {
    let programs = env_count("VG_DIFF_PROGRAMS", 40);
    let per = env_count("VG_DIFF_INPUTS", 20);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("differential");
    std::fs::create_dir_all(&dir).unwrap();
    let mut rng = Rng(SEED);
    let (mut runs, mut traps) = (0, 0);
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
        let opts = vg_codegen::Options {
            target: vg_codegen::Target::Aarch64Darwin,
            proof_facts: false,
            runtime_checks: true,
        };
        let out = dir.join(format!("p{s}"));
        let built = vg_codegen::build(root, &callee, &opts, &out)
            .unwrap_or_else(|e| panic!("seed {s}: build failed: {e}\n{src}"));
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
            let mut host = vg_interp::NoHost;
            let want = vg_interp::run(root, &args, &callee, &mut host);
            let got = native(&built.artifact, root, &args);
            match (&want, &got) {
                (Ok(w), Ok(g)) => assert_eq!(w, g, "seed {s}, inputs {args:?}\n{src}"),
                (Err(_), Err(4)) => traps += 1,
                _ => panic!("seed {s}, inputs {args:?}: interp {want:?}, native {got:?}\n{src}"),
            }
            runs += 1;
        }
    }
    eprintln!(
        "{programs} programs, {runs} runs agree with vg-interp ({traps} traps on both sides)"
    );
}
