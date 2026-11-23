use std::collections::BTreeMap;
use std::path::Path;

use vg_core::asm_local;
use vg_core::ir::{Hash, Node};
use vg_smt::{Options, Verdict, find_z3, verify};
use vg_telemetry::{Names, Reproduced, report};

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

fn callee_map(nodes: &[vg_core::Assembled]) -> BTreeMap<Hash, Node> {
    nodes
        .iter()
        .map(|a| (a.hashes.node, a.node.clone()))
        .collect()
}

#[test]
fn g6_seeded_slices() {
    let dir = workspace_root().join("tests/telemetry/seeded");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("read tests/telemetry/seeded")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "vgs"))
        .collect();
    files.sort();
    assert_eq!(files.len(), 60, "expected exactly 60 seeded bug files");

    let opts = Options {
        z3: find_z3(),
        timeout_ms: 5000,
    };

    let mut small_slices = 0usize;
    let mut failures = Vec::new();

    for path in &files {
        let fname = path.file_name().unwrap().to_string_lossy();
        let src = std::fs::read_to_string(path).unwrap();
        let expect_id = src
            .lines()
            .find_map(|l| l.strip_prefix("// expect: reject "))
            .map(str::trim)
            .unwrap_or_else(|| panic!("missing expect line in {fname}"));
        let slice_max: usize = src
            .lines()
            .find_map(|l| l.strip_prefix("// slice-max: "))
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(5);

        let target_label = expect_id.split_once('/').unwrap().0.trim_start_matches('@');

        let assembled = asm_local(&src).unwrap_or_else(|e| panic!("{fname}: asm failed: {e}"));
        let map = callee_map(&assembled);
        let callee = |h: &Hash| map.get(h).cloned();
        let target = assembled
            .iter()
            .find(|a| a.label == target_label)
            .unwrap_or_else(|| panic!("{fname}: missing node @{target_label}"));
        let names = Names::from_source(&src, target_label, &|_| None)
            .unwrap_or_else(|e| panic!("{fname}: names failed: {e}"));

        let outcomes = verify(&target.node, &target.label, &callee, &opts);
        let Some(ob) = outcomes.iter().find(|o| o.id == expect_id) else {
            let ids: Vec<&str> = outcomes.iter().map(|o| o.id.as_str()).collect();
            failures.push(format!(
                "{fname}: missing obligation {expect_id}; got {ids:?}"
            ));
            continue;
        };
        if ob.verdict != Verdict::Counterexample {
            failures.push(format!(
                "{fname}: expected Counterexample for {expect_id}, got {:?} ({})",
                ob.verdict, ob.message
            ));
            continue;
        }

        let rep = report(&target.node, &target.label, &names, ob, &callee)
            .unwrap_or_else(|| panic!("{fname}: report returned None"));

        let len = rep.slice.len();
        if len <= slice_max && !rep.slice.is_empty() {
            small_slices += 1;
        }
        if rep.reproduced == Reproduced::No {
            eprintln!(
                "NOTE {fname}: reproduced=no slice_len={len} diff={:?}",
                rep.diff
            );
        }
    }

    assert!(
        failures.is_empty(),
        "seeded files failed verification expectations:\n{}",
        failures.join("\n")
    );

    println!("G6 small_slices={small_slices}/60");
    assert!(
        small_slices >= 48,
        "G6 requires at least 48/60 seeded bugs with slice <= 5 ops, got {small_slices}/60"
    );
}
