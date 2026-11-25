#![cfg(feature = "r-13")]
//! R-13 experiment (docs/research/R-13-report.md): lift each function of
//! research/r-13/corpus.tsv through OmniLift's Python path (Alive2 plus 1e6 fuzz cases inside
//! `lift_source`), run each lift in vg-interp on seeded inputs, and let
//! research/r-13/compare.py check every result against CPython.
//!
//! cargo test --release -p vg-omnilift --features r-13 --test r13 -- --ignored --nocapture
//! Writes research/r-13/results/{lift.json,summary.json}; the per-case file stays in target/.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use vg_core::types::Type;
use vg_core::value::Value;
use vg_interp::{NoEffectsHost, run};
use vg_omnilift::ingest::Language;
use vg_omnilift::lift::lift_source;

const CASES: usize = 100_000;
const SEED: u64 = 13;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn python(args: &[&str]) -> String {
    let out = Command::new("python3")
        .arg("-I")
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn range(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() % (hi - lo + 1) as u64) as i64
    }
}

const EDGES: [i64; 17] = [
    0,
    1,
    -1,
    2,
    -2,
    63,
    64,
    -64,
    i32::MAX as i64,
    i32::MIN as i64,
    1 << 32,
    1 << 62,
    -(1 << 62),
    i64::MAX,
    i64::MIN,
    i64::MAX - 1,
    i64::MIN + 1,
];

/// Small, medium, edge and full-width ints; only small ones when an int drives a loop.
fn draw_int(rng: &mut Rng, small: bool) -> i64 {
    if small {
        return rng.range(-64, 2048);
    }
    match rng.next() % 4 {
        0 => rng.range(-16, 16),
        1 => rng.range(-(1 << 16), 1 << 16),
        2 => EDGES[(rng.next() % EDGES.len() as u64) as usize],
        _ => rng.next() as i64,
    }
}

/// One argument as JSON (for CPython) and as a vg-interp value (for the lift).
fn draw(rng: &mut Rng, kind: &str, ty: &Type, small: bool) -> (String, Value) {
    let int_value = |v: i64| match ty {
        Type::UInt(_) => Value::Int(i128::from(v as u64)),
        _ => Value::Int(i128::from(v)),
    };
    match kind {
        "bool" => {
            let b = rng.next() % 2;
            (
                if b == 1 { "true" } else { "false" }.into(),
                int_value(b as i64),
            )
        }
        "bytes" | "ints" => {
            let n = (rng.next() % if kind == "bytes" { 13 } else { 9 }) as usize;
            let elems: Vec<i64> = (0..n)
                .map(|_| {
                    if kind == "bytes" {
                        (rng.next() % 256) as i64
                    } else {
                        draw_int(rng, true)
                    }
                })
                .collect();
            // The lift reads every sequence as Slice<u8>: int64 elements are 8 LE bytes.
            let bytes: Vec<Value> = if kind == "bytes" {
                elems.iter().map(|&b| Value::Int(i128::from(b))).collect()
            } else {
                elems
                    .iter()
                    .flat_map(|e| e.to_le_bytes())
                    .map(|b| Value::Int(i128::from(b)))
                    .collect()
            };
            let json = format!(
                "[{}]",
                elems
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            );
            (json, Value::Seq(bytes))
        }
        _ => {
            let v = draw_int(rng, small);
            (v.to_string(), int_value(v))
        }
    }
}

/// The lift returns the C int64_t as u64 bits; CPython's value is the signed reading.
fn as_python_int(v: &Value) -> Option<i128> {
    match v {
        Value::Int(x) if *x > i128::from(i64::MAX) => Some(*x - (1i128 << 64)),
        Value::Int(x) => Some(*x),
        Value::Bool(b) => Some(i128::from(*b)),
        _ => None,
    }
}

fn json_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[test]
#[ignore = "R-13 experiment: minutes of Alive2 and fuzzing; run explicitly"]
fn r13_corpus() {
    let roots = python(&[
        "-c",
        "import sysconfig; p = sysconfig.get_paths(); print(p['stdlib']); print(p['purelib'])",
    ]);
    let mut roots = roots.lines();
    let (stdlib, site) = (
        roots.next().unwrap().to_string(),
        roots.next().unwrap().to_string(),
    );
    let compare = repo().join("research/r-13/compare.py");
    let compare = compare.to_str().unwrap();
    let corpus = std::fs::read_to_string(repo().join("research/r-13/corpus.tsv")).unwrap();

    let out_dir = repo().join("target/r13");
    std::fs::create_dir_all(&out_dir).unwrap();
    let cases_path = out_dir.join("cases.jsonl");
    let mut cases = String::new();
    let mut lift_report = Vec::new();
    let mut rng = Rng(SEED);

    for line in corpus
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let cols: Vec<&str> = line.split('\t').collect();
        let root = if cols[0] == "stdlib" { &stdlib } else { &site };
        let path = Path::new(root).join(cols[1]);
        let (rel, function) = (format!("{}/{}", cols[0], cols[1]), cols[2]);
        eprintln!("[r13] {rel}::{function}");
        let lifted = match lift_source(&path, function, Language::Python, &[]) {
            Ok(l) => l,
            Err(d) => {
                let first = d.message.lines().next().unwrap_or("").to_string();
                lift_report.push(format!(
                    "{{\"function\": {}, \"lifted\": false, \"code\": {}, \"error\": {}}}",
                    json_str(&format!("{rel}::{function}")),
                    json_str(d.code),
                    json_str(&first)
                ));
                continue;
            }
        };
        lift_report.push(format!(
            "{{\"function\": {}, \"lifted\": true, \"bounded_check\": {}, \"unroll\": {}, \
             \"fuzz_cases\": {}, \"returns_result\": {}, \"node_hash\": {}}}",
            json_str(&format!("{rel}::{function}")),
            json_str(&lifted.bounded_check),
            lifted.unroll,
            lifted.fuzz_cases,
            lifted.returns_result,
            json_str(&vg_core::ir::hex(&lifted.node_hash))
        ));

        let kinds: Vec<String> = python(&[compare, "--kinds", path.to_str().unwrap(), function])
            .trim()
            .trim_matches(['[', ']'])
            .split(", ")
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_matches('"').to_string())
            .collect();
        let root_node = &lifted.nodes.last().unwrap().node;
        let map: BTreeMap<_, _> = lifted
            .nodes
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        let small = lifted.vgsym.contains("GENERATE(");
        let kinds_json = format!(
            "[{}]",
            kinds
                .iter()
                .map(|k| json_str(k))
                .collect::<Vec<_>>()
                .join(",")
        );
        for _ in 0..CASES {
            let (json_args, values): (Vec<String>, Vec<Value>) = kinds
                .iter()
                .zip(&root_node.params)
                .map(|(k, ty)| draw(&mut rng, k, ty, small))
                .unzip();
            let result = run(
                root_node,
                &values,
                &|h| map.get(h).cloned(),
                &mut NoEffectsHost,
            );
            let lifted_json = match result.as_deref() {
                Ok([Value::Ok(v)]) if as_python_int(v).is_some() => {
                    format!("{{\"ok\": {}}}", as_python_int(v).unwrap())
                }
                Ok([v]) if as_python_int(v).is_some() => {
                    format!("{{\"ok\": {}}}", as_python_int(v).unwrap())
                }
                Ok([Value::Err(_)]) => "{\"err\": true}".to_string(),
                // A trap is neither CPython's value nor Err: compare.py counts it WRONG.
                other => format!("{{\"trap\": {}}}", json_str(&format!("{other:?}"))),
            };
            writeln!(
                cases,
                "{{\"path\": {}, \"function\": {}, \"params\": {kinds_json}, \"args\": [{}], \"lifted\": {lifted_json}}}",
                json_str(path.to_str().unwrap()),
                json_str(function),
                json_args.join(",")
            )
            .unwrap();
        }
    }

    let results = repo().join("research/r-13/results");
    std::fs::create_dir_all(&results).unwrap();
    std::fs::write(
        results.join("lift.json"),
        format!("[\n{}\n]\n", lift_report.join(",\n")),
    )
    .unwrap();
    std::fs::write(&cases_path, cases).unwrap();
    let summary = results.join("summary.json");
    let out = Command::new("python3")
        .args([
            "-I",
            compare,
            cases_path.to_str().unwrap(),
            summary.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    eprintln!("{}", String::from_utf8_lossy(&out.stdout));
    assert!(
        out.status.success(),
        "WRONG cases: see {}",
        summary.display()
    );
}
