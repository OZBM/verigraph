//! Arbitrary bytes: no panic; whatever decodes as canonical .vgir encodes to the same bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;
use vg_core::codec;

fuzz_target!(|data: &[u8]| {
    if let Ok(node) = codec::decode(data) {
        assert_eq!(codec::encode(&node).0, data, "decode then encode changed the bytes");
    }
});
