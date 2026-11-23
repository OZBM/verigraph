//! P3.8 and G3: examples/audio_limiter.vgs verifies. Its `@limit_frame` binds SHA-256 through
//! `@MESH_QUERY @sha256`, so the test publishes examples/sha256.vgs to a fresh project's Mesh,
//! resolves the query there, then proves every obligation in release mode (two solvers). Without
//! cvc5 and Bitwuzla it skips, unless VG_REQUIRE_SOLVERS=1.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value as J;

fn example(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name)
}

fn vg(args: &[&str]) -> J {
    let out = Command::new(env!("CARGO_BIN_EXE_vg"))
        .args(args)
        .output()
        .expect("vg runs");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "vg {args:?} printed no JSON ({e}): {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
fn audio_limiter_verifies_in_release_mode() {
    if vg_smt::find_solver("cvc5").is_none() || vg_smt::find_solver("bitwuzla").is_none() {
        assert!(
            std::env::var("VG_REQUIRE_SOLVERS").as_deref() != Ok("1"),
            "cvc5 or bitwuzla is not installed and VG_REQUIRE_SOLVERS=1"
        );
        eprintln!("skipped: cvc5 or bitwuzla is not installed");
        return;
    }
    let root = std::env::temp_dir().join(format!("vg-audio-limiter-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let r = root.to_str().unwrap();
    let sha = example("sha256.vgs");
    let limiter = example("audio_limiter.vgs");

    let published = vg(&["mesh", "publish", sha.to_str().unwrap(), "--root", r]);
    assert_eq!(published["ok"], true, "{published}");
    let resolved = vg(&["mesh", "resolve", limiter.to_str().unwrap(), "--root", r]);
    assert_eq!(resolved["ok"], true, "{resolved}");
    let out = vg(&[
        "verify",
        limiter.to_str().unwrap(),
        "--root",
        r,
        "--mode",
        "release",
        "--detail",
        "0",
    ]);
    let results = out["results"].as_array().expect("results");
    let open: Vec<String> = results
        .iter()
        .filter(|o| {
            // In release mode a proof record is reused only when two solvers made it (P3.5).
            o["verdict"] != "proven"
                || !(o["evidence"] == "two-solvers"
                    || o["evidence"] == "cached"
                    || (o["evidence"] == "prescreen"
                        && o["id"]
                            .as_str()
                            .is_some_and(|id| id.ends_with("/NONVACUOUS"))))
        })
        .map(|o| format!("{} {} {}", o["id"], o["verdict"], o["evidence"]))
        .collect();
    assert!(open.is_empty(), "not proven by two solvers: {open:?}");
    assert!(
        results
            .iter()
            .any(|o| o["id"].as_str().unwrap().starts_with("@limit_frame/")),
        "the node that calls SHA-256 through the Mesh is among them"
    );
    let _ = std::fs::remove_dir_all(&root);
}
