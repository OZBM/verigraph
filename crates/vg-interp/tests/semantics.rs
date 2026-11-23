//! vg-interp on whole programs: the soft-knee story of docs/03, and every pure node of the alpha
//! corpus on seeded inputs (it must return or trap, never panic).

use std::fs;
use std::path::{Path, PathBuf};

use vg_core::ir::{Hash, Node};
use vg_core::types::Type;
use vg_core::value::Value;
use vg_interp::{Clause, NoEffectsHost, holds, run};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn soft_knee(file: &str) -> (Node, Vec<Value>, Vec<Value>, Result<bool, vg_interp::Trap>) {
    let src = fs::read_to_string(repo().join(file)).unwrap();
    let nodes = vg_core::asm_local(&src).unwrap();
    let node = nodes
        .iter()
        .find(|a| a.label == "soft_knee")
        .unwrap()
        .node
        .clone();
    let none = |_: &Hash| None;
    // x = 3e38, knee = 0.5, as f32 bits.
    let args = vec![
        Value::Float(3e38f32.to_bits().into()),
        Value::Float(0.5f32.to_bits().into()),
    ];
    assert_eq!(
        holds(&node, Clause::Pre, &args, &[], &none).map_err(|t| t.message),
        Ok(true)
    );
    let out = run(&node, &args, &none, &mut NoEffectsHost).unwrap();
    let post = holds(&node, Clause::Post, &args, &out, &none);
    (node, args, out, post)
}

/// docs/03: the first draft overflows t to +inf, computes inf / inf = NaN and returns NaN, which
/// breaks POST.0; the repaired kernel returns 1.0 on the same input and keeps its POST.
#[test]
fn soft_knee_story() {
    let (_, _, out, post) = soft_knee("examples/soft_knee_draft.vgs");
    assert_eq!(
        out,
        vec![Value::Float(0x7FC0_0000)],
        "the draft returns NaN"
    );
    assert_eq!(
        post.map_err(|t| t.message),
        Ok(false),
        "the draft breaks its POST"
    );
    let (_, _, out, post) = soft_knee("examples/quickstart.vgs");
    assert_eq!(out, vec![Value::Float(1.0f32.to_bits().into())]);
    assert_eq!(
        post.map_err(|t| t.message),
        Ok(true),
        "the repaired kernel keeps its POST"
    );
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

fn random(t: &Type, r: &mut Rng) -> Option<Value> {
    Some(match t {
        Type::Bool => Value::Bool(r.next() & 1 == 1),
        Type::UInt(w) => Value::Int(i128::from(r.next() >> (64 - w))),
        Type::SInt(w) => Value::Int(i128::from((r.next() as i64) >> (64 - w))),
        Type::Float(w @ (32 | 64)) => {
            let bits = r.next() >> (64 - w);
            let nan = vg_core::float::is_nan(t, bits);
            Value::Float(if nan {
                vg_core::float::canonical_nan(t)
            } else {
                bits
            })
        }
        Type::Array(e, n) if *n <= 64 => {
            Value::Seq((0..*n).map(|_| random(e, r)).collect::<Option<_>>()?)
        }
        Type::Slice(e) => Value::Seq(
            (0..r.next() % 5)
                .map(|_| random(e, r))
                .collect::<Option<_>>()?,
        ),
        _ => return None,
    })
}

#[test]
fn alpha_corpus_nodes_run_without_panicking() {
    let mut dirs: Vec<PathBuf> = fs::read_dir(repo().join("tests/corpus/alpha"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let mut r = Rng(0xA1FA_2026_1123);
    let (mut runs, mut traps) = (0, 0);
    for d in dirs {
        let mut files: Vec<PathBuf> = fs::read_dir(&d)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        files.sort();
        let src = fs::read_to_string(&files[0]).unwrap();
        if src.contains("@MESH_QUERY") {
            continue;
        }
        let nodes = vg_core::asm_local(&src).unwrap();
        let callee = |h: &Hash| {
            nodes
                .iter()
                .find(|a| a.hashes.node == *h)
                .map(|a| a.node.clone())
        };
        for a in nodes
            .iter()
            .filter(|a| !a.node.spec && a.node.effects.is_pure())
        {
            for _ in 0..20 {
                let args: Option<Vec<Value>> = (0..a.node.input_count() as u32)
                    .map(|i| random(a.node.input_type(i)?, &mut r))
                    .collect();
                let Some(args) = args else { break };
                runs += 1;
                traps += usize::from(run(&a.node, &args, &callee, &mut NoEffectsHost).is_err());
            }
        }
    }
    eprintln!("{runs} runs of alpha-corpus nodes, {traps} traps, no panic");
    assert!(runs > 500, "only {runs} runs");
}
