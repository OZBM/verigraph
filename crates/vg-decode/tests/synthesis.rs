use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn parse_run_line(line: &str) -> (String, Value, Vec<Value>) {
    let rest = line.strip_prefix("// run:").expect("run prefix").trim();
    let (node, rest) = rest
        .split_once(' ')
        .unwrap_or_else(|| panic!("invalid // run: line `{line}`"));
    let (in_str, out_str) = rest
        .split_once("=>")
        .unwrap_or_else(|| panic!("invalid // run: line missing `=>`: `{line}`"));
    let inputs: Value =
        serde_json::from_str(in_str.trim()).unwrap_or_else(|e| panic!("bad run input JSON: {e}"));
    assert!(inputs.is_object(), "run input must be JSON object: {line}");
    let outputs: Vec<Value> =
        serde_json::from_str(out_str.trim()).unwrap_or_else(|e| panic!("bad run output JSON: {e}"));
    (node.to_string(), inputs, outputs)
}

#[test]
fn synthesis_benchmark_30_tasks_valid() {
    let synth_dir = repo_root().join("benchmarks/synthesis");
    let readme_path = synth_dir.join("README.md");
    assert!(
        readme_path.is_file(),
        "missing benchmarks/synthesis/README.md"
    );
    let readme = std::fs::read_to_string(&readme_path).unwrap();

    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&synth_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    assert_eq!(
        dirs.len(),
        30,
        "expected 30 synthesis task directories, found {}",
        dirs.len()
    );

    let mut prefixes = BTreeSet::new();
    for (idx, dir) in dirs.iter().enumerate() {
        let name = dir.file_name().unwrap().to_str().unwrap();
        let expected_prefix = format!("{:02}_", idx + 1);
        assert!(
            name.starts_with(&expected_prefix),
            "expected directory starting with `{expected_prefix}`, found `{name}`"
        );
        assert!(prefixes.insert(expected_prefix));
        assert!(
            readme.contains(name),
            "README.md missing entry for `{name}`"
        );

        let req_path = dir.join("request.md");
        let ref_path = dir.join("reference.vgs");
        let con_path = dir.join("contract.vgs");
        let tst_path = dir.join("tests.json");

        for p in [&req_path, &ref_path, &con_path, &tst_path] {
            assert!(p.is_file(), "missing {}", p.display());
        }

        // 1. request.md: plain language, 2-6 sentences, no VG-Sym syntax
        let req = std::fs::read_to_string(&req_path).unwrap();
        assert!(!req.trim().is_empty(), "empty request.md in {name}");
        assert!(
            !req.contains("NODE @") && !req.contains("SIGNATURE:") && !req.contains("BODY {"),
            "request.md in {name} contains VG-Sym syntax"
        );
        let sentences = req.matches(". ").count() + usize::from(req.trim_end().ends_with('.'));
        assert!(
            (2..=6).contains(&sentences),
            "request.md in {name} has {sentences} sentences (expected 2..=6)"
        );

        // 2. reference.vgs: assembles, type-checks, has // expect: verify and >=2 // run: lines
        let ref_src = std::fs::read_to_string(&ref_path).unwrap();
        assert!(
            ref_src.contains("// expect: verify"),
            "{name}/reference.vgs missing `// expect: verify`"
        );
        let assembled = vg_core::asm_local(&ref_src)
            .unwrap_or_else(|d| panic!("asm_local failed on {name}/reference.vgs: {d}"));
        assert!(
            !assembled.is_empty(),
            "{name}/reference.vgs produced no nodes"
        );
        vg_types::check_source(&ref_src)
            .unwrap_or_else(|ds| panic!("check_source failed on {name}/reference.vgs: {ds:?}"));

        let run_lines: Vec<_> = ref_src
            .lines()
            .filter(|l| l.starts_with("// run:"))
            .map(parse_run_line)
            .collect();
        assert!(
            !run_lines.is_empty(),
            "{name}/reference.vgs must have // run: lines"
        );

        // 3. contract.vgs: parses as VG-Sym, states CALL_HASH refinement rule, has @MESH_QUERY
        let con_src = std::fs::read_to_string(&con_path).unwrap();
        assert!(
            con_src.contains("CALL_HASH"),
            "{name}/contract.vgs missing CALL_HASH refinement header comment"
        );
        let con_ast = vg_core::parser::parse(&con_src)
            .unwrap_or_else(|d| panic!("parse failed on {name}/contract.vgs: {d}"));
        let has_query = con_ast
            .items
            .iter()
            .any(|it| matches!(it, vg_core::ast::Item::Query(_)));
        assert!(has_query, "{name}/contract.vgs missing @MESH_QUERY");

        // 4. tests.json: >= 8 vectors with node name, inputs object, outputs array
        let tst_src = std::fs::read_to_string(&tst_path).unwrap();
        let tst: Value = serde_json::from_str(&tst_src)
            .unwrap_or_else(|e| panic!("bad JSON in {name}/tests.json: {e}"));
        let node = tst["node"].as_str().expect("tests.json node string");
        assert!(node.starts_with('@'), "node must start with @ in {name}");
        for (r_node, _, _) in &run_lines {
            assert_eq!(r_node, node, "run line node mismatch in {name}");
        }
        let cases = tst["cases"].as_array().expect("tests.json cases array");
        assert!(
            cases.len() >= 8,
            "{name}/tests.json has {} cases (expected >= 8)",
            cases.len()
        );
        for (ci, case) in cases.iter().enumerate() {
            assert!(
                case["inputs"].is_object(),
                "{name}/tests.json case {ci} inputs must be object"
            );
            assert!(
                case["outputs"].is_array() && !case["outputs"].as_array().unwrap().is_empty(),
                "{name}/tests.json case {ci} outputs must be non-empty array"
            );
        }
    }
}
