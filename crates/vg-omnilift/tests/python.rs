//! R-13 Python ingest (docs/08, docs/research/R-13-report.md): lifts return CPython's value or
//! Err where CPython raises or leaves int64, and code outside the subset is rejected.
//! The lift tests skip when a tool is missing, unless VG_REQUIRE_ALIVE2=1.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use vg_core::value::Value;
use vg_interp::{NoEffectsHost, run};
use vg_omnilift::ingest::{Language, has_compiler};
use vg_omnilift::lift::{Lifted, lift_source};

fn corpus(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/lift/python")
        .join(file)
}

fn ready() -> bool {
    if std::env::var("VG_REQUIRE_ALIVE2").as_deref() == Ok("1") {
        return true;
    }
    let ok = vg_omnilift::verify::has_alive_tv()
        && vg_omnilift::ingest::has_opt()
        && has_compiler(Language::Python);
    if !ok {
        eprintln!("SKIP: alive-tv, opt, clang or python3 >= 3.11 is missing");
    }
    ok
}

/// Ok(Python int) or Err, from the lift run in vg-interp.
fn eval(l: &Lifted, args: &[i64]) -> Result<i128, ()> {
    let map: BTreeMap<_, _> = l
        .nodes
        .iter()
        .map(|a| (a.hashes.node, a.node.clone()))
        .collect();
    let root = &l.nodes.last().unwrap().node;
    // LLVM ints are signless: a parameter the lift types u64 takes the bit pattern.
    let vals: Vec<Value> = args
        .iter()
        .zip(&root.params)
        .map(|(&a, ty)| match ty {
            vg_core::types::Type::UInt(_) => Value::Int(i128::from(a as u64)),
            _ => Value::Int(i128::from(a)),
        })
        .collect();
    let out = run(root, &vals, &|h| map.get(h).cloned(), &mut NoEffectsHost).unwrap();
    let signed = |x: i128| {
        if x > i128::from(i64::MAX) {
            x - (1i128 << 64)
        } else {
            x
        }
    };
    match out.as_slice() {
        [Value::Ok(v)] => match **v {
            Value::Int(x) => Ok(signed(x)),
            _ => panic!("{v:?}"),
        },
        [Value::Err(_)] => Err(()),
        [Value::Int(x)] => Ok(signed(*x)),
        other => panic!("{other:?}"),
    }
}

fn lift(function: &str) -> Lifted {
    let l = lift_source(&corpus("kernels.py"), function, Language::Python, &[]).unwrap();
    assert_eq!(l.bounded_check, "alive2");
    assert_eq!(l.fuzz_cases, 1_000_000);
    l
}

#[test]
fn zipfile_crc_round_matches_cpython() {
    if !ready() {
        return;
    }
    let l = lift("crc_byte");
    let crc = |mut c: i64| {
        for _ in 0..8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
        }
        c
    };
    for c in [0, 1, 0xff, 0x1234_5678, -1, i64::MIN] {
        assert_eq!(eval(&l, &[c]), Ok(i128::from(crc(c))), "crc_byte({c})");
    }
}

#[test]
fn index_error_and_negative_indices_follow_cpython() {
    if !ready() {
        return;
    }
    let l = lift("square_at");
    assert_eq!(eval(&l, &[3]), Ok(9));
    assert_eq!(eval(&l, &[-1]), Ok(49)); // SQUARES[-1]
    assert_eq!(eval(&l, &[-8]), Ok(0));
    assert_eq!(eval(&l, &[8]), Err(())); // IndexError
    assert_eq!(eval(&l, &[-9]), Err(()));
    assert_eq!(eval(&l, &[1 << 61]), Err(())); // not a wrapped byte offset into the table
}

#[test]
fn raise_and_int64_overflow_are_err() {
    if !ready() {
        return;
    }
    let l = lift("clamp_or_raise");
    assert_eq!(eval(&l, &[5, 0, 3]), Ok(3));
    assert_eq!(eval(&l, &[-5, 0, 3]), Ok(0));
    assert_eq!(eval(&l, &[1, 3, 0]), Err(())); // raise ValueError
    let l = lift("fib");
    assert_eq!(eval(&l, &[10]), Ok(55));
    assert_eq!(eval(&l, &[-3]), Ok(0)); // range(-3) is empty
    // fib(93) leaves int64; CPython returns a big int, the lift Err.
    assert_eq!(eval(&l, &[93]), Err(()));
}

fn rejected(file: &str, function: &str, needle: &str) {
    let e = lift_source(&corpus(file), function, Language::Python, &[]).unwrap_err();
    assert_eq!(e.code, "VG-L0001", "{e}");
    assert!(e.message.contains(needle), "{function}: {}", e.message);
}

#[test]
fn functions_outside_the_subset_are_rejected_not_guessed() {
    if !has_compiler(Language::Python) {
        eprintln!("SKIP: python3 >= 3.11 is missing");
        return;
    }
    for (function, needle) in [
        ("true_div", "unsupported operator Div"),
        ("recurse", "recursion"),
        ("uses_mutated_table", "may be mutated"),
        ("maybe_unbound", "may be read before it is assigned"),
        ("slices", "can be indexed"),
        ("bool_from_int", "return type is bool"),
        ("bare_return", "bare return"),
        ("float_const", "unsupported constant 1.5"),
        ("mutates", "only names and tuples of names can be assigned"),
        ("side_effect", "expression statements"),
    ] {
        rejected("rejected.py", function, needle);
    }
    rejected("star_import.py", "low", "star import");
}
