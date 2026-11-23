//! Regression tests for the findings of reports/gate-g1-review.md (appendix A). r01-r03 and r06 now
//! expect a rejection: the fix bounds the widths the format writes in one byte (VG-T0118) and checks
//! Eff linearity for every node (VG-E0102), so the colliding programs no longer assemble.

mod common;

use std::fs;

use common::repo;
use vg_core::canon::canonicalize;
use vg_core::codec::{decode, encode};
use vg_core::ir::{Contract, Node, Op, Operand};
use vg_core::types::{EffectMask, Handle, Type};
use vg_core::{asm_local, opcode};

fn list(item: &str, n: usize) -> String {
    vec![item; n].join(", ")
}

fn identity_node(t: &str) -> String {
    format!(
        "module r;\n\nNODE @id {{\n  SIGNATURE: (x: {t}) -> ({t});\n  EFFECTS: [];\n  CONTRACT {{ }}\n  BODY {{\n    %0 = IDENTITY(x);\n    RETURN(%0);\n  }}\n}}\n"
    )
}

fn rejected_with(src: &str, code: &str) {
    match asm_local(src) {
        Ok(_) => panic!("assembled, but {code} was expected"),
        Err(e) => assert_eq!(e.code, code, "{e}"),
    }
}

/// B1: a 257-member tuple used to write its member count as 1 and collide with another type.
#[test]
fn r01_tuples_wider_than_255_are_rejected() {
    rejected_with(
        &identity_node(&format!("Tuple<Tuple<{}>, u8>", list("u8", 257))),
        "VG-T0118",
    );
    rejected_with(
        &identity_node(&format!("Tensor<u8, [{}]>", list("1", 255))),
        "VG-T0118",
    );
}

/// B2: result slot 256 of a 257-result CALL_HASH used to alias slot 0.
#[test]
fn r02_more_than_255_results_are_rejected() {
    let rets = std::iter::once("y")
        .chain(std::iter::repeat_n("x", 256))
        .collect::<Vec<_>>()
        .join(", ");
    let src = format!(
        "module r;\n\nNODE @g {{\n  SIGNATURE: (x: u8, y: u8) -> ({});\n  EFFECTS: [];\n  CONTRACT {{ }}\n  BODY {{\n    RETURN({rets});\n  }}\n}}\n",
        list("u8", 257)
    );
    rejected_with(&src, "VG-T0118");
}

/// B3: the widest legal tuple and tensor assemble, decode and re-encode byte for byte.
#[test]
fn r03_widest_legal_types_round_trip() {
    for t in [
        format!("Tuple<{}>", list("u8", 255)),
        format!("Tensor<u8, [{}]>", list("1", 254)),
    ] {
        let a = asm_local(&identity_node(&t)).expect("assembles");
        let n = decode(&a[0].bytes).expect("vg asm output decodes");
        assert_eq!(encode(&n).0, a[0].bytes);
    }
}

fn golden(name: &str) -> Vec<u8> {
    fs::read(repo().join("tests/golden").join(name)).expect("golden file")
}

#[test]
fn r04_decoder_refuses_a_wrong_header_hash() {
    for (at, what) in [(16, "node"), (48, "body"), (80, "contract")] {
        let mut b = golden("minimal.inc.vgir");
        assert!(decode(&b).is_ok());
        b[at] ^= 1;
        assert!(
            decode(&b).is_err(),
            "a flipped {what} hash byte was accepted"
        );
    }
}

#[test]
fn r05_decoder_refuses_trailing_and_truncated_bytes() {
    let b = golden("sections.all.vgir");
    assert!(decode(&b).is_ok());
    let mut longer = b.clone();
    longer.extend([0u8; 16]);
    assert!(decode(&longer).is_err(), "16 trailing zero bytes accepted");
    assert!(
        decode(&b[..b.len() - 16]).is_err(),
        "truncated file accepted"
    );
}

fn api_node(
    effects: u32,
    params: Vec<Type>,
    results: Vec<Type>,
    ops: Vec<Op>,
    outputs: Vec<Operand>,
) -> Node {
    Node {
        spec: false,
        effects: EffectMask(effects),
        params,
        results,
        learnable: vec![],
        consts: vec![],
        calls: vec![],
        ops,
        outputs,
        contract: Contract::default(),
        debug: None,
    }
}

fn clock_read(eff: Operand, clock: u32) -> Op {
    Op {
        opcode: opcode::by_name("CLOCK_READ").unwrap().code,
        mode: 0,
        operands: vec![eff, Operand::Input(clock)],
        results: vec![Type::Eff, Type::UInt(64)],
    }
}

/// B4: two CLOCK_READs on one Eff token are not one read. Such a node forks its Eff token, so it is
/// rejected before step 5 could merge anything; a linear chain of two reads keeps both.
#[test]
fn r06_effect_ops_are_never_merged() {
    let clock = EffectMask::bit("CLOCK").unwrap();
    let sig = vec![Type::Eff, Type::Handle(Handle::Clock)];
    let forked = api_node(
        clock,
        sig.clone(),
        vec![Type::UInt(64), Type::UInt(64)],
        vec![
            clock_read(Operand::Input(0), 1),
            clock_read(Operand::Input(0), 1),
        ],
        vec![Operand::Op(0, 1), Operand::Op(1, 1)],
    );
    assert_eq!(
        canonicalize(&forked).err().map(|e| e.code),
        Some("VG-E0102")
    );
    let chain = api_node(
        clock,
        sig,
        vec![Type::Eff, Type::UInt(64), Type::UInt(64)],
        vec![
            clock_read(Operand::Input(0), 1),
            clock_read(Operand::Op(0, 0), 1),
        ],
        vec![Operand::Op(1, 0), Operand::Op(0, 1), Operand::Op(1, 1)],
    );
    assert_eq!(canonicalize(&chain).expect("linear").node.ops.len(), 2);
}

/// B5: the decoder is the gate for stored and Mesh nodes; a forked Eff token must not pass it.
#[test]
fn r07_decoder_refuses_a_forked_eff_token() {
    let clock = EffectMask::bit("CLOCK").unwrap();
    let n = api_node(
        clock,
        vec![
            Type::Eff,
            Type::Handle(Handle::Clock),
            Type::Handle(Handle::Clock),
        ],
        vec![Type::UInt(64), Type::UInt(64)],
        vec![
            clock_read(Operand::Input(0), 1),
            clock_read(Operand::Input(0), 2),
        ],
        vec![Operand::Op(0, 1), Operand::Op(1, 1)],
    );
    assert_eq!(canonicalize(&n).err().map(|e| e.code), Some("VG-E0102"));
    // decode() runs canonicalize() on every node it reads (codec.rs), and no header hash can be
    // computed for such a node, so neither the store nor Mesh can hand one back.
    assert!(vg_core::canon::hashes(&n).is_err());
}

#[test]
fn r08_decoder_refuses_a_noncanonical_nan_constant() {
    let mut n = api_node(
        0,
        vec![Type::Float(32)],
        vec![Type::Float(32)],
        vec![],
        vec![Operand::Const(0)],
    );
    n.consts = vec![vg_core::ir::Const {
        ty: Type::Float(32),
        bytes: 0x7FC0_0001u32.to_le_bytes().to_vec(),
    }];
    let (bytes, _) = encode(&canonicalize(&n).expect("valid").node);
    assert!(decode(&bytes).is_err(), "NaN payload constant accepted");
}
