//! OmniLift ingest for C++, Swift, Zig and GLSL (docs/08): each kernel lifts, passes the
//! Alive2 round trip and differential fuzzing inside `lift_source`, and runs in vg-interp.
//! Each test skips when its compiler is missing, unless VG_REQUIRE_ALIVE2=1.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vg_core::value::Value;
use vg_interp::{NoEffectsHost, run};
use vg_omnilift::ingest::{Language, has_compiler};
use vg_omnilift::lift::{Lifted, lift_source};

fn kernels(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/lift/languages")
        .join(file)
}

fn ready(lang: Language) -> bool {
    if std::env::var("VG_REQUIRE_ALIVE2").as_deref() == Ok("1") {
        return true;
    }
    let ok =
        vg_omnilift::verify::has_alive_tv() && vg_omnilift::ingest::has_opt() && has_compiler(lang);
    if !ok {
        eprintln!(
            "SKIP: alive-tv, opt or the {} compiler is missing",
            lang.as_str()
        );
    }
    ok
}

fn eval(l: &Lifted, args: &[Value]) -> Vec<Value> {
    let map: BTreeMap<_, _> = l
        .nodes
        .iter()
        .map(|a| (a.hashes.node, a.node.clone()))
        .collect();
    run(
        &l.nodes.last().unwrap().node,
        args,
        &|h| map.get(h).cloned(),
        &mut NoEffectsHost,
    )
    .unwrap()
}

fn int(v: u64) -> Value {
    Value::Int(i128::from(v))
}

fn bitmix(a: u32, b: u32) -> u64 {
    u64::from(((a << 5) ^ (b >> 3)).wrapping_add(a & 0xff))
}

fn check_bitmix_and_sum_bytes(path: &Path, lang: Language) {
    let l = lift_source(path, "bitmix", lang, &[]).unwrap();
    assert_eq!(l.provenance.language, lang);
    assert!(l.fuzz_cases > 0 && l.bounded_check == "alive2");
    for (a, b) in [(0x1234, 0x5678), (u32::MAX, 7), (0, u32::MAX)] {
        assert_eq!(
            eval(&l, &[int(a.into()), int(b.into())]),
            vec![int(bitmix(a, b))]
        );
    }
    let l = lift_source(path, "sum_bytes", lang, &[]).unwrap();
    let data: Vec<Value> = [10u64, 20, 30, 255].into_iter().map(int).collect();
    assert_eq!(eval(&l, &[Value::Seq(data)]), vec![int(315)]);
}

#[test]
fn cpp_kernels_lift() {
    if !ready(Language::Cpp) {
        return;
    }
    let path = kernels("kernels.cpp");
    check_bitmix_and_sum_bytes(&path, Language::Cpp);
    let l = lift_source(&path, "rotmix", Language::Cpp, &[]).unwrap();
    let (a, b) = (0xdead_beef_u32, 0x0123_4567_u32);
    let want = a.rotate_left(7) ^ b.rotate_left(13);
    assert_eq!(
        eval(&l, &[int(a.into()), int(b.into())]),
        vec![int(want.into())]
    );
}

#[test]
fn swift_kernels_lift() {
    if ready(Language::Swift) {
        check_bitmix_and_sum_bytes(&kernels("kernels.swift"), Language::Swift);
    }
}

#[test]
fn zig_kernels_lift() {
    if ready(Language::Zig) {
        check_bitmix_and_sum_bytes(&kernels("kernels.zig"), Language::Zig);
    }
}

fn pcg_hash(v: u32) -> u32 {
    let state = v.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
    let word = ((state >> ((state >> 28) + 4)) ^ state).wrapping_mul(277_803_737);
    (word >> 22) ^ word
}

#[test]
fn glsl_kernels_lift_with_wrapping_int() {
    if !ready(Language::Glsl) {
        return;
    }
    let path = kernels("kernels.glsl");
    let l = lift_source(&path, "bitmix", Language::Glsl, &[]).unwrap();
    assert_eq!(
        eval(&l, &[int(0x1234), int(0x5678)]),
        vec![int(bitmix(0x1234, 0x5678))]
    );

    let l = lift_source(&path, "pcg_hash", Language::Glsl, &[]).unwrap();
    for v in [0u32, 1, 0xffff_ffff, 12345] {
        assert_eq!(eval(&l, &[int(v.into())]), vec![int(pcg_hash(v).into())]);
    }

    // GLSL int wraps: the hash leaves i32 range after a few steps and must wrap, not fail.
    // The lifter may keep an Err guard on the loop counter that it cannot prove dead; the
    // hash itself must still wrap and return Ok.
    let l = lift_source(&path, "poly_hash", Language::Glsl, &[]).unwrap();
    for n in [0i32, 1, 20, 3000] {
        // LLVM ints are signless and the lift returns them as u32: compare the bits.
        let bits = (0..n).fold(7i32, |s, i| s.wrapping_mul(31).wrapping_add(i)) as u32;
        let want = int(bits.into());
        let want = if l.returns_result {
            Value::Ok(Box::new(want))
        } else {
            want
        };
        assert_eq!(eval(&l, &[Value::Int(n.into())]), vec![want]);
    }
}

fn glsl_rejects(src: &str, function: &str, needle: &str) {
    let dir = std::env::temp_dir().join(format!("vg_glsl_neg_{}_{function}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("neg.glsl");
    std::fs::write(&path, src).unwrap();
    let e = lift_source(&path, function, Language::Glsl, &[]).unwrap_err();
    assert_eq!(e.code, "VG-L0001", "{e}");
    assert!(e.message.contains(needle), "{}", e.message);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn glsl_rejects_constructs_whose_cpp_meaning_differs() {
    if !ready(Language::Glsl) {
        return;
    }
    // GLSL reads 0xFFFFFFFF as int -1; C++ reads it as unsigned 4294967295.
    glsl_rejects(
        "int f(int a) { return a / 0xFFFFFFFF; }\n",
        "f",
        "0xFFFFFFFF",
    );
    // GLSL passes arrays by value; C++ would pass a pointer the callee can write through.
    glsl_rejects(
        "uint g(uint a[4]) { a[0] = 1u; return a[0]; }\n",
        "g",
        "pointer parameter",
    );
    // Not GLSL at all: glslc rejects it before C++ sees it.
    glsl_rejects("uint h(uint* p) { return *p; }\n", "h", "glslc");
}
