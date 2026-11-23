//! Gate G8 integration tests (docs/08, docs/09, docs/16):
//! 1. Lift zlib `crc32_z` and `adler32_z`, Mbed TLS C `mbedtls_sha256_compress`,
//!    and Rust `sha2` portable `compress256`.
//! 2. Verify Alive2 bounded translation validation at `unroll >= 8` and `1_000_000`
//!    differential fuzz cases per target.
//! 3. Ingest `SPEC @sha256_compress` and all lifted node graphs into `vg_mesh::Mesh`
//!    through the 6-check ingestion gate with provenance, attach `bounded(8)` attestations
//!    for both C and Rust SHA-256 lifts, and verify both join one spec class.
//! 4. Negative tests: mutated lifts fail Alive2 (`VG-L0002`) and differential fuzzing (`VG-L0003`).

use std::collections::BTreeMap;
use std::path::Path;

use ed25519_dalek::SigningKey;
use vg_core::ir::{Hash, hex};
use vg_core::value::Value;
use vg_interp::{NoEffectsHost, run};
use vg_mesh::{
    Evidence, Mesh, MeshContractQuery, Provenance as MeshProvenance, sign_attestation,
    sign_submission,
};
use vg_omnilift::ingest::Language;
use vg_omnilift::lift::{Lifted, lift_source};
use vg_omnilift::verify::{run_alive2_check, run_differential_fuzz};

fn repo_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

fn publish_lifted_to_mesh(mesh: &mut Mesh, lifted: &Lifted, sk: &SigningKey) {
    let mut seen = std::collections::BTreeSet::new();
    for a in &lifted.nodes {
        if !seen.insert(a.hashes.node) || mesh.get_node(&a.hashes.node).unwrap().is_some() {
            continue;
        }
        let mut sub = sign_submission(a.bytes.clone(), "omnilift", sk);
        sub.label = Some(a.label.clone());
        if a.hashes.node == lifted.node_hash {
            sub.provenance = Some(MeshProvenance {
                node_hash: lifted.node_hash,
                source: format!("{}::{}", lifted.provenance.path, lifted.provenance.function),
                commit: "HEAD".into(),
                compiler: lifted.provenance.compiler.clone(),
                flags: lifted.provenance.compiler_args.join(" "),
            });
        }
        let out = mesh.publish(sub);
        assert!(
            out.rejected.is_empty(),
            "Mesh rejected lifted node @{}: {:?}",
            a.label,
            out.rejected
        );
    }
}

#[test]
fn g8_lifts_four_targets_and_unifies_sha256_spec_class() {
    if std::env::var("VG_REQUIRE_ALIVE2").as_deref() != Ok("1")
        && (!vg_omnilift::verify::has_alive_tv() || !vg_omnilift::ingest::has_opt())
    {
        eprintln!("SKIP: alive-tv or opt not installed on this host");
        return;
    }
    let root = repo_root();

    // 1. Lift all four G8 targets (each runs Alive2 unroll=8 + 1,000,000 differential fuzz cases).
    let crc = lift_source(
        &root.join("tests/lift/zlib/crc32.c"),
        "crc32_z",
        Language::C,
        &[],
    )
    .expect("crc32_z lift succeeds");
    assert_eq!(crc.bounded_check, "alive2");
    assert!(crc.unroll >= 8);
    assert!(crc.fuzz_cases >= 1_000_000);

    let adler = lift_source(
        &root.join("tests/lift/zlib/adler32.c"),
        "adler32_z",
        Language::C,
        &[],
    )
    .expect("adler32_z lift succeeds");
    assert_eq!(adler.bounded_check, "alive2");
    assert!(adler.unroll >= 8);
    assert!(adler.fuzz_cases >= 1_000_000);

    let mbed = lift_source(
        &root.join("tests/lift/mbedtls/sha256_shim.c"),
        "mbedtls_sha256_compress",
        Language::C,
        &[],
    )
    .expect("mbedtls_sha256_compress lift succeeds");
    assert_eq!(mbed.bounded_check, "alive2");
    assert!(mbed.unroll >= 8);
    assert!(mbed.fuzz_cases >= 1_000_000);

    let rsha = lift_source(
        &root.join("tests/lift/rust-sha2/lib.rs"),
        "compress256",
        Language::Rust,
        &[],
    )
    .expect("rust-sha2 compress256 lift succeeds");
    assert_eq!(rsha.bounded_check, "alive2");
    assert!(rsha.unroll >= 8);
    assert!(rsha.fuzz_cases >= 1_000_000);

    // Structural hashes of C and Rust SHA-256 lifts are distinct, as docs/08 & docs/09 note.
    assert_ne!(mbed.node_hash, rsha.node_hash);

    // 2. Assemble SPEC @sha256_compress and check that SPEC, C lift, and Rust lift agree on
    //    FIPS 180-4 test blocks in vg-interp.
    let spec_src = std::fs::read_to_string(root.join("tests/lift/spec/sha256.vgs")).unwrap();
    let spec_nodes = vg_core::asm(&spec_src, &|_| None, false).expect("sha256.vgs assembles");
    let spec_assembled = spec_nodes
        .iter()
        .find(|a| a.label == "sha256_compress")
        .expect("@sha256_compress present");
    assert!(spec_assembled.node.spec);
    let spec_hash = spec_assembled.hashes.node;

    let spec_map: BTreeMap<Hash, _> = spec_nodes
        .iter()
        .map(|a| (a.hashes.node, a.node.clone()))
        .collect();
    let mbed_map: BTreeMap<Hash, _> = mbed
        .nodes
        .iter()
        .map(|a| (a.hashes.node, a.node.clone()))
        .collect();
    let rsha_map: BTreeMap<Hash, _> = rsha
        .nodes
        .iter()
        .map(|a| (a.hashes.node, a.node.clone()))
        .collect();

    let init_state: Vec<Value> = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ]
    .into_iter()
    .map(|w| Value::Int(i128::from(w)))
    .collect();
    let mut abc_block = [0u8; 64];
    abc_block[0] = b'a';
    abc_block[1] = b'b';
    abc_block[2] = b'c';
    abc_block[3] = 0x80;
    abc_block[63] = 24;
    let block_vals: Vec<Value> = abc_block
        .iter()
        .map(|&b| Value::Int(i128::from(b)))
        .collect();

    let mut host = NoEffectsHost;
    let out_spec = run(
        &spec_assembled.node,
        &[
            Value::Seq(init_state.clone()),
            Value::Seq(block_vals.clone()),
        ],
        &|h| spec_map.get(h).cloned(),
        &mut host,
    )
    .unwrap();
    let out_mbed = run(
        &mbed.nodes.last().unwrap().node,
        &[
            Value::Seq(init_state.clone()),
            Value::Seq(block_vals.clone()),
        ],
        &|h| mbed_map.get(h).cloned(),
        &mut host,
    )
    .unwrap();
    let out_rsha = run(
        &rsha.nodes.last().unwrap().node,
        &[Value::Seq(init_state), Value::Seq(block_vals)],
        &|h| rsha_map.get(h).cloned(),
        &mut host,
    )
    .unwrap();
    assert_eq!(out_spec, out_mbed);
    assert_eq!(out_spec, out_rsha);

    // 3. Publish SPEC @sha256_compress and all four lifted node graphs into VG-Mesh.
    let mesh_dir = std::env::temp_dir().join(format!("vg_g8_mesh_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&mesh_dir);
    let mut mesh = Mesh::open(&mesh_dir).expect("open mesh");
    let sk = SigningKey::from_bytes(&[0x42u8; 32]);

    for a in &spec_nodes {
        let mut sub = sign_submission(a.bytes.clone(), "omnilift", &sk);
        sub.label = Some(a.label.clone());
        if a.node.spec {
            sub.spec_name = Some(a.label.clone());
        }
        let out = mesh.publish(sub);
        assert!(
            out.rejected.is_empty(),
            "Mesh rejected spec node @{}: {:?}",
            a.label,
            out.rejected
        );
    }

    publish_lifted_to_mesh(&mut mesh, &crc, &sk);
    publish_lifted_to_mesh(&mut mesh, &adler, &sk);
    publish_lifted_to_mesh(&mut mesh, &mbed, &sk);
    publish_lifted_to_mesh(&mut mesh, &rsha, &sk);

    // Record bounded(8) spec-class attestations for both C and Rust SHA-256 lifts.
    let att_mbed = sign_attestation(
        mbed.node_hash,
        spec_hash,
        "alive2+fuzz",
        Evidence::Bounded(u64::from(mbed.unroll)),
        "omnilift",
        &sk,
    );
    mesh.add_attestation(att_mbed)
        .expect("mbedtls attestation accepted");

    let att_rsha = sign_attestation(
        rsha.node_hash,
        spec_hash,
        "alive2+fuzz",
        Evidence::Bounded(u64::from(rsha.unroll)),
        "omnilift",
        &sk,
    );
    mesh.add_attestation(att_rsha)
        .expect("rust-sha2 attestation accepted");

    // 4. Query VG-Mesh for the `sha256_compress` spec class and verify both C and Rust lifts
    //    are returned in the same spec class with `bounded(8)` evidence and provenance intact.
    let q = MeshContractQuery {
        input_types: vec!["Array<u32, 8>".into(), "Array<u8, 64>".into()],
        output_types: vec!["Array<u32, 8>".into()],
        max_effects: vec![],
        pre: None,
        post: None,
        max_cost: None,
        intent: None,
        spec: Some("sha256_compress".into()),
        min_evidence: Some(Evidence::Bounded(8)),
        target: None,
        limit: Some(10),
    };
    let cands = mesh.query(&q, 10).expect("spec class query succeeds");
    assert_eq!(
        cands.len(),
        2,
        "expected both C and Rust SHA-256 lifts in spec class, got: {cands:?}"
    );
    let returned_hashes: Vec<Hash> = cands.iter().map(|c| c.node_hash).collect();
    assert!(
        returned_hashes.contains(&mbed.node_hash),
        "C SHA-256 lift {} missing from spec class",
        hex(&mbed.node_hash)
    );
    assert!(
        returned_hashes.contains(&rsha.node_hash),
        "Rust SHA-256 lift {} missing from spec class",
        hex(&rsha.node_hash)
    );
    for c in &cands {
        assert_eq!(c.spec_hash, Some(spec_hash));
        assert_eq!(c.evidence, "bounded(8)");
        let prov = mesh
            .provenance_for(&c.node_hash)
            .unwrap()
            .expect("provenance recorded");
        assert!(!prov.compiler.is_empty());
        assert!(!prov.flags.is_empty());
    }

    mesh.verify_log_integrity()
        .expect("C2SP transparency log integrity verified");
    let _ = std::fs::remove_dir_all(&mesh_dir);
}

#[test]
fn g8_rejects_mutated_lift_with_vg_l0002_and_vg_l0003() {
    if std::env::var("VG_REQUIRE_ALIVE2").as_deref() != Ok("1")
        && (!vg_omnilift::verify::has_alive_tv() || !vg_omnilift::ingest::has_opt())
    {
        eprintln!("SKIP: alive-tv or opt not installed on this host");
        return;
    }
    let dir = std::env::temp_dir().join(format!("vg_g8_neg_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let c_path = dir.join("bitmix.c");
    std::fs::write(
        &c_path,
        r#"
#include <stdint.h>
uint32_t bitmix(uint32_t a, uint32_t b) {
    return ((a << 5) ^ (b >> 3)) + (a & 0xffu);
}
"#,
    )
    .unwrap();

    let lifted = lift_source(&c_path, "bitmix", Language::C, &[]).unwrap();

    // Mutate the lifted VG-Sym and node graph (`ADD.wrap` -> `SUB.wrap`).
    let mut bad = lifted.clone();
    bad.vgsym = bad.vgsym.replace("ADD.wrap", "SUB.wrap");
    bad.nodes = vg_core::asm(&bad.vgsym, &|_| None, false).expect("mutated vgsym assembles");
    bad.node_hash = bad.nodes.last().unwrap().hashes.node;

    let alive_err =
        run_alive2_check(&bad, 8).expect_err("Alive2 must reject mutated ADD->SUB lift");
    assert_eq!(alive_err.code, "VG-L0002", "{alive_err}");

    let fuzz_err = run_differential_fuzz(&bad, 1_000)
        .expect_err("Differential fuzzer must reject mutated ADD->SUB lift");
    assert_eq!(fuzz_err.code, "VG-L0003", "{fuzz_err}");

    let _ = std::fs::remove_dir_all(&dir);
}
