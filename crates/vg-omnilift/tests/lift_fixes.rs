//! Regression cases for lifter bugs found by R-13 (2026-11-25), in C so they hold for every
//! language: an out-of-bounds read of a constant table is Err (it read a clamped element),
//! and a negative signed value survives reinterpretation as unsigned (CAST.in_range trapped).
//! Skips without alive-tv and opt, unless VG_REQUIRE_ALIVE2=1.

use std::collections::BTreeMap;

use vg_core::types::Type;
use vg_core::value::Value;
use vg_interp::{NoEffectsHost, run};
use vg_omnilift::ingest::Language;
use vg_omnilift::lift::{Lifted, lift_source};

const SRC: &str = r#"
#include <stddef.h>
#include <stdint.h>

static const int64_t T[4] = {10, 20, 30, 40};

int64_t table_at(int64_t i) { return T[i]; }

int64_t sar3(int64_t x) { return x >> 3; }

"#;

fn ready() -> bool {
    let ok = std::env::var("VG_REQUIRE_ALIVE2").as_deref() == Ok("1")
        || (vg_omnilift::verify::has_alive_tv() && vg_omnilift::ingest::has_opt());
    if !ok {
        eprintln!("SKIP: alive-tv or opt not installed on this host");
    }
    ok
}

fn lift(function: &str) -> Lifted {
    let dir = std::env::temp_dir().join(format!("vg_lift_fixes_{}_{function}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("fixes.c");
    std::fs::write(&path, SRC).unwrap();
    let l = lift_source(&path, function, Language::C, &[]).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    l
}

fn eval(l: &Lifted, args: &[Value]) -> Value {
    let map: BTreeMap<_, _> = l
        .nodes
        .iter()
        .map(|a| (a.hashes.node, a.node.clone()))
        .collect();
    let out = run(
        &l.nodes.last().unwrap().node,
        args,
        &|h| map.get(h).cloned(),
        &mut NoEffectsHost,
    );
    out.unwrap().into_iter().next().unwrap()
}

/// An i64 argument as the lift's parameter type takes it (LLVM ints are signless).
fn arg(l: &Lifted, i: usize, v: i64) -> Value {
    match l.nodes.last().unwrap().node.params[i] {
        Type::UInt(_) => Value::Int(i128::from(v as u64)),
        _ => Value::Int(i128::from(v)),
    }
}

fn ok(v: i64) -> Value {
    Value::Ok(Box::new(Value::Int(i128::from(v as u64))))
}

#[test]
fn constant_table_out_of_bounds_is_err_not_a_clamped_element() {
    if !ready() {
        return;
    }
    let l = lift("table_at");
    assert!(l.returns_result);
    assert!(l.ub_found.iter().any(|u| u.kind == "out_of_bounds"));
    assert_eq!(eval(&l, &[arg(&l, 0, 2)]), ok(30));
    for bad in [4, 5, -1, 1 << 61] {
        assert!(
            matches!(eval(&l, &[arg(&l, 0, bad)]), Value::Err(_)),
            "T[{bad}]"
        );
    }
}

#[test]
fn negative_signed_values_do_not_trap() {
    if !ready() {
        return;
    }
    let l = lift("sar3");
    for x in [-1i64, -9, i64::MIN, 17] {
        let got = eval(&l, &[arg(&l, 0, x)]);
        let want = Value::Int(i128::from((x >> 3) as u64));
        assert!(
            got == want || got == Value::Ok(Box::new(want.clone())),
            "sar3({x}) = {got:?}"
        );
    }
}
