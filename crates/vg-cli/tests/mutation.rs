//! Mutation testing (P3.6, G3; docs/16 "Verifier suites"): mutate every verified node, and if a
//! mutant still verifies, search for an input that satisfies PRE and makes the mutant trap or break
//! POST in vg-interp. Finding one means the verifier accepted a contract-breaking program: the test
//! fails. Mutants that survive without a violation are counted, not failed (R-04).

use std::path::Path;

use vg_core::canon::canonicalize;
use vg_core::generate::Rng;
use vg_core::ir::{Const, Hash, Node};
use vg_core::types::Type;
use vg_core::value::Value;
use vg_core::{asm_local, opcode};

const SEED: u64 = 0x3D7A_2026_1123;

fn code(name: &str) -> u16 {
    opcode::by_name(name).expect("opcode").code
}

/// Every single-step mutant of docs/16: flipped comparisons, changed modes, replaced constants,
/// swapped operands of non-commutative ops, swapped SELECT branches.
fn mutants(n: &Node) -> Vec<(String, Node)> {
    let mut out = Vec::new();
    let flips = [("LT", "LE"), ("GT", "GE"), ("EQ", "NE")];
    for (i, op) in n.ops.iter().enumerate() {
        let info = opcode::by_code(op.opcode).expect("valid");
        for (a, b) in flips {
            for (from, to) in [(a, b), (b, a)] {
                if op.opcode == code(from) {
                    let mut m = n.clone();
                    m.ops[i].opcode = code(to);
                    out.push((format!("op {i}: {from} -> {to}"), m));
                }
            }
        }
        let modes: &[u8] = match info.modes {
            opcode::Modes::IntArith => &[0, 1, 2],
            opcode::Modes::Shift => &[0, 1],
            opcode::Modes::Compare => &[1, 2],
            _ => &[],
        };
        for &mode in modes.iter().filter(|&&m| m != op.mode) {
            let mut m = n.clone();
            m.ops[i].mode = mode;
            out.push((
                format!("op {i}: {} mode {} -> {mode}", info.name, op.mode),
                m,
            ));
        }
        let commutes = info.comm != opcode::Comm::No;
        if op.operands.len() >= 2
            && !commutes
            && !matches!(
                info.shape,
                opcode::Shape::Kernel
                    | opcode::Shape::Fold
                    | opcode::Shape::Guard
                    | opcode::Shape::Call
            )
        {
            let (x, y) = if info.name == "SELECT" {
                (1, 2)
            } else {
                (0, 1)
            };
            if op.operands[x] != op.operands[y] {
                let mut m = n.clone();
                m.ops[i].operands.swap(x, y);
                out.push((
                    format!("op {i}: {} operands {x} and {y} swapped", info.name),
                    m,
                ));
            }
        }
    }
    for (c, k) in n.consts.iter().enumerate() {
        for (what, bytes) in replacements(k) {
            let mut m = n.clone();
            m.consts[c].bytes = bytes;
            out.push((format!("const {c}: {what}"), m));
        }
    }
    out
}

fn replacements(k: &Const) -> Vec<(String, Vec<u8>)> {
    let int = |v: i128| vg_core::lower::int_const(v).bytes;
    match &k.ty {
        t @ (Type::UInt(_) | Type::SInt(_)) => {
            let v = vg_core::lower::int_value(&k.bytes).unwrap_or(0);
            let (lo, hi) = match t {
                Type::UInt(w) => (0, (1i128 << w) - 1),
                Type::SInt(w) => (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1),
                _ => unreachable!(),
            };
            [v + 1, v - 1, 0, lo, hi]
                .into_iter()
                .filter(|x| *x != v && (lo..=hi).contains(x))
                .map(|x| (format!("{v} -> {x}"), int(x)))
                .collect()
        }
        t @ Type::Float(32) | t @ Type::Float(64) => {
            let n = t.byte_size().unwrap() as usize;
            let mut bits = [0u8; 8];
            bits[..n].copy_from_slice(&k.bytes);
            let b = u64::from_le_bytes(bits);
            let sign = 1u64 << (8 * n - 1);
            let inf = vg_core::float::canonical_nan(t) & !(sign >> 9 >> if n == 8 { 3 } else { 0 });
            [
                ("sign flipped", b ^ sign),
                ("NaN", vg_core::float::canonical_nan(t)),
                ("+inf", inf),
                ("-inf", inf | sign),
            ]
            .into_iter()
            .filter(|(_, x)| *x != b)
            .map(|(w, x)| (w.to_string(), x.to_le_bytes()[..n].to_vec()))
            .collect()
        }
        _ => vec![],
    }
}

fn random(t: &Type, rng: &mut Rng) -> Option<Value> {
    Some(match t {
        Type::Bool => Value::Bool(rng.chance(1, 2)),
        Type::UInt(w) => Value::Int(i128::from(match rng.below(4) {
            0 => rng.next_u64() % 4,
            1 => (rng.next_u64() >> (64 - w)).saturating_sub(rng.next_u64() % 3),
            _ => rng.next_u64() >> (64 - w),
        })),
        Type::SInt(w) => Value::Int(i128::from(match rng.below(4) {
            0 => (rng.next_u64() % 5) as i64 - 2,
            _ => (rng.next_u64() as i64) >> (64 - w),
        })),
        Type::Float(w) => {
            let special = [0u64, 1 << (w - 1), 0x3F80_0000, 0xBF80_0000, 0x7F7F_FFFF, 1];
            let bits = if rng.chance(1, 3) && *w == 32 {
                special[rng.below(special.len())]
            } else {
                rng.next_u64() >> (64 - w)
            };
            let nan = vg_core::float::is_nan(t, bits);
            Value::Float(if nan {
                vg_core::float::canonical_nan(t)
            } else {
                bits
            })
        }
        Type::Array(e, n) => Value::Seq((0..*n).map(|_| random(e, rng)).collect::<Option<_>>()?),
        _ => return None,
    })
}

/// An input that satisfies PRE and makes the node trap or break POST, among 10,000 seeded tries.
fn violation(
    n: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    rng: &mut Rng,
) -> Option<Vec<Value>> {
    for _ in 0..10_000 {
        let args: Vec<Value> = (0..n.input_count() as u32)
            .map(|i| random(n.input_type(i)?, rng))
            .collect::<Option<_>>()?;
        if vg_interp::holds(n, vg_interp::Clause::Pre, &args, &[], callee) != Ok(true) {
            continue;
        }
        let mut host = vg_interp::NoEffectsHost;
        match vg_interp::run(n, &args, callee, &mut host) {
            Err(_) => return Some(args),
            Ok(r)
                if vg_interp::holds(n, vg_interp::Clause::Post, &args, &r, callee) != Ok(true) =>
            {
                return Some(args);
            }
            Ok(_) => {}
        }
    }
    None
}

#[test]
fn no_contract_breaking_mutant_verifies() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files: Vec<_> = std::fs::read_dir(root.join("tests/corpus/good"))
        .map(|d| d.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    files.push(root.join("examples/quickstart.vgs"));
    files.retain(|p| p.extension().is_some_and(|x| x == "vgs"));
    files.sort();
    let opts = vg_smt::Options {
        timeout_ms: 5_000,
        ..vg_smt::Options::default()
    };
    // Without a solver every mutant would count as killed and the test would pass vacuously.
    let z3_runs = std::process::Command::new(&opts.z3)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(z3_runs, "z3 does not run: {}", opts.z3.display());
    let mut rng = Rng(SEED);
    let (mut killed, mut survived, mut total) = (0, 0, 0);
    let mut unsound = Vec::new();
    for f in &files {
        let src = std::fs::read_to_string(f).unwrap();
        let Ok(nodes) = asm_local(&src) else { continue };
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
            for (what, m) in mutants(&a.node) {
                let Ok(c) = canonicalize(&m) else { continue };
                if vg_types::check_node(&c.node, &callee).is_err() {
                    continue;
                }
                total += 1;
                let proven = vg_smt::verify(&c.node, &a.label, &callee, &opts)
                    .iter()
                    .all(|o| matches!(o.verdict, vg_smt::Verdict::Proven));
                if !proven {
                    killed += 1;
                } else if let Some(args) = violation(&c.node, &callee, &mut rng) {
                    unsound.push(format!(
                        "{}: @{} {what}: verifies, but breaks on {args:?}",
                        f.display(),
                        a.label
                    ));
                } else {
                    survived += 1;
                }
            }
        }
    }
    eprintln!(
        "{total} mutants of {} files: {killed} killed, {survived} survived, {} unsound",
        files.len(),
        unsound.len()
    );
    assert!(
        unsound.is_empty(),
        "contract-breaking mutants verified:\n{}",
        unsound.join("\n")
    );
    assert!(total > 0, "no mutant was checked");
}
