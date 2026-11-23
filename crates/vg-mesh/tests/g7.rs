//! Gate G7 integration, tamper-detection, loopback HTTP, and 100k query benchmark tests.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ed25519_dalek::{Signer, SigningKey};
use rusqlite::Connection;
use serde_json::{Value as J, json};
use vg_core::ir::{Bin, Const, Contract, Hash, Node, Op, Operand, Term, hex};
use vg_core::types::{EffectMask, Type};
use vg_mesh::{
    Benchmark, Evidence, Mesh, MeshContractQuery, Provenance, SyntheticEntry, leaf_hash,
    serve_shared, sign_attestation, sign_name_ref, sign_submission, submission_statement,
    verify_checkpoint,
};

static NEXT_TMP: AtomicU64 = AtomicU64::new(1);

fn temp_dir(tag: &str) -> PathBuf {
    let id = NEXT_TMP.fetch_add(1, Ordering::Relaxed);
    let d = std::env::temp_dir().join(format!("vg-mesh-g7-{tag}-{}-{id}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn sk(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn fixture(name: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/mesh")
        .join(name);
    fs::read_to_string(&root).unwrap_or_else(|e| panic!("read {}: {e}", root.display()))
}

fn asm_with(src: &str, map: &BTreeMap<String, (Hash, Option<Hash>)>) -> Vec<vg_core::Assembled> {
    vg_core::asm(
        src,
        &|label| map.get(label.trim_start_matches('@')).copied(),
        true,
    )
    .expect("assemble fixture")
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[test]
fn objects_and_persistence() {
    let dir = temp_dir("objects");
    let signer_sk = sk(11);
    let spec_hash;
    let impl_hash;

    {
        let mut mesh = Mesh::open(&dir).unwrap();

        // Publish SPEC node.
        let spec_asm = asm_with(&fixture("spec_add1.vgs"), &BTreeMap::new());
        let mut sub_spec = sign_submission(spec_asm[0].bytes.clone(), "alice", &signer_sk);
        sub_spec.label = Some("spec_add1".into());
        sub_spec.spec_name = Some("spec_add1".into());
        let out_spec = mesh.publish(sub_spec);
        assert!(out_spec.rejected.is_empty(), "{:?}", out_spec.rejected);
        spec_hash = out_spec.accepted[0];

        // Publish implementation refining spec_add1 with Provenance.
        let mut map = BTreeMap::new();
        map.insert("spec_add1".into(), (spec_hash, Some(spec_hash)));
        let impls = asm_with(&fixture("impl_add1.vgs"), &map);
        let mut sub_impl = sign_submission(impls[0].bytes.clone(), "alice", &signer_sk);
        sub_impl.label = Some("impl_add1".into());
        sub_impl.provenance = Some(Provenance {
            node_hash: impls[0].hashes.node,
            source: "tests/mesh/impl_add1.vgs".into(),
            commit: "deadbeef".into(),
            compiler: "vg-asm".into(),
            flags: "--keep-debug".into(),
        });
        let out_impl = mesh.publish(sub_impl);
        assert!(out_impl.rejected.is_empty(), "{:?}", out_impl.rejected);
        impl_hash = out_impl.accepted[0];

        // Benchmark object.
        mesh.add_benchmark(Benchmark {
            node_hash: impl_hash,
            target: "aarch64-apple-darwin".into(),
            cycles_per_byte: 0.42,
            p50_us: 1.1,
            p99_us: 2.3,
        })
        .unwrap();

        // Signed Attestation object.
        let att = sign_attestation(
            impl_hash,
            spec_hash,
            "external-checker-v1",
            Evidence::Proved,
            "bob",
            &sk(22),
        );
        mesh.add_attestation(att).unwrap();

        // Signed Name object, then move it.
        let name1 = sign_name_ref("add1-latest", spec_hash, "alice", &signer_sk);
        mesh.set_name(name1).unwrap();
        assert_eq!(
            mesh.resolve_name("add1-latest")
                .unwrap()
                .unwrap()
                .target_hash,
            spec_hash
        );
        let name2 = sign_name_ref("add1-latest", impl_hash, "alice", &signer_sk);
        mesh.set_name(name2).unwrap();
        assert_eq!(
            mesh.resolve_name("add1-latest")
                .unwrap()
                .unwrap()
                .target_hash,
            impl_hash
        );

        mesh.verify_log_integrity().unwrap();
    }

    // Reopen from disk and check every object reloaded cleanly.
    let mesh2 = Mesh::open(&dir).unwrap();
    assert!(mesh2.get_node(&spec_hash).unwrap().is_some());
    assert!(mesh2.get_node(&impl_hash).unwrap().is_some());
    let spec_rec = mesh2.spec(&spec_hash).unwrap().unwrap();
    assert_eq!(spec_rec.name.as_deref(), Some("spec_add1"));
    assert_eq!(spec_rec.input_types, vec!["u32"]);
    assert_eq!(spec_rec.output_types, vec!["u32"]);

    assert!(!mesh2.proofs_for(&impl_hash).unwrap().is_empty());
    assert!(mesh2.attestations_for(&impl_hash).unwrap().len() >= 2);
    let prov = mesh2.provenance_for(&impl_hash).unwrap().unwrap();
    assert_eq!(prov.commit, "deadbeef");
    assert_eq!(
        mesh2
            .resolve_name("add1-latest")
            .unwrap()
            .unwrap()
            .target_hash,
        impl_hash
    );

    let q = MeshContractQuery {
        spec: Some("spec_add1".into()),
        ..Default::default()
    };
    let cands = mesh2.query(&q, 10).unwrap();
    assert_eq!(cands.len(), 1);
    assert_eq!(cands[0].node_hash, impl_hash);
    assert_eq!(cands[0].evidence, "proved");
    assert_eq!(cands[0].benchmarks.len(), 1);
    mesh2.verify_log_integrity().unwrap();

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn spec_class_and_query_pipeline() {
    let dir = temp_dir("pipeline");
    let mut mesh = Mesh::open(&dir).unwrap();
    let signer_sk = sk(33);

    // 1. Publish SPEC + two implementations (different op counts).
    let spec_asm = asm_with(&fixture("spec_add1.vgs"), &BTreeMap::new());
    let mut sub_spec = sign_submission(spec_asm[0].bytes.clone(), "alice", &signer_sk);
    sub_spec.label = Some("spec_add1".into());
    sub_spec.spec_name = Some("spec_add1".into());
    let spec_hash = mesh.publish(sub_spec).accepted[0];

    let mut map = BTreeMap::new();
    map.insert("spec_add1".into(), (spec_hash, Some(spec_hash)));
    let impls = asm_with(&fixture("impl_add1.vgs"), &map);
    assert_eq!(impls.len(), 2);

    let mut sub1 = sign_submission(impls[0].bytes.clone(), "alice", &signer_sk);
    sub1.label = Some("impl_add1".into());
    let mut sub2 = sign_submission(impls[1].bytes.clone(), "alice", &signer_sk);
    sub2.label = Some("impl_add1_alt".into());
    let pub_out = mesh.publish_many(vec![sub1, sub2]);
    assert_eq!(pub_out.accepted.len(), 2, "{:?}", pub_out.rejected);
    let h_fast = pub_out.accepted[0];
    let h_slow = pub_out.accepted[1];

    // Also publish an ARENA node to test effect-mask filtering.
    let arena_asm = asm_with(&fixture("callee_arena.vgs"), &BTreeMap::new());
    let mut sub_k = sign_submission(arena_asm[0].bytes.clone(), "alice", &signer_sk);
    sub_k.label = Some("zero_u32".into());
    let h_k = mesh.publish(sub_k).accepted[0];
    assert_eq!(h_k, arena_asm[0].hashes.node);
    let mut sub_arena = sign_submission(arena_asm[1].bytes.clone(), "alice", &signer_sk);
    sub_arena.label = Some("make_zeros".into());
    let h_arena = mesh.publish(sub_arena).accepted[0];

    // Add benchmarks.
    mesh.add_benchmark(Benchmark {
        node_hash: h_fast,
        target: "aarch64-apple-darwin".into(),
        cycles_per_byte: 0.5,
        p50_us: 1.0,
        p99_us: 1.5,
    })
    .unwrap();
    mesh.add_benchmark(Benchmark {
        node_hash: h_slow,
        target: "aarch64-apple-darwin".into(),
        cycles_per_byte: 1.2,
        p50_us: 2.0,
        p99_us: 3.0,
    })
    .unwrap();

    // Step 1: exact spec match by name and by hex hash.
    let q_spec = MeshContractQuery {
        spec: Some("spec_add1".into()),
        ..Default::default()
    };
    let res_spec = mesh.query_indexed(&q_spec, 10).unwrap();
    assert_eq!(res_spec.len(), 2);
    assert_eq!(res_spec[0].node_hash, h_fast);
    assert_eq!(res_spec[1].node_hash, h_slow);

    let q_spec_hex = MeshContractQuery {
        spec: Some(hex(&spec_hash)),
        ..Default::default()
    };
    assert_eq!(mesh.query_indexed(&q_spec_hex, 10).unwrap().len(), 2);

    // Step 2: structural filter + effect mask filtering.
    let q_arena_denied = MeshContractQuery {
        input_types: vec!["u64".into()],
        output_types: vec!["Slice<u32>".into()],
        max_effects: vec![],
        ..Default::default()
    };
    assert!(mesh.query_indexed(&q_arena_denied, 10).unwrap().is_empty());

    let q_arena_allowed = MeshContractQuery {
        input_types: vec!["u64".into()],
        output_types: vec!["Slice<u32>".into()],
        max_effects: vec!["ARENA".into()],
        ..Default::default()
    };
    let arena_res = mesh.query_indexed(&q_arena_allowed, 10).unwrap();
    assert_eq!(arena_res.len(), 1);
    assert_eq!(arena_res[0].node_hash, h_arena);

    // Step 3: semantic rank favors closer intent.
    let q_sem = MeshContractQuery {
        input_types: vec!["u32".into()],
        output_types: vec!["u32".into()],
        intent: Some("Fast single-op u32 increment".into()),
        ..Default::default()
    };
    let sem_res = mesh.query_indexed(&q_sem, 10).unwrap();
    assert_eq!(sem_res[0].node_hash, h_fast);

    // Step 4: behavioral subtyping via vg-smt.
    // Stronger PRE (arg0 < 500 => arg0 < 1000) and matching POST -> passes!
    let q_behav_ok = MeshContractQuery {
        input_types: vec!["u32".into()],
        output_types: vec!["u32".into()],
        pre: Some("arg0 < 500".into()),
        post: Some("result == arg0 + 1".into()),
        ..Default::default()
    };
    let ok_res = mesh.query(&q_behav_ok, 10).unwrap();
    assert_eq!(ok_res.len(), 2);
    assert_eq!(ok_res[0].node_hash, h_fast);
    // Repeated query uses persistent behavioral cache cleanly.
    assert_eq!(mesh.query(&q_behav_ok, 10).unwrap().len(), 2);

    // Behavioral check with spec: reference in query POST.
    let q_behav_spec = MeshContractQuery {
        input_types: vec!["u32".into()],
        output_types: vec!["u32".into()],
        pre: Some("arg0 < 500".into()),
        post: Some("result == spec:spec_add1(arg0)".into()),
        ..Default::default()
    };
    assert_eq!(mesh.query(&q_behav_spec, 10).unwrap().len(), 2);

    // Weaker PRE (arg0 < 2000 does NOT imply arg0 < 1000) -> rejected!
    let q_behav_bad_pre = MeshContractQuery {
        input_types: vec!["u32".into()],
        output_types: vec!["u32".into()],
        pre: Some("arg0 < 2000".into()),
        post: Some("result == arg0 + 1".into()),
        ..Default::default()
    };
    assert!(mesh.query(&q_behav_bad_pre, 10).unwrap().is_empty());

    // Wrong POST (result == arg0 + 2) -> rejected!
    let q_behav_bad_post = MeshContractQuery {
        input_types: vec!["u32".into()],
        output_types: vec!["u32".into()],
        pre: Some("arg0 < 500".into()),
        post: Some("result == arg0 + 2".into()),
        ..Default::default()
    };
    assert!(mesh.query(&q_behav_bad_post, 10).unwrap().is_empty());

    fs::remove_dir_all(&dir).unwrap();
}

fn parse_http_response(buf: &[u8]) -> (u16, String, Vec<u8>) {
    let sep = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("HTTP response header separator");
    let head = String::from_utf8_lossy(&buf[..sep]).to_string();
    let status: u16 = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let body = buf[sep + 4..].to_vec();
    (status, head, body)
}

fn send_http(addr: std::net::SocketAddr, raw_req: &[u8]) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(raw_req).unwrap();
    stream.flush().unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    parse_http_response(&buf)
}

fn send_http_unix(mesh: &Arc<Mutex<Mesh>>, raw_req: &[u8]) -> (u16, String, Vec<u8>) {
    let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
    let m = Arc::clone(mesh);
    let jh = std::thread::spawn(move || {
        vg_mesh::handle_stream(&m, server);
    });
    client.write_all(raw_req).unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    let mut buf = Vec::new();
    client.read_to_end(&mut buf).unwrap();
    jh.join().unwrap();
    parse_http_response(&buf)
}

type HttpSender<'a> = &'a dyn Fn(&[u8]) -> (u16, String, Vec<u8>);

fn run_http_protocol_suite(mesh: &Arc<Mutex<Mesh>>, send: HttpSender<'_>) {
    // 1. POST /publish
    let signer_sk = sk(55);
    let spec_asm = asm_with(&fixture("spec_add1.vgs"), &BTreeMap::new());
    let stmt = submission_statement(&spec_asm[0].hashes.node, "http-alice");
    let sig = signer_sk.sign(stmt.as_bytes());
    let pub_body = serde_json::to_vec(&json!({
        "vgir_hex": encode_hex(&spec_asm[0].bytes),
        "signer": "http-alice",
        "public_key": hex(&signer_sk.verifying_key().to_bytes()),
        "signature": encode_hex(&sig.to_bytes()),
        "label": "spec_add1",
        "spec_name": "spec_add1"
    }))
    .unwrap();
    let req = format!(
        "POST /publish HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        pub_body.len()
    );
    let mut raw = req.into_bytes();
    raw.extend_from_slice(&pub_body);
    let (status, _, body) = send(&raw);
    assert_eq!(status, 200);
    let pub_json: J = serde_json::from_slice(&body).unwrap();
    assert_eq!(pub_json["accepted"].as_array().unwrap().len(), 1);

    // Publish an implementation too, then POST /query.
    let mut map = BTreeMap::new();
    map.insert(
        "spec_add1".into(),
        (spec_asm[0].hashes.node, Some(spec_asm[0].hashes.node)),
    );
    let impls = asm_with(&fixture("impl_add1.vgs"), &map);
    let impl_stmt = submission_statement(&impls[0].hashes.node, "http-alice");
    let impl_sig = signer_sk.sign(impl_stmt.as_bytes());
    let pub_impl = serde_json::to_vec(&json!({
        "vgir_hex": encode_hex(&impls[0].bytes),
        "signer": "http-alice",
        "public_key": hex(&signer_sk.verifying_key().to_bytes()),
        "signature": encode_hex(&impl_sig.to_bytes()),
        "label": "impl_add1"
    }))
    .unwrap();
    let mut raw2 = format!(
        "POST /publish HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
        pub_impl.len()
    )
    .into_bytes();
    raw2.extend_from_slice(&pub_impl);
    let (status, _, _) = send(&raw2);
    assert_eq!(status, 200);

    let q_body = serde_json::to_vec(&json!({
        "input_types": ["u32"],
        "output_types": ["u32"],
        "max_effects": [],
        "pre": "arg0 < 500",
        "post": "result == arg0 + 1",
        "limit": 5
    }))
    .unwrap();
    let mut q_raw = format!(
        "POST /query HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
        q_body.len()
    )
    .into_bytes();
    q_raw.extend_from_slice(&q_body);
    let (status, _, q_resp) = send(&q_raw);
    assert_eq!(status, 200);
    let q_json: J = serde_json::from_slice(&q_resp).unwrap();
    let cands = q_json["candidates"].as_array().unwrap();
    assert_eq!(cands.len(), 1);
    assert_eq!(
        cands[0]["node_hash"].as_str().unwrap(),
        hex(&impls[0].hashes.node)
    );

    // 2. GET /checkpoint -> valid C2SP signed checkpoint
    let (status, _, cp_bytes) = send(b"GET /checkpoint HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_eq!(status, 200);
    let cp_text = String::from_utf8(cp_bytes).unwrap();
    let (origin, key_name, vk) = {
        let g = mesh.lock().unwrap();
        (
            g.log_origin().to_string(),
            g.log_key_name().to_string(),
            g.log_verifying_key(),
        )
    };
    let cp = verify_checkpoint(&cp_text, &origin, &key_name, &vk).unwrap();
    assert_eq!(cp.size, 2);

    // 3. GET /tile/0/000.p/2 -> 64 bytes of leaf hashes
    let (status, _, tile_bytes) = send(b"GET /tile/0/000.p/2 HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_eq!(status, 200);
    assert_eq!(tile_bytes.len(), 64);

    // Check first leaf hash matches signed note of first entry.
    let expected_note =
        vg_mesh::tlog::format_signed_note(&stmt, "http-alice", &signer_sk.verifying_key(), &sig);
    assert_eq!(&tile_bytes[..32], &leaf_hash(expected_note.as_bytes()));

    // 4. Reject path traversal, malformed JSON, malformed HTTP, and > 16 MiB body with 400.
    let (s_trav, _, _) = send(b"GET /tile/../mesh.sqlite HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert_eq!(s_trav, 400);

    let (s_bad_json, _, _) =
        send(b"POST /query HTTP/1.1\r\nHost: localhost\r\nContent-Length: 3\r\n\r\n{{{");
    assert_eq!(s_bad_json, 400);

    let (s_bad_http, _, _) = send(b"GARBAGE\r\n\r\n");
    assert_eq!(s_bad_http, 400);

    let (s_too_big, _, _) =
        send(b"POST /publish HTTP/1.1\r\nHost: localhost\r\nContent-Length: 16777217\r\n\r\n");
    assert_eq!(s_too_big, 400);
}

#[test]
fn http_api_socketpair() {
    let dir = temp_dir("http_unix");
    let mesh = Arc::new(Mutex::new(Mesh::open(&dir).unwrap()));
    run_http_protocol_suite(&mesh, &|req| send_http_unix(&mesh, req));
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn http_api_loopback() {
    let dir = temp_dir("http");
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("http_api_loopback skipped under sandbox: {e}");
            fs::remove_dir_all(&dir).unwrap();
            return;
        }
        Err(e) => panic!("bind loopback: {e}"),
    };
    let addr = listener.local_addr().unwrap();
    // Probe TCP connect on a separate throwaway listener so the server listener's backlog stays clean.
    if let Ok(probe_listener) = TcpListener::bind("127.0.0.1:0") {
        let probe_addr = probe_listener.local_addr().unwrap();
        if let Err(e) = TcpStream::connect(probe_addr)
            && e.kind() == std::io::ErrorKind::PermissionDenied
        {
            eprintln!("http_api_loopback connect skipped under sandbox: {e}");
            drop(probe_listener);
            drop(listener);
            fs::remove_dir_all(&dir).unwrap();
            return;
        }
    }
    let mesh = Arc::new(Mutex::new(Mesh::open(&dir).unwrap()));
    let shutdown = Arc::new(AtomicBool::new(false));

    let m_srv = Arc::clone(&mesh);
    let sd_srv = Arc::clone(&shutdown);
    let handle = std::thread::spawn(move || serve_shared(m_srv, listener, sd_srv).unwrap());

    run_http_protocol_suite(&mesh, &|req| send_http(addr, req));

    shutdown.store(true, Ordering::Release);
    handle.join().unwrap();
    fs::remove_dir_all(&dir).unwrap();
}

// ---------- Gate G7 Tamper Tests ----------

#[test]
fn tamper_flipped_vgir_byte() {
    let dir = temp_dir("tamper_flip");
    let mut mesh = Mesh::open(&dir).unwrap();
    let spec_asm = asm_with(&fixture("spec_add1.vgs"), &BTreeMap::new());
    let mut bytes = spec_asm[0].bytes.clone();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    let mut sub = sign_submission(bytes, "alice", &sk(1));
    sub.claimed_hash = Some(spec_asm[0].hashes.node);
    let out = mesh.publish(sub);
    assert!(out.accepted.is_empty());
    assert_eq!(out.rejected.len(), 1);
    assert_eq!(out.rejected[0].check, 1);
    assert_eq!(out.rejected[0].code, "VG-M0001");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tamper_wrong_claimed_hash() {
    let dir = temp_dir("tamper_hash");
    let mut mesh = Mesh::open(&dir).unwrap();
    let spec_asm = asm_with(&fixture("spec_add1.vgs"), &BTreeMap::new());
    let mut sub = sign_submission(spec_asm[0].bytes.clone(), "alice", &sk(2));
    sub.claimed_hash = Some([0xAA; 32]);
    let out = mesh.publish(sub);
    assert_eq!(out.rejected[0].check, 1);
    assert_eq!(out.rejected[0].code, "VG-M0001");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tamper_type_error() {
    let dir = temp_dir("tamper_type");
    let mut mesh = Mesh::open(&dir).unwrap();
    // Construct a canonical .vgir node with a type mismatch (RETURN u32 when SIGNATURE returns Bool).
    let bad_node = Node {
        spec: false,
        effects: EffectMask(0),
        params: vec![Type::UInt(32)],
        results: vec![Type::Bool],
        learnable: vec![],
        consts: vec![],
        calls: vec![],
        ops: vec![],
        outputs: vec![Operand::Input(0)],
        contract: Contract::default(),
        debug: None,
    };
    let (bytes, _) = vg_core::codec::encode(&bad_node);
    let sub = sign_submission(bytes, "alice", &sk(3));
    let out = mesh.publish(sub);
    assert_eq!(out.rejected[0].check, 2);
    assert_eq!(out.rejected[0].code, "VG-M0002");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tamper_unverified_contract() {
    let dir = temp_dir("tamper_smt");
    let mut mesh = Mesh::open(&dir).unwrap();
    let bad_src = "module bad_post;
NODE @bad_inc {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE: x < 100;
    POST: result == x + 2;
  }
  BODY {
    %0 = ADD.checked(x, 1:u32);
    RETURN(%0);
  }
}";
    let bad_asm = asm_with(bad_src, &BTreeMap::new());
    let sub = sign_submission(bad_asm[0].bytes.clone(), "alice", &sk(4));
    let out = mesh.publish(sub);
    assert_eq!(out.rejected[0].check, 3);
    assert_eq!(out.rejected[0].code, "VG-M0003");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tamper_understated_effect_mask() {
    let dir = temp_dir("tamper_eff");
    let mut mesh = Mesh::open(&dir).unwrap();
    let signer = sk(5);

    // First publish the genuine ARENA callee.
    let arena_asm = asm_with(&fixture("callee_arena.vgs"), &BTreeMap::new());
    let sub_k = sign_submission(arena_asm[0].bytes.clone(), "alice", &signer);
    let sub_c = sign_submission(arena_asm[1].bytes.clone(), "alice", &signer);
    assert_eq!(mesh.publish_many(vec![sub_k, sub_c]).accepted.len(), 2);
    let callee_hash = arena_asm[1].hashes.node;

    // Now build a caller that calls `callee_hash` via CALL_HASH but claims EFFECTS: [].
    let caller_node = Node {
        spec: false,
        effects: EffectMask(0),
        params: vec![Type::UInt(64)],
        results: vec![Type::Slice(Box::new(Type::UInt(32)))],
        learnable: vec![],
        consts: vec![],
        calls: vec![callee_hash],
        ops: vec![Op {
            opcode: 0x0300, // CALL_HASH
            mode: 0,
            operands: vec![Operand::Callee(0), Operand::Input(0)],
            results: vec![Type::Slice(Box::new(Type::UInt(32)))],
        }],
        outputs: vec![Operand::Op(0, 0)],
        contract: Contract::default(),
        debug: None,
    };
    let (bytes, _) = vg_core::codec::encode(&caller_node);
    let sub = sign_submission(bytes, "alice", &signer);
    let out = mesh.publish(sub);
    assert_eq!(out.rejected[0].check, 4);
    assert_eq!(out.rejected[0].code, "VG-M0004");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tamper_missing_callee() {
    let dir = temp_dir("tamper_missing_callee");
    let mut mesh = Mesh::open(&dir).unwrap();
    let caller_node = Node {
        spec: false,
        effects: EffectMask(0),
        params: vec![Type::UInt(32)],
        results: vec![Type::UInt(32)],
        learnable: vec![],
        consts: vec![],
        calls: vec![[0xDE; 32]],
        ops: vec![Op {
            opcode: 0x0300, // CALL_HASH
            mode: 0,
            operands: vec![Operand::Callee(0), Operand::Input(0)],
            results: vec![Type::UInt(32)],
        }],
        outputs: vec![Operand::Op(0, 0)],
        contract: Contract::default(),
        debug: None,
    };
    let (bytes, _) = vg_core::codec::encode(&caller_node);
    let sub = sign_submission(bytes, "alice", &sk(6));
    let out = mesh.publish(sub);
    assert_eq!(out.rejected[0].check, 4);
    assert_eq!(out.rejected[0].code, "VG-M0004");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tamper_forged_signature() {
    let dir = temp_dir("tamper_forged_sig");
    let mut mesh = Mesh::open(&dir).unwrap();
    let spec_asm = asm_with(&fixture("spec_add1.vgs"), &BTreeMap::new());
    let mut sub = sign_submission(spec_asm[0].bytes.clone(), "alice", &sk(7));
    if let Some(sig) = sub.signature.as_mut() {
        sig[0] ^= 0xFF;
    }
    let out = mesh.publish(sub);
    assert_eq!(out.rejected[0].check, 5);
    assert_eq!(out.rejected[0].code, "VG-M0005");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tamper_missing_signature() {
    let dir = temp_dir("tamper_missing_sig");
    let mut mesh = Mesh::open(&dir).unwrap();
    let spec_asm = asm_with(&fixture("spec_add1.vgs"), &BTreeMap::new());
    let mut sub = sign_submission(spec_asm[0].bytes.clone(), "alice", &sk(8));
    sub.signature = None;
    let out = mesh.publish(sub);
    assert_eq!(out.rejected[0].check, 5);
    assert_eq!(out.rejected[0].code, "VG-M0005");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tamper_reordered_log() {
    let dir = temp_dir("tamper_reordered");
    let signer = sk(9);
    let cp_before;
    {
        let mut mesh = Mesh::open(&dir).unwrap();
        let spec_asm = asm_with(&fixture("spec_add1.vgs"), &BTreeMap::new());
        let mut sub_spec = sign_submission(spec_asm[0].bytes.clone(), "alice", &signer);
        sub_spec.spec_name = Some("spec_add1".into());
        let mut map = BTreeMap::new();
        map.insert(
            "spec_add1".into(),
            (spec_asm[0].hashes.node, Some(spec_asm[0].hashes.node)),
        );
        let impls = asm_with(&fixture("impl_add1.vgs"), &map);
        let sub1 = sign_submission(impls[0].bytes.clone(), "alice", &signer);
        let out = mesh.publish_many(vec![sub_spec, sub1]);
        assert_eq!(out.accepted.len(), 2);
        cp_before = mesh.checkpoint();
    }

    // Swap entries 0 and 1 in SQLite `log_entries`.
    {
        let db = Connection::open(dir.join("mesh.sqlite")).unwrap();
        db.execute_batch(
            "UPDATE log_entries SET entry_index = 99 WHERE entry_index = 0;
             UPDATE log_entries SET entry_index = 0 WHERE entry_index = 1;
             UPDATE log_entries SET entry_index = 1 WHERE entry_index = 99;",
        )
        .unwrap();
    }

    let mesh_reopened = Mesh::open(&dir).unwrap();
    assert!(mesh_reopened.verify_log_integrity().is_err());

    // Also verify consistency proof fails against the pre-reorder checkpoint if root changed,
    // or if we construct a reordered 1-entry checkpoint from the swapped log.
    let swapped_root = mesh_reopened.checkpoint().root;
    assert_ne!(swapped_root, cp_before.root);
    assert!(mesh_reopened.verify_log_consistency(&cp_before).is_err());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tamper_truncated_log() {
    let dir = temp_dir("tamper_truncated");
    let signer = sk(10);
    let cp_size2;
    {
        let mut mesh = Mesh::open(&dir).unwrap();
        let spec_asm = asm_with(&fixture("spec_add1.vgs"), &BTreeMap::new());
        let sub_spec = sign_submission(spec_asm[0].bytes.clone(), "alice", &signer);
        let mut map = BTreeMap::new();
        map.insert(
            "spec_add1".into(),
            (spec_asm[0].hashes.node, Some(spec_asm[0].hashes.node)),
        );
        let impls = asm_with(&fixture("impl_add1.vgs"), &map);
        let sub1 = sign_submission(impls[0].bytes.clone(), "alice", &signer);
        assert_eq!(mesh.publish_many(vec![sub_spec, sub1]).accepted.len(), 2);
        cp_size2 = mesh.checkpoint();
        assert_eq!(cp_size2.size, 2);
    }

    // Delete the second log entry (truncate log from size 2 down to size 1).
    {
        let db = Connection::open(dir.join("mesh.sqlite")).unwrap();
        db.execute("DELETE FROM log_entries WHERE entry_index = 1", [])
            .unwrap();
    }

    let mesh_trunc = Mesh::open(&dir).unwrap();
    assert_eq!(mesh_trunc.log_size(), 1);
    assert!(mesh_trunc.verify_log_consistency(&cp_size2).is_err());
    assert!(mesh_trunc.verify_log_integrity().is_err());

    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tamper_replayed_submission() {
    let dir = temp_dir("tamper_replay");
    let mut mesh = Mesh::open(&dir).unwrap();
    let spec_asm = asm_with(&fixture("spec_add1.vgs"), &BTreeMap::new());
    let sub1 = sign_submission(spec_asm[0].bytes.clone(), "alice", &sk(12));
    let sub2 = sub1.clone();

    let out1 = mesh.publish(sub1);
    assert_eq!(out1.accepted.len(), 1);
    let size_after_first = mesh.log_size();
    let cp_after_first = mesh.checkpoint();

    let out2 = mesh.publish(sub2);
    assert!(out2.accepted.is_empty());
    assert_eq!(out2.rejected.len(), 1);
    assert_eq!(out2.rejected[0].check, 6);
    assert_eq!(out2.rejected[0].code, "VG-M0006");
    assert_eq!(mesh.log_size(), size_after_first);
    assert_eq!(mesh.checkpoint(), cp_after_first);

    fs::remove_dir_all(&dir).unwrap();
}

// ---------- Gate G7 100k Node Benchmark ----------

/// Deterministic SplitMix64 PRNG (seeded, never random).
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    fn gen_range(&mut self, n: usize) -> usize {
        (self.next_u64() as usize) % n
    }
}

#[test]
#[ignore]
fn g7_bench_100k() {
    let dir = temp_dir("bench100k");
    let mut mesh = Mesh::open(&dir).unwrap();
    let mut rng = Rng(0x2026_0007_5647_4d45);

    let type_pool = [
        Type::UInt(8),
        Type::UInt(16),
        Type::UInt(32),
        Type::UInt(64),
        Type::SInt(32),
        Type::SInt(64),
        Type::Float(32),
        Type::Float(64),
        Type::Bool,
        Type::Slice(Box::new(Type::UInt(8))),
        Type::Slice(Box::new(Type::Float(32))),
        Type::Array(Box::new(Type::UInt(8)), 32),
        Type::Array(Box::new(Type::UInt(32)), 16),
        Type::Array(Box::new(Type::Float(32)), 4),
    ];

    let vocab = [
        "sha256",
        "crc32",
        "adler32",
        "blake3",
        "clamp",
        "soft_knee",
        "filter",
        "fir",
        "fft",
        "matrix",
        "multiply",
        "normalize",
        "softmax",
        "quantize",
        "utf8",
        "parse",
        "encode",
        "decode",
        "compress",
        "hash",
        "digest",
        "audio",
        "limiter",
        "gain",
        "stereo",
        "slice",
        "sort",
        "search",
        "checksum",
        "hmac",
        "aes",
        "chacha20",
    ];

    // Build 200 distinct valid type skeletons: outputs match input[0] (or input[1]) so outputs
    // are valid canonical VG-IR Operand::Input references.
    type Skeleton = (Vec<Type>, Vec<Type>, Vec<String>, Vec<String>);
    let mut skeletons: Vec<Skeleton> = Vec::with_capacity(200);
    for i in 0..200usize {
        let p1 = type_pool[i % type_pool.len()].clone();
        let p2 = type_pool[(i / type_pool.len() + i * 3) % type_pool.len()].clone();
        let params = if i % 3 == 0 {
            vec![p1.clone()]
        } else {
            vec![p1.clone(), p2.clone()]
        };
        let results = if i % 3 != 0 && i % 2 == 1 {
            vec![p2]
        } else {
            vec![p1]
        };
        let in_names: Vec<String> = params.iter().map(ToString::to_string).collect();
        let out_names: Vec<String> = results.iter().map(ToString::to_string).collect();
        skeletons.push((params, results, in_names, out_names));
    }

    // Create 100 real SPEC nodes first, each with a unique canonical BLAKE3 hash via Const(0).
    let mut specs: Vec<(Hash, String, usize)> = Vec::with_capacity(100);
    let mut entries: Vec<SyntheticEntry> = Vec::with_capacity(100_000);
    for s in 0..100usize {
        let skel_idx = s % skeletons.len();
        let (ref p_tys, ref r_tys, ref in_s, ref out_s) = skeletons[skel_idx];
        let out_op = if p_tys.len() > 1 && r_tys[0] == p_tys[1] && s % 2 == 1 {
            Operand::Input(1)
        } else {
            Operand::Input(0)
        };
        let spec_node = Node {
            spec: true,
            effects: EffectMask(0),
            params: p_tys.clone(),
            results: r_tys.clone(),
            learnable: vec![],
            consts: vec![Const {
                ty: Type::UInt(64),
                bytes: (s as u64).to_le_bytes().to_vec(),
            }],
            calls: vec![],
            ops: vec![],
            outputs: vec![out_op],
            contract: Contract {
                pre: vec![Term::Bin(
                    Bin::Eq,
                    Box::new(Term::Const(0)),
                    Box::new(Term::Const(0)),
                )],
                ..Contract::default()
            },
            debug: None,
        };
        let h = vg_core::canon::hashes(&spec_node).unwrap().node;
        let name = format!("spec_k{s}");
        let w1 = vocab[s % vocab.len()];
        let w2 = vocab[(s * 5 + 3) % vocab.len()];
        let intent = format!("{w1} {w2} reference specification {s}");
        specs.push((h, name.clone(), skel_idx));
        entries.push(SyntheticEntry {
            node_hash: h,
            is_spec: true,
            effect_mask: 0,
            input_types: in_s.clone(),
            output_types: out_s.clone(),
            cost_ops: 1,
            cost_expr: "ops <= 1".into(),
            intent,
            spec_memberships: vec![],
            spec_name: Some(name),
        });
    }

    // Generate 99,900 real NODEs across the 200 skeletons.
    let mut intent_samples: Vec<String> = Vec::with_capacity(64);
    for i in 0..99_900usize {
        let skel_idx = if i < 2000 {
            specs[i % 100].2
        } else {
            rng.gen_range(skeletons.len())
        };
        let (ref p_tys, ref r_tys, ref in_s, ref out_s) = skeletons[skel_idx];
        let eff_bits = match rng.gen_range(8) {
            0..=4 => 0u32,
            5 => EffectMask::ARENA,
            6 => EffectMask::ARENA | 0x02, // CLOCK
            _ => EffectMask::ARENA | 0x04, // FILE_READ
        };
        let cost_ops = 1 + (rng.next_u64() % 256);
        let out_op = if p_tys.len() > 1 && r_tys[0] == p_tys[1] && skel_idx % 2 == 1 {
            Operand::Input(1)
        } else {
            Operand::Input(0)
        };
        let node = Node {
            spec: false,
            effects: EffectMask(eff_bits),
            params: p_tys.clone(),
            results: r_tys.clone(),
            learnable: vec![],
            consts: vec![Const {
                ty: Type::UInt(64),
                bytes: ((i + 1000) as u64).to_le_bytes().to_vec(),
            }],
            calls: vec![],
            ops: vec![],
            outputs: vec![out_op],
            contract: Contract {
                pre: vec![Term::Bin(
                    Bin::Eq,
                    Box::new(Term::Const(0)),
                    Box::new(Term::Const(0)),
                )],
                ..Contract::default()
            },
            debug: None,
        };
        let h = vg_core::canon::hashes(&node).unwrap().node;

        let w1 = vocab[rng.gen_range(vocab.len())];
        let w2 = vocab[rng.gen_range(vocab.len())];
        let w3 = vocab[rng.gen_range(vocab.len())];
        let intent = format!("{w1} {w2} {w3} kernel variant {}", i % 500);
        if intent_samples.len() < 64 && i % 1500 == 0 {
            intent_samples.push(format!("{w1} {w2} {w3}"));
        }

        let spec_memberships = if i < 5_000 {
            let sp = &specs[i % 100];
            let ev = match i % 3 {
                0 => Evidence::Proved,
                1 => Evidence::Bounded(64),
                _ => Evidence::Tested(1000),
            };
            vec![(sp.0, ev)]
        } else {
            vec![]
        };

        entries.push(SyntheticEntry {
            node_hash: h,
            is_spec: false,
            effect_mask: eff_bits,
            input_types: in_s.clone(),
            output_types: out_s.clone(),
            cost_ops,
            cost_expr: format!("ops <= {cost_ops}"),
            intent,
            spec_memberships,
            spec_name: None,
        });
    }

    assert_eq!(entries.len(), 100_000);
    mesh.insert_synthetic_batch(&entries).unwrap();
    assert_eq!(mesh.node_count(), 100_000);

    // Prepare 10,000 seeded mixed queries: spec, structural, and structural+intent.
    let mut queries = Vec::with_capacity(10_000);
    for q_idx in 0..10_000usize {
        let q = match q_idx % 3 {
            0 => {
                // Spec class query.
                let sp = &specs[rng.gen_range(specs.len())];
                MeshContractQuery {
                    spec: Some(sp.1.clone()),
                    max_effects: vec!["ARENA".into()],
                    limit: Some(10),
                    ..Default::default()
                }
            }
            1 => {
                // Structural + effect mask query.
                let sk_i = rng.gen_range(skeletons.len());
                MeshContractQuery {
                    input_types: skeletons[sk_i].2.clone(),
                    output_types: skeletons[sk_i].3.clone(),
                    max_effects: vec!["ARENA".into()],
                    limit: Some(10),
                    ..Default::default()
                }
            }
            _ => {
                // Structural + semantic intent query.
                let sk_i = rng.gen_range(skeletons.len());
                let intent = intent_samples[rng.gen_range(intent_samples.len())].clone();
                MeshContractQuery {
                    input_types: skeletons[sk_i].2.clone(),
                    output_types: skeletons[sk_i].3.clone(),
                    max_effects: vec!["ARENA".into()],
                    intent: Some(intent),
                    limit: Some(10),
                    ..Default::default()
                }
            }
        };
        queries.push(q);
    }

    // Warm up caches and branch predictors.
    for q in &queries[..500] {
        let _ = mesh.query_indexed(q, 10).unwrap();
    }

    let mut latencies_us = Vec::with_capacity(queries.len());
    for q in &queries {
        let t0 = Instant::now();
        let res = mesh.query_indexed(q, 10).unwrap();
        let dt = t0.elapsed().as_micros() as u64;
        std::hint::black_box(&res);
        latencies_us.push(dt);
    }
    latencies_us.sort_unstable();
    let p50_us = latencies_us[latencies_us.len() / 2];
    let p99_idx = (latencies_us.len() * 99) / 100;
    let p99_us = latencies_us[p99_idx];

    println!("G7 p99_us={p99_us} p50_us={p50_us} nodes=100000 queries=10000");
    assert!(
        p99_us < 5000,
        "G7 p99 latency {p99_us} us exceeded 5000 us budget"
    );

    fs::remove_dir_all(&dir).unwrap();
}
