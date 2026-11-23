//! examples/sha256.vgs, the node M1's audio limiter binds through `@MESH_QUERY @sha256`: its digest
//! matches FIPS 180-4 and Python's hashlib in vg-interp, and every obligation, `@sha256/POST.0:
//! result == spec:sha256_spec(m)` included, is proven.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value as J;

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/sha256.vgs")
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

fn hex(bytes: &[J]) -> String {
    bytes
        .iter()
        .map(|b| format!("{:02x}", b.as_u64().expect("a byte")))
        .collect()
}

#[test]
fn digests_match_fips_180_4() {
    // The first four are the FIPS 180-4 example messages; the digests come from hashlib.
    let cases: [(&[u8], &str); 5] = [
        (b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
        (b"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
        (
            b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu",
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1",
        ),
        (
            &[b'a'; 1000],
            "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3",
        ),
    ];
    let path = example();
    for (m, want) in cases {
        let inputs = serde_json::json!({ "m": m }).to_string();
        let out = vg(&["run", path.to_str().unwrap(), "@sha256", &inputs]);
        assert_eq!(out["ok"], true, "{out}");
        let got = hex(out["outputs"][0].as_array().expect("a digest"));
        assert_eq!(got, want, "digest of a {}-byte message", m.len());
    }
}

#[test]
fn every_obligation_is_proven() {
    let out = vg(&["verify", example().to_str().unwrap(), "--timeout", "60000"]);
    let results = out["results"].as_array().expect("results");
    let open: Vec<_> = results
        .iter()
        .filter(|r| r["verdict"] != "proven")
        .map(|r| format!("{} {}", r["id"], r["verdict"]))
        .collect();
    assert!(open.is_empty(), "not proven: {open:?}");
    assert!(
        results.iter().any(|r| r["id"] == "@sha256/POST.0"),
        "the refinement of the SPEC is among the obligations"
    );
}
