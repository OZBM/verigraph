use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use vg_core::generate::{self, Class};
use vg_core::ir::{Hash, Node};
use vg_types::{CostExpr, Summary, check_node, check_source};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn vgs_files_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "vgs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn alpha_corpus_checks_clean() {
    let alpha_root = repo().join("tests/corpus/alpha");
    let mut families: Vec<String> = fs::read_dir(&alpha_root)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    families.sort();
    assert!(
        families.len() >= 56,
        "expected at least 56 alpha families, found {}",
        families.len()
    );

    let mut clean_files = 0usize;
    let mut t0120_files = 0usize;
    for fam in &families {
        let files = vgs_files_sorted(&alpha_root.join(fam));
        assert!(!files.is_empty(), "family {fam} has no .vgs files");
        for f in &files {
            let src = fs::read_to_string(f).unwrap();
            let res = check_source(&src);
            if fam == "a53_collections_slice_concat" {
                // a53/@join_and_slice computes `CONCAT(a, b)` into a local Array<u32, 4>
                // and returns `SLICE(%0, 0:u64, 3:u64)`, which views a local frame value
                // (docs/13-semantics.md:46, ADR 0003). Soundness forbids accepting it.
                let diags = res.expect_err(&format!(
                    "{} should fail VG-T0120 in @join_and_slice",
                    f.display()
                ));
                assert!(
                    diags.iter().any(|d| {
                        d.code == "VG-T0120" && d.node.as_deref() == Some("@join_and_slice")
                    }),
                    "{}: expected VG-T0120 at @join_and_slice, got {diags:#?}",
                    f.display()
                );
                t0120_files += 1;
            } else {
                let summaries = res.unwrap_or_else(|diags| {
                    panic!(
                        "alpha corpus file {} failed check_source: {diags:#?}",
                        f.display()
                    )
                });
                assert!(!summaries.is_empty());
                // Check determinism: running twice yields identical summaries.
                let summaries2 = check_source(&src).unwrap();
                assert_eq!(summaries, summaries2);
                clean_files += 1;
            }
        }
    }
    assert_eq!(clean_files, 165);
    assert_eq!(t0120_files, 3);
}

#[test]
fn golden_and_examples_check_clean() {
    for rel in [
        "tests/golden/minimal.vgs",
        "tests/golden/sections.vgs",
        "examples/soft_knee_draft.vgs",
    ] {
        let src = fs::read_to_string(repo().join(rel)).unwrap();
        let summaries = check_source(&src)
            .unwrap_or_else(|diags| panic!("{rel} failed check_source: {diags:#?}"));
        assert!(!summaries.is_empty(), "{rel} produced no summaries");
    }

    // Check examples/audio_limiter.vgs by resolving @sha256 with a pure mock implementation + spec.
    let audio_src = fs::read_to_string(repo().join("examples/audio_limiter.vgs")).unwrap();
    let mock_sha_src = r#"
module mock_sha;

SPEC @sha256_spec {
  SIGNATURE: (msg: Slice<u8>) -> (Array<u8, 32>);
  EFFECTS: [];
  CONTRACT { }
  BODY {
    %0 = IDENTITY([0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]:Array<u8, 32>);
    RETURN(%0);
  }
}

NODE @sha256_impl {
  SIGNATURE: (msg: Slice<u8>) -> (Array<u8, 32>);
  EFFECTS: [];
  CONTRACT {
    POST: result == spec:sha256_spec(msg);
  }
  BODY {
    %0 = IDENTITY([0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]:Array<u8, 32>);
    RETURN(%0);
  }
}
"#;
    let mock_asm = vg_core::asm_local(mock_sha_src).unwrap();
    let sha_spec_hash = mock_asm[0].hashes.node;
    let sha_node_hash = mock_asm[1].hashes.node;

    let mut by_hash: BTreeMap<Hash, Node> = mock_asm
        .into_iter()
        .map(|a| (a.hashes.node, a.node))
        .collect();

    let ast = vg_core::parser::parse(&audio_src).unwrap();
    let assembled = vg_core::asm_module(
        &ast,
        &audio_src,
        &|label| {
            if label == "sha256" {
                Some((sha_node_hash, Some(sha_spec_hash)))
            } else {
                None
            }
        },
        false,
    )
    .unwrap();

    let mut summaries: Vec<(String, Summary)> = Vec::new();
    for a in assembled {
        let s = check_node(&a.node, &|h| by_hash.get(h).cloned())
            .unwrap_or_else(|diags| panic!("audio_limiter @{} failed: {diags:#?}", a.label));
        by_hash.insert(a.hashes.node, a.node);
        summaries.push((a.label, s));
    }
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].0, "soft_knee");
    assert_eq!(summaries[0].1.cost_ops, 13);
    assert_eq!(summaries[0].1.arena_bytes, CostExpr::Const(0));
    assert_eq!(summaries[1].0, "limit_frame");
    // limit_frame:
    //   %0 = SIMD_MAP(@soft_knee, frame, knee) -> 1 + 1024 * 13 = 13313
    //   %1 = AS_BYTES(%0) -> 1
    //   %2 = CALL_HASH(@sha256, %1) -> 1 + 0 = 1
    //   total = 13315
    assert_eq!(summaries[1].1.cost_ops, 13315);
    assert_eq!(summaries[1].1.arena_bytes, CostExpr::Const(0));
}

#[test]
fn generated_programs_check_clean() {
    let seed: u64 = 0x0d0c_2026_1030;
    for class in [Class::A, Class::B] {
        for s in 0..50u64 {
            let src = generate::program(seed ^ s, class, 5 + (s as usize % 56));
            check_source(&src).unwrap_or_else(|diags| {
                panic!("generated {class:?} seed {s} failed check_source:\n{src}\n{diags:#?}")
            });
        }
    }
}
