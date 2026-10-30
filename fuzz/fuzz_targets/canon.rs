//! Well-formed programs from the generator, driven by the fuzzer's bytes: canonicalization is
//! idempotent, and a random sequence of equivalence moves keeps every hash (docs/16).
#![no_main]

use libfuzzer_sys::fuzz_target;
use vg_core::generate::{self, Class};
use vg_core::{asm_local, canon, parser, printer};

fuzz_target!(|data: &[u8]| {
    if data.len() < 17 {
        return;
    }
    let seed = u64::from_le_bytes(data[..8].try_into().unwrap());
    let moves = u64::from_le_bytes(data[8..16].try_into().unwrap());
    let class = if data[16] & 1 == 0 { Class::A } else { Class::B };
    let src = generate::program(seed, class, 1 + usize::from(data[16] >> 1) % 60);
    let base = asm_local(&src).expect("generated programs assemble");
    for a in &base {
        assert_eq!(canon::canonicalize(&a.node).unwrap().node, a.node, "canonicalization is not idempotent");
    }
    let (variant, log) = generate::variant(&parser::parse(&src).unwrap(), moves);
    let text = generate::layout(&printer::print(&variant), moves);
    let again = asm_local(&text).expect("variants assemble");
    let mut h1: Vec<_> = base.iter().map(|a| a.hashes).collect();
    let mut h2: Vec<_> = again.iter().map(|a| a.hashes).collect();
    h1.sort_by_key(|h| h.node);
    h2.sort_by_key(|h| h.node);
    assert_eq!(h1, h2, "moves {log:?} changed a hash\n{src}\n{text}");
});
