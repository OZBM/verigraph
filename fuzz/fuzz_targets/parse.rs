//! Arbitrary bytes: no panic; whatever parses prints and parses back to the same tree, and
//! whatever assembles decodes back to the same node (docs/16, "Fuzzing").
#![no_main]

use libfuzzer_sys::fuzz_target;
use vg_core::{asm_local, codec, parser, printer};

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else { return };
    let Ok(m) = parser::parse(src) else { return };
    let text = printer::print(&m);
    let again = parser::parse(&text).expect("printed text parses");
    assert_eq!(m, again, "print then parse changed the tree");
    if let Ok(nodes) = asm_local(src) {
        for a in nodes {
            assert_eq!(codec::decode(&a.bytes).expect("assembled bytes decode"), a.node);
        }
    }
});
