use std::collections::BTreeMap;
use std::path::Path;

use vg_core::ir::{Hash, Node};
use vg_core::types::Type;
use vg_core::value::Value;
use vg_core::{asm_local, float};
use vg_smt::{Options, Outcome, Verdict, find_z3, verify};
use vg_telemetry::{
    GuardMap, Names, Reproduced, guard_report, parse_guard_event, prose, report, slice,
};

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
fn soft_knee_draft_acceptance() {
    let path = workspace_root().join("examples/soft_knee_draft.vgs");
    let src = std::fs::read_to_string(&path).expect("read soft_knee_draft.vgs");
    let assembled = asm_local(&src).expect("assemble soft_knee_draft");
    let map = callee_map(&assembled);
    let callee = |h: &Hash| map.get(h).cloned();
    let soft = assembled
        .iter()
        .find(|a| a.label == "soft_knee")
        .expect("soft_knee node");
    let names = Names::from_source(&src, "soft_knee", &|_| None).expect("names");

    // 1. Acceptance test with exact docs/15 inputs: knee = "0.5", x = "3e38".
    let x_bits = float::parse("3e38", false, &Type::Float(32)).unwrap();
    let knee_bits = float::parse("0.5", false, &Type::Float(32)).unwrap();
    let outcome = Outcome {
        id: "@soft_knee/POST.0".to_string(),
        verdict: Verdict::Counterexample,
        code: Some("VG-V0001"),
        solver: "z3".to_string(),
        inputs: Some(vec![Value::Float(x_bits), Value::Float(knee_bits)]),
        message: "refuted".to_string(),
        evidence: None,
    };
    let rep = report(&soft.node, &soft.label, &names, &outcome, &callee)
        .expect("FailureReport for POST.0");

    assert_eq!(rep.node, "@soft_knee");
    assert_eq!(rep.obligation, "@soft_knee/POST.0");
    assert_eq!(rep.obligation_hash.len(), 64);
    assert_eq!(rep.clause, "result >= -1.0");
    assert_eq!(rep.verdict, "counterexample");
    assert_eq!(rep.code, "VG-V0001");
    assert_eq!(rep.reproduced, Reproduced::Concrete);
    assert_eq!(rep.inputs.get("x").and_then(|v| v.as_str()), Some("3e38"));
    assert_eq!(rep.inputs.get("knee").and_then(|v| v.as_str()), Some("0.5"));
    assert!(
        rep.slice.len() <= 5,
        "expected slice <= 5 ops, got {}: {:?}",
        rep.slice.len(),
        rep.slice
    );
    assert!(
        rep.slice
            .iter()
            .any(|s| s.port == "%4" && s.op.starts_with("FDIV(") && s.value == "inf"),
        "slice should contain %4 FDIV -> inf: {:?}",
        rep.slice
    );
    assert!(
        rep.slice
            .iter()
            .any(|s| s.port == "%12" && s.op.starts_with("FCLAMP(") && s.value == "nan"),
        "slice should contain %12 FCLAMP -> nan: {:?}",
        rep.slice
    );

    let d = rep
        .diff
        .as_deref()
        .expect("soft_knee POST.0 should produce an unsat-core diff");
    assert!(
        d.contains("%4") && d.contains("finite"),
        "diff should constrain %4 to be finite, got: {d}"
    );

    // Deterministic sorted JSON serialization.
    let j1 = rep.to_json_string();
    let j2 = rep.to_json_string();
    assert_eq!(j1, j2);
    let parsed: serde_json::Value = serde_json::from_str(&j1).unwrap();
    let keys: Vec<&String> = parsed.as_object().unwrap().keys().collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);

    let text = prose(&rep);
    assert!(text.contains("@soft_knee/POST.0"));
    assert!(text.contains("%4"));
    assert!(text.contains("%12"));

    // 2. Also run live vg_smt::verify on soft_knee_draft.vgs and verify report works on its outcome.
    let outcomes = verify(
        &soft.node,
        &soft.label,
        &callee,
        &Options {
            z3: find_z3(),
            timeout_ms: 5000,
            ..Options::default()
        },
    );
    let post0 = outcomes
        .iter()
        .find(|o| o.id == "@soft_knee/POST.0")
        .expect("POST.0 outcome");
    assert_eq!(post0.verdict, Verdict::Counterexample);
    let live_rep =
        report(&soft.node, &soft.label, &names, post0, &callee).expect("live FailureReport");
    assert_eq!(live_rep.reproduced, Reproduced::Concrete);
    assert!(
        live_rep.slice.len() <= 5,
        "live slice > 5: {:?}",
        live_rep.slice
    );
    assert!(live_rep.slice.iter().any(|s| s.port == "%4"));

    // Direct slice(...) call agrees.
    let direct_slice = slice(
        &soft.node,
        "@soft_knee/POST.0",
        &names,
        &[Value::Float(x_bits), Value::Float(knee_bits)],
        &callee,
    );
    assert_eq!(direct_slice, rep.slice);
}

#[test]
fn sparse_collection_over_16_elements_and_learnable_theta() {
    let src = r#"
module sparse_mod;

NODE @big_read {
  SIGNATURE: (xs: Array<u32, 32>, idx: u64) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE: idx == 7 && xs[7] < 100;
    POST: result < 50;
  }
  LEARNABLE {
    gain: f32 in [0.5, 2.0];
  }
  BODY {
    %0 = INDEX(xs, idx);
    %1 = CAST.f32(%0);
    %2 = FMUL(%1, gain);
    %3 = CAST.u32(%2);
    RETURN(%3);
  }
}
"#;
    let assembled = asm_local(src).expect("asm");
    let map = callee_map(&assembled);
    let callee = |h: &Hash| map.get(h).cloned();
    let n = &assembled[0];
    let names = Names::from_source(src, "big_read", &|_| None).expect("names");

    let mut arr = vec![Value::Int(1); 32];
    arr[7] = Value::Int(40);
    let gain_bits = float::parse("2.0", false, &Type::Float(32)).unwrap();
    let outcome = Outcome {
        id: "@big_read/POST.0".to_string(),
        verdict: Verdict::Counterexample,
        code: Some("VG-V0001"),
        solver: "z3".to_string(),
        inputs: Some(vec![
            Value::Seq(arr),
            Value::Int(7),
            Value::Float(gain_bits),
        ]),
        message: "refuted".to_string(),
        evidence: None,
    };
    let rep = report(&n.node, &n.label, &names, &outcome, &callee).expect("report");
    assert_eq!(rep.reproduced, Reproduced::Concrete);

    // `xs` has 32 elements (> 16), so it must be sparse with keys 0, 7, 31.
    let xs_json = rep.inputs.get("xs").expect("xs in inputs");
    assert_eq!(xs_json["len"], 32);
    let at = xs_json["at"].as_object().expect("at object");
    assert!(at.contains_key("0"));
    assert_eq!(at.get("7").and_then(|v| v.as_u64()), Some(40));
    assert!(at.contains_key("31"));
    assert_eq!(at.len(), 3);

    // `gain` is LEARNABLE, so it lives in `theta`, not `inputs`.
    assert!(!rep.inputs.contains_key("gain"));
    let theta = rep.theta.as_ref().expect("theta present");
    assert_eq!(theta.get("gain").and_then(|v| v.as_str()), Some("2.0"));
}

#[test]
fn runtime_guard_report_and_event_parsing() {
    let src = r#"
module guard_mod;

NODE @safe_branch {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE: x > 0;
  }
  BODY {
    %0 = SUB.checked(x, 1:u32);
    RETURN(%0);
  }
}

NODE @fallback {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {}
  BODY { RETURN(0:u32); }
}

NODE @guarded {
  SIGNATURE: (x: u32) -> (u32, u32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = GT.u(x, 0:u32);
    %1 = ASSERT_GUARD(%0, @safe_branch, x, else=@fallback);
    %2 = ASSERT_GUARD.branch(%0, @safe_branch, x, else=@fallback);
    RETURN(%1, %2);
  }
}
"#;
    let assembled = asm_local(src).expect("asm");
    let map = callee_map(&assembled);
    let callee = |h: &Hash| map.get(h).cloned();
    let guarded = assembled.iter().find(|a| a.label == "guarded").unwrap();
    let names = Names::from_source(src, "guarded", &|_| None).expect("names");

    // Find canonical op index of plain ASSERT_GUARD vs ASSERT_GUARD.branch.
    let plain_idx = guarded
        .node
        .ops
        .iter()
        .position(|op| {
            op.mode == 0
                && vg_core::opcode::by_code(op.opcode).is_some_and(|i| i.name == "ASSERT_GUARD")
        })
        .unwrap();
    let branch_idx = guarded
        .node
        .ops
        .iter()
        .position(|op| {
            op.mode == 1
                && vg_core::opcode::by_code(op.opcode).is_some_and(|i| i.name == "ASSERT_GUARD")
        })
        .unwrap();

    let gmap_json = format!(
        r#"{{
          "g0": {{"obligation": "@guarded/%{plain_idx}/ASSERT_GUARD", "obligation_hash": "{}"}},
          "g1": "@guarded/%{branch_idx}/ASSERT_GUARD"
        }}"#,
        "0".repeat(64)
    );
    let gmap = GuardMap::from_json(&gmap_json).expect("parse GuardMap");
    assert_eq!(gmap.entries.len(), 2);

    let line = format!(
        "{{\"event\":\"guard\",\"guard\":\"g0\",\"obligation\":\"@guarded/%{plain_idx}/ASSERT_GUARD\",\"node\":\"0123456789ab\",\"target\":\"aarch64-apple-darwin\"}}\n"
    );
    let ev = parse_guard_event(&line).expect("parse GuardEvent");
    assert_eq!(ev.guard, "g0");
    assert_eq!(ev.target, "aarch64-apple-darwin");

    let rep = guard_report(
        &gmap,
        &ev.guard,
        &ev.target,
        &guarded.node,
        &guarded.label,
        &names,
        &[Value::Int(0)],
        &callee,
    )
    .expect("guard_report");
    assert_eq!(rep.guard.as_deref(), Some("g0"));
    assert_eq!(rep.target.as_deref(), Some("aarch64-apple-darwin"));
    assert_eq!(rep.reproduced, Reproduced::Concrete);
    assert!(!rep.slice.is_empty());

    // ASSERT_GUARD.branch is normal control flow and must be rejected if queried as a guard failure.
    let err = guard_report(
        &gmap,
        "g1",
        "aarch64-apple-darwin",
        &guarded.node,
        &guarded.label,
        &names,
        &[Value::Int(0)],
        &callee,
    );
    assert!(err.is_err());
}

#[test]
fn corpus_bad_files_produce_valid_reports() {
    let bad_dir = workspace_root().join("tests/corpus/bad");
    let mut entries: Vec<_> = std::fs::read_dir(&bad_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "vgs"))
        .collect();
    entries.sort();
    assert!(!entries.is_empty());

    let opts = Options {
        z3: find_z3(),
        timeout_ms: 5000,
        ..Options::default()
    };

    for path in entries {
        let src = std::fs::read_to_string(&path).unwrap();
        let expect_id = src
            .lines()
            .find_map(|l| l.strip_prefix("// expect: reject "))
            .map(str::trim)
            .unwrap_or_else(|| panic!("missing expect line in {}", path.display()));
        let target_label = expect_id.split_once('/').unwrap().0.trim_start_matches('@');
        let assembled = asm_local(&src).unwrap();
        let map = callee_map(&assembled);
        let callee = |h: &Hash| map.get(h).cloned();
        let target = assembled.iter().find(|a| a.label == target_label).unwrap();
        let names = Names::from_source(&src, target_label, &|_| None).unwrap();

        let outcomes = verify(&target.node, &target.label, &callee, &opts);
        let ob = outcomes
            .iter()
            .find(|o| o.id == expect_id)
            .unwrap_or_else(|| panic!("missing outcome {expect_id} in {}", path.display()));
        assert_eq!(
            ob.verdict,
            Verdict::Counterexample,
            "expected Counterexample for {} in {}",
            expect_id,
            path.display()
        );
        let rep = report(&target.node, &target.label, &names, ob, &callee).expect("FailureReport");
        assert_eq!(rep.reproduced, Reproduced::Concrete, "{}", path.display());
        assert!(!rep.slice.is_empty(), "empty slice in {}", path.display());
    }
}
