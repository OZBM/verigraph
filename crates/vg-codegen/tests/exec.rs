//! End-to-end tests of vg-codegen: build real aarch64-apple-darwin executables, run them through the
//! stdin/stdout protocol and compare with values derived independently in Rust (docs/13 semantics:
//! exact integer arithmetic in i128, IEEE operations of the host).

// Those executables run only on an Apple Silicon host; CI runs this file on macos-arm64.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use vg_codegen::{BuildReport, Options, Target, build};
use vg_core::diag::Diagnostic;
use vg_core::ir::{Hash, Node};
use vg_core::types::{EffectMask, Type};
use vg_core::{Assembled, asm_local};

static SEQ: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------------------------

struct Module {
    all: Vec<Assembled>,
    by_hash: HashMap<Hash, Node>,
}

impl Module {
    fn new(src: &str) -> Module {
        let all = asm_local(src).unwrap_or_else(|d| panic!("assembly failed: {d:?}"));
        let by_hash = all
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        Module { all, by_hash }
    }

    fn node(&self, label: &str) -> &Node {
        &self
            .all
            .iter()
            .find(|a| a.label.trim_start_matches('@') == label)
            .unwrap_or_else(|| panic!("no node {label}"))
            .node
    }

    fn build_to(&self, label: &str, opts: &Options, out: &Path) -> Result<BuildReport, Diagnostic> {
        build(
            self.node(label),
            &|h| self.by_hash.get(h).cloned(),
            opts,
            out,
        )
    }
}

fn corpus(rel: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

fn scratch_dir() -> PathBuf {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("vg-exec-test-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Exe {
    dir: PathBuf,
    path: PathBuf,
    residual_guards: u32,
}

impl Drop for Exe {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn aarch64(proof_facts: bool, runtime_checks: bool) -> Options {
    Options {
        target: Target::Aarch64Darwin,
        proof_facts,
        runtime_checks,
    }
}

fn exe(m: &Module, label: &str, proof_facts: bool, runtime_checks: bool) -> Exe {
    let dir = scratch_dir();
    let path = dir.join("prog");
    let rep = m
        .build_to(label, &aarch64(proof_facts, runtime_checks), &path)
        .unwrap_or_else(|d| panic!("build {label} failed: {d:?}"));
    assert!(rep.linked, "aarch64 build must be linked");
    Exe {
        dir,
        path,
        residual_guards: rep.residual_guards,
    }
}

struct Out {
    code: i32,
    stdout: Vec<u8>,
    stderr: String,
}

fn run(e: &Exe, input: &[u8]) -> Out {
    let mut child = Command::new(&e.path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&data);
    });
    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    Out {
        code: out
            .status
            .code()
            .unwrap_or_else(|| panic!("killed by a signal: {:?}", out.status)),
        stdout: out.stdout,
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn le(v: i128, bytes: usize) -> Vec<u8> {
    v.to_le_bytes()[..bytes].to_vec()
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn f32s(xs: &[f32]) -> Vec<u8> {
    xs.iter().flat_map(|x| x.to_bits().to_le_bytes()).collect()
}

/// An f32 as the runtime writes it: NaN canonical.
fn canon32(x: f32) -> f32 {
    if x.is_nan() { f32::NAN } else { x }
}

fn fmin32(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else if a == b {
        if a.is_sign_negative() { a } else { b }
    } else if a < b {
        a
    } else {
        b
    }
}

fn fmax32(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        f32::NAN
    } else if a == b {
        if a.is_sign_positive() { a } else { b }
    } else if a > b {
        a
    } else {
        b
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

// ---------------------------------------------------------------------------------------------
// Integer modes
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct IntTy {
    w: u32,
    signed: bool,
}

impl IntTy {
    fn name(self) -> String {
        format!("{}{}", if self.signed { "i" } else { "u" }, self.w)
    }
    fn bytes(self) -> usize {
        (self.w / 8) as usize
    }
    fn bounds(self) -> (i128, i128) {
        if self.signed {
            (-(1i128 << (self.w - 1)), (1i128 << (self.w - 1)) - 1)
        } else {
            (0, (1i128 << self.w) - 1)
        }
    }
    fn in_range(self, v: i128) -> bool {
        let (lo, hi) = self.bounds();
        (lo..=hi).contains(&v)
    }
    /// The unique value of the type congruent to v modulo 2^w.
    fn wrap(self, v: i128) -> i128 {
        let m = 1i128 << self.w;
        let r = v.rem_euclid(m);
        if self.signed && r >= m / 2 { r - m } else { r }
    }
    fn clamp(self, v: i128) -> i128 {
        let (lo, hi) = self.bounds();
        v.clamp(lo, hi)
    }
    fn edges(self) -> Vec<i128> {
        let (lo, hi) = self.bounds();
        let mut v = vec![lo, lo + 1, -1, 0, 1, 2, hi / 2, hi - 1, hi];
        v.retain(|x| self.in_range(*x));
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// The exact result of a binary op; None when it exceeds even i128 (only a u64 product can).
fn exact(op: &str, a: i128, b: i128) -> Option<i128> {
    match op {
        "ADD" => Some(a + b),
        "SUB" => Some(a - b),
        "MUL" => a.checked_mul(b),
        "DIV" => Some(a / b),
        "MOD" => Some(a % b),
        "NEG" => Some(-a),
        _ => unreachable!(),
    }
}

fn wrapped(t: IntTy, op: &str, a: i128, b: i128) -> i128 {
    let v = if op == "MUL" {
        a.wrapping_mul(b)
    } else {
        exact(op, a, b).unwrap()
    };
    t.wrap(v)
}

fn saturated(t: IntTy, op: &str, a: i128, b: i128) -> i128 {
    match exact(op, a, b) {
        Some(v) => t.clamp(v),
        None => t.bounds().1,
    }
}

fn int_wrap_and_sat(t: IntTy) {
    let n = t.name();
    let src = format!(
        "module m;
NODE @k {{
  SIGNATURE: (a: {n}, b: {n}) -> ({n}, {n}, {n}, {n}, {n}, {n}, {n}, {n});
  EFFECTS: [];
  CONTRACT {{ }}
  BODY {{
    %0 = ADD.wrap(a, b);
    %1 = ADD.sat(a, b);
    %2 = SUB.wrap(a, b);
    %3 = SUB.sat(a, b);
    %4 = MUL.wrap(a, b);
    %5 = MUL.sat(a, b);
    %6 = NEG.wrap(a);
    %7 = NEG.sat(a);
    RETURN(%0, %1, %2, %3, %4, %5, %6, %7);
  }}
}}
"
    );
    let m = Module::new(&src);
    let e = exe(&m, "k", true, true);
    for &a in &t.edges() {
        for &b in &t.edges() {
            let want: Vec<i128> = vec![
                wrapped(t, "ADD", a, b),
                saturated(t, "ADD", a, b),
                wrapped(t, "SUB", a, b),
                saturated(t, "SUB", a, b),
                wrapped(t, "MUL", a, b),
                saturated(t, "MUL", a, b),
                wrapped(t, "NEG", a, b),
                saturated(t, "NEG", a, b),
            ];
            let want: Vec<u8> = want.iter().flat_map(|v| le(*v, t.bytes())).collect();
            let o = run(&e, &cat(&[le(a, t.bytes()), le(b, t.bytes())]));
            assert_eq!(o.code, 0, "{n} a={a} b={b}: {}", o.stderr);
            assert_eq!(o.stdout, want, "{n} a={a} b={b}");
        }
    }
}

fn int_division(t: IntTy) {
    let n = t.name();
    let src = format!(
        "module m;
NODE @k {{
  SIGNATURE: (a: {n}, b: {n}) -> ({n}, {n}, {n}, {n});
  EFFECTS: [];
  CONTRACT {{
    PRE: b != 0;
  }}
  BODY {{
    %0 = DIV.wrap(a, b);
    %1 = DIV.sat(a, b);
    %2 = MOD.wrap(a, b);
    %3 = MOD.sat(a, b);
    RETURN(%0, %1, %2, %3);
  }}
}}
"
    );
    let m = Module::new(&src);
    let e = exe(&m, "k", true, true);
    for &a in &t.edges() {
        for &b in &t.edges() {
            let input = cat(&[le(a, t.bytes()), le(b, t.bytes())]);
            let o = run(&e, &input);
            if b == 0 {
                assert_eq!(o.code, 1, "{n} a={a} b=0 must fail PRE");
                assert!(o.stdout.is_empty());
                continue;
            }
            let want: Vec<i128> = vec![
                wrapped(t, "DIV", a, b),
                saturated(t, "DIV", a, b),
                wrapped(t, "MOD", a, b),
                saturated(t, "MOD", a, b),
            ];
            let want: Vec<u8> = want.iter().flat_map(|v| le(*v, t.bytes())).collect();
            assert_eq!(o.code, 0, "{n} a={a} b={b}: {}", o.stderr);
            assert_eq!(o.stdout, want, "{n} a={a} b={b}");
        }
    }
}

fn int_checked(t: IntTy) {
    let n = t.name();
    for op in ["ADD", "SUB", "MUL", "NEG", "DIV", "MOD"] {
        let call = if op == "NEG" {
            "NEG.checked(a)".to_string()
        } else {
            format!("{op}.checked(a, b)")
        };
        let pre = if op == "DIV" || op == "MOD" {
            "PRE: b != 0;"
        } else {
            ""
        };
        let src = format!(
            "module m;
NODE @k {{
  SIGNATURE: (a: {n}, b: {n}) -> ({n});
  EFFECTS: [];
  CONTRACT {{ {pre} }}
  BODY {{
    %0 = {call};
    RETURN(%0);
  }}
}}
"
        );
        let m = Module::new(&src);
        for facts in [false, true] {
            let e = exe(&m, "k", facts, true);
            for &a in &t.edges() {
                for &b in &t.edges() {
                    let o = run(&e, &cat(&[le(a, t.bytes()), le(b, t.bytes())]));
                    let ctx = format!("{n} {op}.checked a={a} b={b} facts={facts}");
                    if (op == "DIV" || op == "MOD") && b == 0 {
                        assert_eq!(o.code, 1, "{ctx}");
                        continue;
                    }
                    match exact(op, a, b).filter(|v| t.in_range(*v)) {
                        Some(v) => {
                            assert_eq!(o.code, 0, "{ctx}: {}", o.stderr);
                            assert_eq!(o.stdout, le(v, t.bytes()), "{ctx}");
                        }
                        None => {
                            assert_eq!(o.code, 4, "{ctx} must trap");
                            assert!(o.stdout.is_empty(), "{ctx}");
                            assert!(o.stderr.contains("\"event\""), "{ctx}: {}", o.stderr);
                        }
                    }
                }
            }
        }
    }
}

macro_rules! int_suite {
    ($m:ident, $w:expr, $signed:expr) => {
        mod $m {
            use super::*;
            const T: IntTy = IntTy {
                w: $w,
                signed: $signed,
            };
            #[test]
            fn wrap_and_sat_modes() {
                int_wrap_and_sat(T);
            }
            #[test]
            fn division_and_remainder_modes() {
                int_division(T);
            }
            #[test]
            fn checked_modes_trap_exactly_when_the_result_leaves_the_type() {
                int_checked(T);
            }
        }
    };
}

int_suite!(int_i8, 8, true);
int_suite!(int_u8, 8, false);
int_suite!(int_i32, 32, true);
int_suite!(int_u64, 64, false);
int_suite!(int_i64, 64, true);

// ---------------------------------------------------------------------------------------------
// Shifts and rotates
// ---------------------------------------------------------------------------------------------

fn shifts(t: IntTy) {
    let n = t.name();
    let w = t.w as i128;
    let src = format!(
        "module m;
NODE @w {{
  SIGNATURE: (a: {n}, s: u32) -> ({n}, {n}, {n}, {n});
  EFFECTS: [];
  CONTRACT {{ }}
  BODY {{
    %0 = SHL.wrap(a, s);
    %1 = SHR.wrap(a, s);
    %2 = ROTL(a, s);
    %3 = ROTR(a, s);
    RETURN(%0, %1, %2, %3);
  }}
}}
NODE @c {{
  SIGNATURE: (a: {n}, s: u32) -> ({n}, {n});
  EFFECTS: [];
  CONTRACT {{ }}
  BODY {{
    %0 = SHL.checked(a, s);
    %1 = SHR.checked(a, s);
    RETURN(%0, %1);
  }}
}}
"
    );
    let m = Module::new(&src);
    let ew = exe(&m, "w", true, true);
    let ec = exe(&m, "c", true, true);
    let amounts: Vec<i128> = vec![
        0,
        1,
        w - 1,
        w,
        w + 1,
        2 * w - 1,
        63,
        64,
        65,
        255,
        4_294_967_295,
    ];
    for &a in &t.edges() {
        for &s in &amounts {
            let input = cat(&[le(a, t.bytes()), le(s, 4)]);
            let sh = s % w;
            let u = a.rem_euclid(1i128 << w);
            let mask = (1i128 << w) - 1;
            let want: Vec<i128> = vec![
                a << sh,
                a >> sh,
                ((u << sh) | (u >> (w - sh))) & mask,
                ((u >> sh) | (u << (w - sh))) & mask,
            ];
            let want: Vec<u8> = want.iter().flat_map(|v| le(*v, t.bytes())).collect();
            let o = run(&ew, &input);
            assert_eq!(o.code, 0, "{n} wrap a={a} s={s}: {}", o.stderr);
            assert_eq!(o.stdout, want, "{n} wrap a={a} s={s}");

            let o = run(&ec, &input);
            if s < w {
                assert_eq!(o.code, 0, "{n} checked a={a} s={s}: {}", o.stderr);
                let want: Vec<u8> = [a << s, a >> s]
                    .iter()
                    .flat_map(|v| le(*v, t.bytes()))
                    .collect();
                assert_eq!(o.stdout, want, "{n} checked a={a} s={s}");
            } else {
                assert_eq!(o.code, 4, "{n} checked a={a} s={s} must trap");
            }
        }
    }
}

macro_rules! shift_suite {
    ($name:ident, $w:expr, $signed:expr) => {
        #[test]
        fn $name() {
            shifts(IntTy {
                w: $w,
                signed: $signed,
            });
        }
    };
}

shift_suite!(shifts_and_rotates_u8, 8, false);
shift_suite!(shifts_and_rotates_i8, 8, true);
shift_suite!(shifts_and_rotates_u32, 32, false);
shift_suite!(shifts_and_rotates_i64, 64, true);

// ---------------------------------------------------------------------------------------------
// Floats
// ---------------------------------------------------------------------------------------------

macro_rules! float_suite {
    ($m:ident, $F:ident, $B:ident, $tn:literal, $maxsub:expr) => {
        mod $m {
            use super::*;
            type F = $F;
            const TN: &str = $tn;

            fn bits(x: F) -> $B {
                if x.is_nan() {
                    F::NAN.to_bits()
                } else {
                    x.to_bits()
                }
            }
            fn enc(x: F) -> Vec<u8> {
                x.to_bits().to_le_bytes().to_vec()
            }
            fn push(want: &mut Vec<u8>, x: F) {
                want.extend(bits(x).to_le_bytes());
            }
            fn fmin(a: F, b: F) -> F {
                if a.is_nan() || b.is_nan() {
                    F::NAN
                } else if a == b {
                    if a.is_sign_negative() { a } else { b }
                } else if a < b {
                    a
                } else {
                    b
                }
            }
            fn fmax(a: F, b: F) -> F {
                if a.is_nan() || b.is_nan() {
                    F::NAN
                } else if a == b {
                    if a.is_sign_positive() { a } else { b }
                } else if a > b {
                    a
                } else {
                    b
                }
            }
            fn edges() -> Vec<F> {
                vec![
                    F::NAN,
                    F::INFINITY,
                    F::NEG_INFINITY,
                    -0.0,
                    0.0,
                    F::from_bits(1),
                    F::from_bits($maxsub),
                    F::MIN_POSITIVE,
                    1.0,
                    -1.5,
                    3.0,
                    F::MAX,
                    -F::MAX,
                ]
            }

            #[test]
            fn arithmetic_comparisons_and_unary_ops_on_edge_values() {
                let src = format!(
                    "module m;
NODE @ops {{
  SIGNATURE: (a: {TN}, b: {TN}, c: {TN}) -> ({TN}, {TN}, {TN}, {TN}, {TN}, {TN}, {TN}, {TN}, {TN}, {TN}, Bool, Bool, Bool, Bool, Bool, Bool);
  EFFECTS: [];
  CONTRACT {{ }}
  BODY {{
    %0 = FADD(a, b);
    %1 = FSUB(a, b);
    %2 = FMUL(a, b);
    %3 = FDIV(a, b);
    %4 = FMA(a, b, c);
    %5 = FNEG(a);
    %6 = FABS(a);
    %7 = FMIN(a, b);
    %8 = FMAX(a, b);
    %9 = FSQRT(a);
    %10 = EQ(a, b);
    %11 = NE(a, b);
    %12 = LT.f(a, b);
    %13 = LE.f(a, b);
    %14 = GT.f(a, b);
    %15 = GE.f(a, b);
    RETURN(%0, %1, %2, %3, %4, %5, %6, %7, %8, %9, %10, %11, %12, %13, %14, %15);
  }}
}}
"
                );
                let m = Module::new(&src);
                for facts in [false, true] {
                    let e = exe(&m, "ops", facts, true);
                    for &a in &edges() {
                        for &b in &edges() {
                            for c in [-0.0, 1.0, F::NAN] {
                                let input = cat(&[enc(a), enc(b), enc(c)]);
                                let mut want = Vec::new();
                                for v in [
                                    a + b,
                                    a - b,
                                    a * b,
                                    a / b,
                                    a.mul_add(b, c),
                                    -a,
                                    a.abs(),
                                    fmin(a, b),
                                    fmax(a, b),
                                    a.sqrt(),
                                ] {
                                    push(&mut want, v);
                                }
                                for t in [a == b, a != b, a < b, a <= b, a > b, a >= b] {
                                    want.push(u8::from(t));
                                }
                                let o = run(&e, &input);
                                let ctx = format!("a={a:e} b={b:e} c={c:e} facts={facts}");
                                assert_eq!(o.code, 0, "{ctx}: {}", o.stderr);
                                assert_eq!(o.stdout, want, "{ctx}");
                            }
                        }
                    }
                }
            }

            #[test]
            fn fclamp_and_fcopysign_follow_ieee_and_trap_outside_their_domain() {
                let src = format!(
                    "module m;
NODE @cl {{
  SIGNATURE: (x: {TN}, lo: {TN}, hi: {TN}) -> ({TN});
  EFFECTS: [];
  CONTRACT {{ }}
  BODY {{
    %0 = FCLAMP(x, lo, hi);
    RETURN(%0);
  }}
}}
NODE @cs {{
  SIGNATURE: (x: {TN}, y: {TN}) -> ({TN});
  EFFECTS: [];
  CONTRACT {{ }}
  BODY {{
    %0 = FCOPYSIGN(x, y);
    RETURN(%0);
  }}
}}
"
                );
                let m = Module::new(&src);
                let ecl = exe(&m, "cl", true, true);
                let ecs = exe(&m, "cs", true, true);
                let bounds: Vec<F> = vec![-1.0, -0.0, 0.0, 1.0, F::NEG_INFINITY, F::INFINITY, F::NAN];
                for &x in &edges() {
                    for &lo in &bounds {
                        for &hi in &bounds {
                            let o = run(&ecl, &cat(&[enc(x), enc(lo), enc(hi)]));
                            let ctx = format!("FCLAMP x={x:e} lo={lo:e} hi={hi:e}");
                            if lo.is_nan() || hi.is_nan() || lo > hi {
                                assert_eq!(o.code, 4, "{ctx} must trap");
                                assert!(o.stderr.contains("FCLAMP.bounds"), "{ctx}: {}", o.stderr);
                            } else {
                                let mut want = Vec::new();
                                push(&mut want, fmin(fmax(x, lo), hi));
                                assert_eq!(o.code, 0, "{ctx}: {}", o.stderr);
                                assert_eq!(o.stdout, want, "{ctx}");
                            }
                        }
                    }
                    for &y in &edges() {
                        let o = run(&ecs, &cat(&[enc(x), enc(y)]));
                        let ctx = format!("FCOPYSIGN x={x:e} y={y:e}");
                        if y.is_nan() {
                            assert_eq!(o.code, 4, "{ctx} must trap");
                            assert!(o.stderr.contains("FCOPYSIGN.sign"), "{ctx}: {}", o.stderr);
                        } else {
                            let mut want = Vec::new();
                            push(&mut want, x.copysign(y));
                            assert_eq!(o.code, 0, "{ctx}: {}", o.stderr);
                            assert_eq!(o.stdout, want, "{ctx}");
                        }
                    }
                }
            }

            #[test]
            fn a_non_canonical_nan_input_is_rejected_with_exit_2() {
                let src = format!(
                    "module m;
NODE @id {{
  SIGNATURE: (a: {TN}) -> ({TN});
  EFFECTS: [];
  CONTRACT {{ }}
  BODY {{
    %0 = FNEG(a);
    RETURN(%0);
  }}
}}
"
                );
                let m = Module::new(&src);
                let e = exe(&m, "id", false, true);
                let canonical = F::NAN.to_bits();
                let payload: $B = canonical | 1;
                let negative: $B = canonical | (1 << (std::mem::size_of::<$B>() * 8 - 1));
                for bad in [payload, negative] {
                    let o = run(&e, &bad.to_le_bytes());
                    assert_eq!(o.code, 2, "bits {bad:#x}");
                    assert!(o.stdout.is_empty());
                }
                let o = run(&e, &canonical.to_le_bytes());
                assert_eq!(o.code, 0);
                assert_eq!(o.stdout, F::NAN.to_bits().to_le_bytes());
            }
        }
    };
}

float_suite!(f32_ops, f32, u32, "f32", 0x007f_ffffu32);
float_suite!(f64_ops, f64, u64, "f64", 0x000f_ffff_ffff_ffffu64);

#[test]
fn casts_between_integers_and_floats_round_and_trap_like_docs_14() {
    let src = "module m;
NODE @f32_i32 { SIGNATURE: (x: f32) -> (i32); EFFECTS: []; CONTRACT { } BODY { %0 = CAST.i32(x); RETURN(%0); } }
NODE @f64_u8 { SIGNATURE: (x: f64) -> (u8); EFFECTS: []; CONTRACT { } BODY { %0 = CAST.u8(x); RETURN(%0); } }
NODE @i64_f32 { SIGNATURE: (x: i64) -> (f32); EFFECTS: []; CONTRACT { } BODY { %0 = CAST.f32(x); RETURN(%0); } }
NODE @u64_f64 { SIGNATURE: (x: u64) -> (f64); EFFECTS: []; CONTRACT { } BODY { %0 = CAST.f64(x); RETURN(%0); } }
NODE @f64_f32 { SIGNATURE: (x: f64) -> (f32); EFFECTS: []; CONTRACT { } BODY { %0 = CAST.f32(x); RETURN(%0); } }
NODE @f32_f64 { SIGNATURE: (x: f32) -> (f64); EFFECTS: []; CONTRACT { } BODY { %0 = CAST.f64(x); RETURN(%0); } }
";
    let m = Module::new(src);

    let e = exe(&m, "f32_i32", true, true);
    for x in [
        0.0f32,
        -0.0,
        1.9,
        -1.9,
        0.5,
        -0.5,
        2_147_483_520.0,
        2_147_483_648.0,
        -2_147_483_648.0,
        -2_147_483_904.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ] {
        let o = run(&e, &f32s(&[x]));
        let t = x.trunc();
        if x.is_finite() && (-2_147_483_648.0..2_147_483_648.0).contains(&t) {
            assert_eq!(o.code, 0, "f32->i32 {x}: {}", o.stderr);
            assert_eq!(o.stdout, le(t as i128, 4), "f32->i32 {x}");
        } else {
            assert_eq!(o.code, 4, "f32->i32 {x} must trap");
        }
    }

    let e = exe(&m, "f64_u8", true, true);
    for x in [
        255.9f64,
        256.0,
        -0.9,
        -1.0,
        0.0,
        -0.0,
        254.5,
        f64::NAN,
        1e300,
        f64::NEG_INFINITY,
    ] {
        let o = run(&e, &x.to_bits().to_le_bytes());
        let t = x.trunc();
        if x.is_finite() && (0.0..256.0).contains(&t) {
            assert_eq!(o.code, 0, "f64->u8 {x}: {}", o.stderr);
            assert_eq!(o.stdout, vec![t as u8], "f64->u8 {x}");
        } else {
            assert_eq!(o.code, 4, "f64->u8 {x} must trap");
        }
    }

    let e = exe(&m, "i64_f32", true, true);
    for x in [
        0i64,
        1,
        -1,
        16_777_217,
        -16_777_217,
        16_777_219,
        i64::MAX,
        i64::MIN,
        9_007_199_254_740_993,
    ] {
        let o = run(&e, &x.to_le_bytes());
        assert_eq!(o.code, 0);
        assert_eq!(o.stdout, f32s(&[x as f32]), "i64->f32 {x}");
    }

    let e = exe(&m, "u64_f64", true, true);
    for x in [
        0u64,
        1,
        9_007_199_254_740_993,
        9_007_199_254_740_995,
        u64::MAX,
        u64::MAX - 1024,
    ] {
        let o = run(&e, &x.to_le_bytes());
        assert_eq!(o.code, 0);
        assert_eq!(o.stdout, (x as f64).to_bits().to_le_bytes(), "u64->f64 {x}");
    }

    let e = exe(&m, "f64_f32", true, true);
    for x in [
        1e39f64,
        -1e39,
        1e-46,
        -1e-46,
        1.000_000_059_604_644_8,
        1.000_000_059_604_645,
        3.402_823_567_797_336_6e38,
        f64::NAN,
        -0.0,
        f64::INFINITY,
    ] {
        let o = run(&e, &x.to_bits().to_le_bytes());
        assert_eq!(o.code, 0);
        assert_eq!(o.stdout, f32s(&[canon32(x as f32)]), "f64->f32 {x:e}");
    }

    let e = exe(&m, "f32_f64", true, true);
    for x in [
        1.5f32,
        f32::from_bits(1),
        f32::MAX,
        -0.0,
        f32::INFINITY,
        f32::NAN,
    ] {
        let o = run(&e, &f32s(&[x]));
        let want = if x.is_nan() { f64::NAN } else { f64::from(x) };
        assert_eq!(o.code, 0);
        assert_eq!(o.stdout, want.to_bits().to_le_bytes(), "f32->f64 {x:e}");
    }
}

// ---------------------------------------------------------------------------------------------
// Combinators, guards and calls (tests/corpus/alpha and examples/quickstart.vgs)
// ---------------------------------------------------------------------------------------------

#[test]
fn simd_map_over_an_array_with_a_learnable_broadcast() {
    // a29: process_array(samples: Array<f32, 4>) with θ gain in [0.5, 2.0] (default: the lower bound).
    let m = Module::new(&corpus("tests/corpus/alpha/a29_simd_map_array/01.vgs"));
    let e = exe(&m, "process_array", true, true);
    let samples = [-3.0f32, 0.25, -0.0, 100.0];
    let want = |g: f32| -> Vec<u8> {
        let v: Vec<f32> = samples
            .iter()
            .map(|x| fmin32(fmax32(*x * g, -2.0), 2.0))
            .collect();
        f32s(&v)
    };
    for g in [0.5f32, 1.0, 2.0] {
        let o = run(&e, &cat(&[f32s(&samples), f32s(&[g])]));
        assert_eq!(o.code, 0, "gain {g}: {}", o.stderr);
        assert_eq!(o.stdout, want(g), "gain {g}");
    }
    let o = run(&e, &f32s(&samples));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(o.stdout, want(0.5), "default theta is the lower bound");
    assert_eq!(run(&e, &cat(&[f32s(&samples), f32s(&[2.5])])).code, 1);
    assert_eq!(run(&e, &cat(&[f32s(&samples), f32s(&[0.25])])).code, 1);
    assert_eq!(
        run(&e, &f32s(&[f32::NAN, 0.0, 0.0, 0.0])).code,
        1,
        "PRE is_finite"
    );
    assert_eq!(run(&e, &f32s(&[f32::INFINITY, 0.0, 0.0, 0.0])).code, 1);
    assert_eq!(
        run(&e, &0x7fc0_0001u32.to_le_bytes().repeat(4)).code,
        2,
        "non-canonical NaN"
    );
}

#[test]
fn simd_map_over_a_tensor_and_a_slice() {
    // a30: soft threshold of a 4x2 tensor, bias broadcast.
    let m = Module::new(&corpus("tests/corpus/alpha/a30_simd_map_tensor/01.vgs"));
    let e = exe(&m, "filter_matrix", true, true);
    let mat = [1.5f32, -1.5, 0.25, -0.25, 3.0, -3.0, 0.0, -0.0];
    for bias in [0.0f32, 0.5, 2.0] {
        let want: Vec<f32> = mat
            .iter()
            .map(|x| fmax32(x.abs() - bias, 0.0).copysign(*x))
            .collect();
        let o = run(&e, &cat(&[f32s(&mat), f32s(&[bias])]));
        assert_eq!(o.code, 0, "bias {bias}: {}", o.stderr);
        assert_eq!(o.stdout, f32s(&want), "bias {bias}");
    }
    assert_eq!(run(&e, &cat(&[f32s(&mat), f32s(&[-1.0])])).code, 1);

    // a31: a slice result is allocated in the arena.
    let m = Module::new(&corpus("tests/corpus/alpha/a31_simd_map_slice/01.vgs"));
    let e = exe(&m, "transform_slice", true, true);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for len in [0usize, 1, 5, 100, 1000] {
        let xs: Vec<u32> = (0..len).map(|_| rng.next() as u32).collect();
        let mask = 0xF0F0_1234u32;
        let mut input = le(len as i128, 8);
        input.extend(xs.iter().flat_map(|x| x.to_le_bytes()));
        input.extend(mask.to_le_bytes());
        let mut want = le(len as i128, 8);
        want.extend(
            xs.iter()
                .flat_map(|x| (((x & mask).rotate_left(3)) ^ mask).to_le_bytes()),
        );
        let o = run(&e, &input);
        assert_eq!(o.code, 0, "len {len}: {}", o.stderr);
        assert_eq!(o.stdout, want, "len {len}");
    }
}

#[test]
fn fold_bounded_matches_a_rust_fold() {
    // a32: acc + (elem << shift) with wrapping semantics.
    let m = Module::new(&corpus("tests/corpus/alpha/a32_fold_bounded_basic/01.vgs"));
    let e = exe(&m, "sum_shifted_array", true, true);
    for (items, shift) in [
        ([1u64, 2, 3, 4], 0u32),
        ([1, 2, 3, 4], 3),
        ([u64::MAX, u64::MAX, 1, 0], 1),
        ([1 << 63, 1 << 63, 5, 6], 64),
        ([7, 8, 9, 10], 100),
    ] {
        let want = items
            .iter()
            .fold(0u64, |acc, x| acc.wrapping_add(x.wrapping_shl(shift)));
        let mut input: Vec<u8> = items.iter().flat_map(|x| x.to_le_bytes()).collect();
        input.extend(shift.to_le_bytes());
        let o = run(&e, &input);
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.stdout, want.to_le_bytes(), "{items:?} << {shift}");
    }

    // quickstart: sum4 with PRE xs[i] <= 1000, and add_step's own PRE.
    let q = Module::new(&corpus("examples/quickstart.vgs"));
    let e = exe(&q, "sum4", true, true);
    let o = run(
        &e,
        &[1u32, 2, 3, 994]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect::<Vec<u8>>(),
    );
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(o.stdout, 1000u32.to_le_bytes());
    let o = run(
        &e,
        &[1000u32, 1000, 1000, 1000]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect::<Vec<u8>>(),
    );
    assert_eq!(o.stdout, 4000u32.to_le_bytes());
    let o = run(
        &e,
        &[1u32, 2, 3, 1001]
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect::<Vec<u8>>(),
    );
    assert_eq!(o.code, 1, "PRE xs[i] <= 1000");
    let e = exe(&q, "add_step", true, true);
    assert_eq!(
        run(&e, &cat(&[le(3000, 4), le(1000, 4)])).stdout,
        4000u32.to_le_bytes()
    );
    assert_eq!(run(&e, &cat(&[le(3001, 4), le(1, 4)])).code, 1);
}

#[test]
fn generate_builds_arrays_and_slices() {
    // a35: Array<u64, 8> from a literal count; the kernel is a saturating product.
    let m = Module::new(&corpus(
        "tests/corpus/alpha/a35_generate_literal_count/01.vgs",
    ));
    let e = exe(&m, "gen_table", true, true);
    for step in [0u64, 1, 3, u64::MAX / 3, u64::MAX] {
        let want: Vec<u8> = (0..8u64)
            .flat_map(|i| i.saturating_mul(step).to_le_bytes())
            .collect();
        let o = run(&e, &step.to_le_bytes());
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.stdout, want, "step {step}");
    }

    // a36: a Slice<f32> whose length is an input.
    let m = Module::new(&corpus("tests/corpus/alpha/a36_generate_port_count/01.vgs"));
    let e = exe(&m, "create_ramp", true, true);
    for count in [0u64, 1, 7, 100] {
        for base in [0.5f32, -3.0, 1e30] {
            let mut want = le(i128::from(count), 8);
            want.extend(f32s(
                &(0..count).map(|i| (i as f32) + base).collect::<Vec<_>>(),
            ));
            let o = run(&e, &cat(&[le(i128::from(count), 8), f32s(&[base])]));
            assert_eq!(o.code, 0, "count {count}: {}", o.stderr);
            assert_eq!(o.stdout, want, "count {count} base {base}");
        }
    }
    assert_eq!(
        run(&e, &cat(&[le(3, 8), f32s(&[f32::INFINITY])])).code,
        1,
        "PRE is_finite(base)"
    );
}

#[test]
fn assert_guard_dispatches_and_keeps_a_runtime_guard() {
    // a37: denom != 0 selects fast_div, otherwise fallback_div returns 0.
    let m = Module::new(&corpus("tests/corpus/alpha/a37_assert_guard_basic/01.vgs"));
    let e = exe(&m, "guarded_division", true, true);
    assert!(
        e.residual_guards >= 1,
        "an unproven ASSERT_GUARD is a residual guard, got {}",
        e.residual_guards
    );
    for (x, d) in [
        (7i32, 2i32),
        (-7, 2),
        (7, -2),
        (5, 0),
        (i32::MIN, 1),
        (i32::MAX, -1),
        (0, 0),
    ] {
        let want = if d == 0 { 0 } else { x / d };
        let o = run(&e, &cat(&[le(i128::from(x), 4), le(i128::from(d), 4)]));
        assert_eq!(o.code, 0, "{x}/{d}: {}", o.stderr);
        assert_eq!(o.stdout, le(i128::from(want), 4), "{x}/{d}");
    }
    let o = run(&e, &cat(&[le(i128::from(i32::MIN), 4), le(-1, 4)]));
    assert_eq!(o.code, 4, "DIV.checked MIN / -1 inside fast_div must trap");

    // a38: ASSERT_GUARD.branch is control flow, never counted as a guard.
    let m = Module::new(&corpus("tests/corpus/alpha/a38_assert_guard_branch/01.vgs"));
    let e = exe(&m, "select_operation", true, true);
    assert_eq!(e.residual_guards, 0, "a branch is not a guard");
    let enc =
        |c: u8, a: u32, b: u32| cat(&[vec![c], a.to_le_bytes().to_vec(), b.to_le_bytes().to_vec()]);
    let o = run(&e, &enc(1, 7, 5));
    assert_eq!(o.code, 0, "{}", o.stderr);
    assert_eq!(
        o.stdout,
        cat(&[12u32.to_le_bytes().to_vec(), 35u32.to_le_bytes().to_vec()])
    );
    let o = run(&e, &enc(0, 7, 5));
    assert_eq!(
        o.stdout,
        cat(&[2u32.to_le_bytes().to_vec(), 5u32.to_le_bytes().to_vec()])
    );
    let o = run(&e, &enc(0, 5, 7));
    assert_eq!(
        o.stdout,
        cat(&[
            5u32.wrapping_sub(7).to_le_bytes().to_vec(),
            5u32.to_le_bytes().to_vec()
        ])
    );
    assert_eq!(
        run(&e, &enc(1, u32::MAX, 2)).code,
        4,
        "ADD.checked overflow"
    );
    assert_eq!(
        run(&e, &enc(2, 1, 1)).code,
        2,
        "Bool byte 2 is an invalid encoding"
    );
}

#[test]
fn call_hash_calls_the_callee_and_as_bytes_views_an_array() {
    // a22: two calls with multiple results.
    let m = Module::new(&corpus("tests/corpus/alpha/a22_multi_node_call/01.vgs"));
    let e = exe(&m, "pipeline_proc", true, true);
    for (x, y, s) in [
        (1.5f32, 0.25, 2.0),
        (3e38, 3e38, 0.0),
        (-0.0, 0.0, 1.0),
        (1e-45, 1e-45, 0.5),
        (2.0, 3.0, -4.0),
    ] {
        let sum = x + y;
        let diff = x - y;
        let want = f32s(&[canon32(sum * s), canon32(diff * s)]);
        let o = run(&e, &f32s(&[x, y, s]));
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.stdout, want, "x={x:e} y={y:e} s={s:e}");
    }

    // a21: AS_BYTES of an Array<f32, 4> passed to a callee taking a Slice<u8>.
    let m = Module::new(&corpus("tests/corpus/alpha/a21_as_bytes_hash/01.vgs"));
    let e = exe(&m, "digest_floats", true, true);
    for arr in [[1.0f32, 2.0, 3.0, 4.0], [-0.5, 0.0, 7.25, 1e10], [0.0; 4]] {
        let b = arr[0].to_le_bytes();
        let want = u32::from(b[0]) + u32::from(b[1]);
        let o = run(&e, &f32s(&arr));
        assert_eq!(o.code, 0, "{}", o.stderr);
        assert_eq!(o.stdout, want.to_le_bytes(), "{arr:?}");
    }
    let e = exe(&m, "sum_byte_pair", true, true);
    let slice = |b: &[u8]| cat(&[le(b.len() as i128, 8), b.to_vec()]);
    assert_eq!(run(&e, &slice(&[200, 100, 9])).stdout, 300u32.to_le_bytes());
    assert_eq!(run(&e, &slice(&[1])).code, 1, "PRE len(bytes) >= 2");
    assert_eq!(run(&e, &slice(&[])).code, 1);
}

#[test]
fn quickstart_nodes_match_independent_oracles() {
    let q = Module::new(&corpus("examples/quickstart.vgs"));

    let e = exe(&q, "add_sat", true, true);
    for (a, b) in [
        (200u8, 100u8),
        (0, 0),
        (255, 1),
        (254, 1),
        (1, 254),
        (255, 255),
    ] {
        let o = run(&e, &[a, b]);
        assert_eq!(o.stdout, vec![a.saturating_add(b)], "{a}+{b}");
    }
    assert_eq!(run(&e, &[200, 100]).stdout, vec![255], "quickstart // run:");

    let e = exe(&q, "midpoint", true, true);
    for (a, b) in [
        (u32::MAX, u32::MAX - 2),
        (0, 0),
        (1, 2),
        (u32::MAX, 0),
        (123_456_789, 987_654_321),
    ] {
        let want = ((u64::from(a) + u64::from(b)) / 2) as u32;
        let o = run(
            &e,
            &cat(&[a.to_le_bytes().to_vec(), b.to_le_bytes().to_vec()]),
        );
        assert_eq!(o.stdout, want.to_le_bytes(), "midpoint {a} {b}");
    }

    let e = exe(&q, "soft_knee", true, true);
    let knee_edges = [0.5f32, 0.7, 0.99];
    let xs = [
        0.0f32,
        -0.0,
        0.25,
        -0.25,
        0.6,
        -0.6,
        1.0,
        3e38,
        -3e38,
        f32::MAX,
        -f32::MAX,
        f32::from_bits(1),
        -f32::from_bits(1),
        f32::MIN_POSITIVE,
    ];
    for &knee in &knee_edges {
        for &x in &xs {
            let ax = x.abs();
            let t3 = 1.0 - knee;
            let t4 = fmax32(ax - knee, 0.0) / t3;
            let t7 = 1.0 - 1.0 / (1.0 + t4);
            let t9 = t3.mul_add(t7, knee).copysign(x);
            let t11 = if ax <= knee { x } else { t9 };
            let want = fmin32(fmax32(t11, -1.0), 1.0);
            let o = run(&e, &f32s(&[x, knee]));
            assert_eq!(o.code, 0, "x={x:e} knee={knee}: {}", o.stderr);
            assert_eq!(o.stdout, f32s(&[canon32(want)]), "x={x:e} knee={knee}");
        }
    }
    assert_eq!(
        run(&e, &f32s(&[3e38, 0.5])).stdout,
        f32s(&[1.0]),
        "quickstart // run:"
    );
    assert_eq!(
        run(&e, &f32s(&[f32::INFINITY, 0.5])).code,
        1,
        "PRE is_finite(x)"
    );
    assert_eq!(run(&e, &f32s(&[0.0, 0.4])).code, 1, "PRE knee >= 0.5");
}

// ---------------------------------------------------------------------------------------------
// Targets without a sysroot: object files only
// ---------------------------------------------------------------------------------------------

#[test]
fn x86_64_and_wasm32_builds_have_their_format() {
    let q = Module::new(&corpus("examples/quickstart.vgs"));
    for (target, magic) in [
        (Target::X86_64Linux, &[0x7f, b'E', b'L', b'F'][..]),
        (Target::Wasm32Wasi, &[0x00, b'a', b's', b'm'][..]),
    ] {
        for label in ["add_sat", "sum4", "soft_knee"] {
            let dir = scratch_dir();
            let out = dir.join("obj");
            let rep = q
                .build_to(
                    label,
                    &Options {
                        target,
                        proof_facts: true,
                        runtime_checks: true,
                    },
                    &out,
                )
                .unwrap_or_else(|d| panic!("{target:?} {label}: {d:?}"));
            assert_eq!(rep.artifact, out);
            let bytes = std::fs::read(&out).unwrap();
            assert!(bytes.starts_with(magic), "{target:?} {label}: bad magic");
            assert!(bytes.len() > 100);
            if target == Target::X86_64Linux {
                // e_type: 1 is a relocatable object, 2 or 3 an executable.
                let e_type = u16::from_le_bytes([bytes[16], bytes[17]]);
                assert_eq!(e_type == 1, !rep.linked, "{label}: e_type {e_type}");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// Two builds of one node give the same bytes on every target (AGENTS.md: determinism); zig once
/// recorded each build's temporary directory in the x86_64 executable.
#[test]
fn builds_are_byte_for_byte_reproducible() {
    let q = Module::new(&corpus("examples/quickstart.vgs"));
    let mut targets = vec![Target::X86_64Linux, Target::Wasm32Wasi];
    if cfg!(target_os = "macos") {
        targets.push(Target::Aarch64Darwin);
    }
    for target in targets {
        let opts = Options {
            target,
            proof_facts: true,
            runtime_checks: false,
        };
        let build = || {
            let dir = scratch_dir();
            let out = dir.join("prog");
            q.build_to("soft_knee", &opts, &out)
                .unwrap_or_else(|d| panic!("{target:?}: {d:?}"));
            let bytes = std::fs::read(&out).unwrap();
            let _ = std::fs::remove_dir_all(&dir);
            bytes
        };
        assert!(build() == build(), "{target:?}: two builds differ");
    }
}

// ---------------------------------------------------------------------------------------------
// Corpus sweep: every effect-free node of tests/corpus/alpha
// ---------------------------------------------------------------------------------------------

fn gen_value(ty: &Type, rng: &mut Rng, zero: bool, out: &mut Vec<u8>) -> bool {
    match ty {
        Type::Bool => out.push(if zero { 0 } else { (rng.next() & 1) as u8 }),
        Type::UInt(w) | Type::SInt(w) => {
            let mut v = if zero { 0 } else { rng.next() };
            if rng.next().is_multiple_of(2) {
                v %= 16;
            }
            let n = usize::from(*w / 8);
            let mut bytes = v.to_le_bytes().to_vec();
            bytes.resize(n.max(8), 0);
            out.extend(&bytes[..n]);
        }
        Type::Float(32) | Type::Float(64) => {
            let table = [0.0f64, -0.0, 1.0, 0.5, -2.5, 100.0, 0.25, 7.0];
            let x = if zero {
                0.0
            } else {
                table[(rng.next() % 8) as usize]
            };
            if matches!(ty, Type::Float(32)) {
                out.extend((x as f32).to_bits().to_le_bytes());
            } else {
                out.extend(x.to_bits().to_le_bytes());
            }
        }
        Type::Float(16) => out.extend(if zero { 0u16 } else { 0x3c00 }.to_le_bytes()),
        Type::BFloat16 => out.extend(if zero { 0u16 } else { 0x3f80 }.to_le_bytes()),
        Type::Array(elem, n) => {
            for _ in 0..*n {
                if !gen_value(elem, rng, zero, out) {
                    return false;
                }
            }
        }
        Type::Tensor(elem, dims) => {
            for _ in 0..dims.iter().map(|d| *d as usize).product::<usize>() {
                if !gen_value(elem, rng, zero, out) {
                    return false;
                }
            }
        }
        Type::Slice(elem) => {
            let len = if zero { 0 } else { rng.next() % 5 };
            out.extend(len.to_le_bytes());
            for _ in 0..len {
                if !gen_value(elem, rng, zero, out) {
                    return false;
                }
            }
        }
        _ => return false,
    }
    true
}

fn alpha_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/alpha");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    let mut files = Vec::new();
    for d in dirs {
        let mut fs: Vec<PathBuf> = std::fs::read_dir(d)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "vgs"))
            .collect();
        fs.sort();
        files.extend(fs);
    }
    files
}

/// Builds and smoke-runs every effect-free node of the files in this shard. A node the backend
/// cannot lower must produce an error diagnostic; it is listed, never run.
fn sweep(shard: usize, shards: usize) {
    let mut built = 0usize;
    let mut unsupported: Vec<String> = Vec::new();
    let mut idx = 0usize;
    for file in alpha_files() {
        let src = std::fs::read_to_string(&file).unwrap();
        let Ok(all) = asm_local(&src) else { continue };
        let by_hash: HashMap<Hash, Node> = all
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        for a in &all {
            let n = &a.node;
            if n.spec || n.effects.0 & !EffectMask::ARENA != 0 {
                continue;
            }
            idx += 1;
            if idx % shards != shard {
                continue;
            }
            let name = format!(
                "{}/{}::{}",
                file.parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_string_lossy(),
                file.file_name().unwrap().to_string_lossy(),
                a.label
            );
            let dir = scratch_dir();
            let mut exes = Vec::new();
            let mut failed = None;
            for facts in [false, true] {
                let path = dir.join(format!("prog-{facts}"));
                match build(
                    n,
                    &|h| by_hash.get(h).cloned(),
                    &aarch64(facts, true),
                    &path,
                ) {
                    Ok(rep) => {
                        assert!(rep.linked);
                        exes.push(path);
                    }
                    Err(d) => {
                        failed = Some(d);
                        break;
                    }
                }
            }
            if let Some(d) = failed {
                unsupported.push(format!("{name}: {} {}", d.code, d.message));
                let _ = std::fs::remove_dir_all(&dir);
                continue;
            }
            built += 1;
            // The two other targets must build too: linked where their toolchain is installed.
            for target in [Target::X86_64Linux, Target::Wasm32Wasi] {
                let rep = build(
                    n,
                    &|h| by_hash.get(h).cloned(),
                    &Options {
                        target,
                        proof_facts: true,
                        runtime_checks: true,
                    },
                    &dir.join("obj"),
                )
                .unwrap_or_else(|d| panic!("{name} {target:?}: {d:?}"));
                assert!(rep.artifact.is_file(), "{name} {target:?}: no artifact");
            }
            let holder: Vec<Exe> = exes
                .iter()
                .map(|p| Exe {
                    dir: dir.clone(),
                    path: p.clone(),
                    residual_guards: 0,
                })
                .collect();
            for seed in 0..4u64 {
                let mut rng = Rng(0x2545_f491_4f6c_dd1d ^ (idx as u64 * 7919 + seed));
                let mut input = Vec::new();
                let mut ok = true;
                for p in &n.params {
                    ok &= gen_value(p, &mut rng, seed == 0, &mut input);
                }
                if !ok {
                    break;
                }
                let plain = run(&holder[0], &input);
                let fast = run(&holder[1], &input);
                assert_eq!(
                    plain.code, fast.code,
                    "{name} seed {seed}: facts changed the exit code"
                );
                assert!(plain.code <= 4, "{name}: exit {}", plain.code);
                assert_eq!(
                    plain.stdout, fast.stdout,
                    "{name} seed {seed}: facts changed the output"
                );
                if plain.code != 0 {
                    assert!(plain.stdout.is_empty(), "{name}: output written on failure");
                }
            }
            drop(holder);
        }
    }
    println!(
        "sweep shard {shard}: {built} built and run, {} not lowered",
        unsupported.len()
    );
    for u in &unsupported {
        println!("  not lowered: {u}");
    }
    assert!(
        unsupported.is_empty(),
        "nodes the backend rejects (each with a diagnostic):\n{}",
        unsupported.join("\n")
    );
}

macro_rules! sweep_shard {
    ($name:ident, $i:expr) => {
        #[test]
        fn $name() {
            sweep($i, 4);
        }
    };
}

sweep_shard!(corpus_sweep_shard_0, 0);
sweep_shard!(corpus_sweep_shard_1, 1);
sweep_shard!(corpus_sweep_shard_2, 2);
sweep_shard!(corpus_sweep_shard_3, 3);
