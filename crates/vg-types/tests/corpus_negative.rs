use std::fs;
use std::path::{Path, PathBuf};

use vg_core::parser;
use vg_types::check_source;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bad_vgs_files_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let rd = fs::read_dir(dir).unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()));
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "vgs") {
            out.push(p);
        }
    }
    out.sort();
    out
}

#[test]
fn types_bad_corpus_rejected_with_expected_codes() {
    let dir = repo().join("tests/corpus/errors");
    let files = bad_vgs_files_sorted(&dir);
    assert!(
        files.len() >= 50,
        "expected at least 50 negative corpus cases, found {}",
        files.len()
    );

    for f in &files {
        let src = fs::read_to_string(f).unwrap();
        let first_line = src.lines().next().unwrap_or("");
        let expected_code = first_line
            .strip_prefix("// expect:")
            .map(str::trim)
            .unwrap_or_else(|| {
                panic!(
                    "{}: first line must be `// expect: VG-xxxx`, found {first_line:?}",
                    f.display()
                )
            });

        // Every file in tests/corpus/errors/ must be syntactically valid VG-Sym.
        parser::parse(&src).unwrap_or_else(|e| {
            panic!(
                "{}: expected valid VG-Sym syntax, got parse error {e}",
                f.display()
            )
        });

        let diags = check_source(&src).expect_err(&format!(
            "{}: expected rejection with {expected_code}, but check_source succeeded",
            f.display()
        ));
        assert!(
            diags.iter().any(|d| d.code == expected_code),
            "{}: expected diagnostic code {expected_code}, got: {diags:#?}",
            f.display()
        );
    }
}
