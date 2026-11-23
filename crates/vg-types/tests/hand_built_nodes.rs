use std::collections::BTreeMap;

use vg_core::codec::{decode, encode};
use vg_core::ir::{
    Bin, Builtin, Const, Contract, Hash, Learnable, Node, Op, Operand, Resource, Term,
};
use vg_core::opcode;
use vg_core::types::{EffectMask, Handle, Type};
use vg_types::{CostExpr, check_node, check_source};

fn op_code(name: &str) -> u16 {
    opcode::by_name(name).unwrap().code
}

fn u32_const(v: u32) -> Const {
    Const {
        ty: Type::UInt(32),
        bytes: v.to_le_bytes().to_vec(),
    }
}

fn u64_const(v: u64) -> Const {
    Const {
        ty: Type::UInt(64),
        bytes: v.to_le_bytes().to_vec(),
    }
}

fn f32_const(v: f32) -> Const {
    Const {
        ty: Type::Float(32),
        bytes: v.to_bits().to_le_bytes().to_vec(),
    }
}

fn base_node() -> Node {
    Node {
        spec: false,
        effects: EffectMask(0),
        params: vec![],
        results: vec![],
        learnable: vec![],
        consts: vec![],
        calls: vec![],
        ops: vec![],
        outputs: vec![],
        contract: Contract::default(),
        debug: None,
    }
}

fn no_callees(_: &Hash) -> Option<Node> {
    None
}

fn assert_has_code(node: &Node, callee: &dyn Fn(&Hash) -> Option<Node>, code: &str) {
    let diags =
        check_node(node, callee).expect_err(&format!("expected {code}, but node checked clean"));
    assert!(
        diags.iter().any(|d| d.code == code),
        "expected {code}, got: {diags:#?}"
    );
}

#[test]
fn decoded_node_forked_eff_two_distinct_effects_rejected() {
    // Regression test for gate-g1-review findings B4 and B5:
    // A decoded .vgir node whose single Eff input feeds two distinct effect ops
    // (CLOCK_READ and FILE_WRITE) passes codec::decode (canon::validate does not
    // check Eff linearity), and MUST be rejected by vg-types with VG-E0102.
    let mut n = base_node();
    n.effects = EffectMask(0x0002 | 0x0008); // CLOCK | FILE_WRITE
    n.params = vec![
        Type::Eff,
        Type::Handle(Handle::Clock),
        Type::Handle(Handle::FileOut),
        Type::Slice(Box::new(Type::UInt(8))),
    ];
    n.results = vec![Type::Eff, Type::UInt(64)];
    n.ops = vec![
        Op {
            opcode: op_code("CLOCK_READ"),
            mode: 0,
            operands: vec![Operand::Input(0), Operand::Input(1)],
            results: vec![Type::Eff, Type::UInt(64)],
        },
        Op {
            opcode: op_code("FILE_WRITE"),
            mode: 0,
            // BUG: reuses Input(0) instead of Op(0, 0)!
            operands: vec![Operand::Input(0), Operand::Input(2), Operand::Input(3)],
            results: vec![
                Type::Eff,
                Type::Result(Box::new(Type::UInt(64)), Box::new(Type::UInt(32))),
            ],
        },
    ];
    n.outputs = vec![Operand::Op(1, 0), Operand::Op(0, 1)];

    let canonical = vg_core::canon::canonicalize(&n).unwrap().node;
    let (bytes, _) = encode(&canonical);
    let decoded = decode(&bytes).expect("codec::decode accepts canonical forked-Eff node (B4/B5)");
    let diags = check_node(&decoded, &no_callees)
        .expect_err("vg-types must reject forked Eff token after decode");
    assert!(
        diags.iter().any(|d| d.code == "VG-E0102"),
        "expected VG-E0102, got {diags:#?}"
    );
}

#[test]
fn eff_linearity_edge_cases_on_hand_built_nodes() {
    // 1. Forked Eff into two CLOCK_READs; one result returned, other dropped.
    let mut n = base_node();
    n.effects = EffectMask(0x0002);
    n.params = vec![Type::Eff, Type::Handle(Handle::Clock)];
    n.results = vec![Type::Eff, Type::UInt(64)];
    n.ops = vec![
        Op {
            opcode: op_code("CLOCK_READ"),
            mode: 0,
            operands: vec![Operand::Input(0), Operand::Input(1)],
            results: vec![Type::Eff, Type::UInt(64)],
        },
        Op {
            opcode: op_code("CLOCK_READ"),
            mode: 0,
            operands: vec![Operand::Input(0), Operand::Input(1)],
            results: vec![Type::Eff, Type::UInt(64)],
        },
    ];
    n.outputs = vec![Operand::Op(1, 0), Operand::Op(1, 1)];
    assert_has_code(&n, &no_callees, "VG-E0102");

    // 2. Dropped Eff input (returned Eff comes from nowhere — actually forged or input used 0 times).
    let mut n2 = base_node();
    n2.effects = EffectMask(0x0002);
    n2.params = vec![Type::Eff, Type::Eff, Type::Handle(Handle::Clock)];
    n2.results = vec![Type::Eff, Type::UInt(64)];
    n2.ops = vec![Op {
        opcode: op_code("CLOCK_READ"),
        mode: 0,
        operands: vec![Operand::Input(0), Operand::Input(2)],
        results: vec![Type::Eff, Type::UInt(64)],
    }];
    n2.outputs = vec![Operand::Op(0, 0), Operand::Op(0, 1)];
    assert_has_code(&n2, &no_callees, "VG-E0102");

    // 3. Dropped Eff op result (uses Input(0) for RETURN while CLOCK_READ produces unused Eff).
    let mut n3 = base_node();
    n3.effects = EffectMask(0x0002);
    n3.params = vec![Type::Eff, Type::Handle(Handle::Clock)];
    n3.results = vec![Type::Eff, Type::UInt(64)];
    n3.ops = vec![Op {
        opcode: op_code("CLOCK_READ"),
        mode: 0,
        operands: vec![Operand::Input(0), Operand::Input(1)],
        results: vec![Type::Eff, Type::UInt(64)],
    }];
    n3.outputs = vec![Operand::Input(0), Operand::Op(0, 1)];
    assert_has_code(&n3, &no_callees, "VG-E0102");
}

#[test]
fn structural_and_mvp_op_rejections() {
    // VG-S0101 unknown opcode
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.ops = vec![Op {
        opcode: 0x9999,
        mode: 0,
        operands: vec![Operand::Input(0)],
        results: vec![Type::UInt(32)],
    }];
    n.outputs = vec![Operand::Op(0, 0)];
    let diags = check_node(&n, &no_callees).unwrap_err();
    assert!(
        diags
            .iter()
            .any(|d| d.code == "VG-S0101" && d.op.as_deref() == Some("op 0"))
    );

    // VG-S0102 invalid mode byte
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.ops = vec![Op {
        opcode: op_code("ADD"),
        mode: 7,
        operands: vec![Operand::Input(0), Operand::Input(0)],
        results: vec![Type::UInt(32)],
    }];
    n.outputs = vec![Operand::Op(0, 0)];
    assert_has_code(&n, &no_callees, "VG-S0102");

    // VG-S0103 wrong operand count
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.ops = vec![Op {
        opcode: op_code("ADD"),
        mode: 1,
        operands: vec![Operand::Input(0)],
        results: vec![Type::UInt(32)],
    }];
    n.outputs = vec![Operand::Op(0, 0)];
    assert_has_code(&n, &no_callees, "VG-S0103");

    // VG-S0104 non-MVP opcodes (IDENTITY, CONST, ARENA_ALLOC, NET_READ, PARTITION_BY)
    for name in [
        "CONST",
        "IDENTITY",
        "ARENA_ALLOC",
        "NET_READ",
        "PARTITION_BY",
    ] {
        let mut n = base_node();
        n.params = vec![Type::UInt(32)];
        n.results = vec![Type::UInt(32)];
        n.ops = vec![Op {
            opcode: op_code(name),
            mode: 0,
            operands: vec![Operand::Input(0)],
            results: vec![Type::UInt(32)],
        }];
        n.outputs = vec![Operand::Op(0, 0)];
        assert_has_code(&n, &no_callees, "VG-S0104");
    }

    // VG-S0104 Type::Arena in signature
    let mut n = base_node();
    n.params = vec![Type::Arena(1024)];
    n.results = vec![Type::Arena(1024)];
    n.outputs = vec![Operand::Input(0)];
    assert_has_code(&n, &no_callees, "VG-S0104");

    // VG-S0106 wrong op.results count
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.ops = vec![Op {
        opcode: op_code("ADD"),
        mode: 1,
        operands: vec![Operand::Input(0), Operand::Input(0)],
        results: vec![Type::UInt(32), Type::UInt(32)],
    }];
    n.outputs = vec![Operand::Op(0, 0)];
    assert_has_code(&n, &no_callees, "VG-S0106");

    // VG-S0203 forward op reference
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.ops = vec![Op {
        opcode: op_code("NOT"),
        mode: 0,
        operands: vec![Operand::Op(0, 0)],
        results: vec![Type::UInt(32)],
    }];
    n.outputs = vec![Operand::Op(0, 0)];
    assert_has_code(&n, &no_callees, "VG-S0203");
}

#[test]
fn callee_resolution_cycles_and_spec_calls() {
    let mut spec_node = base_node();
    spec_node.spec = true;
    spec_node.params = vec![Type::UInt(32)];
    spec_node.results = vec![Type::UInt(32)];
    spec_node.outputs = vec![Operand::Input(0)];
    let spec_hash = vg_core::canon::hashes(&spec_node).unwrap().node;

    // VG-S0009 unresolvable callee hash
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.calls = vec![[0xAB; 32]];
    n.ops = vec![Op {
        opcode: op_code("CALL_HASH"),
        mode: 0,
        operands: vec![Operand::Callee(0), Operand::Input(0)],
        results: vec![Type::UInt(32)],
    }];
    n.outputs = vec![Operand::Op(0, 0)];
    assert_has_code(&n, &no_callees, "VG-S0009");

    // VG-S0110 calling a SPEC node from CALL_HASH
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.calls = vec![spec_hash];
    n.ops = vec![Op {
        opcode: op_code("CALL_HASH"),
        mode: 0,
        operands: vec![Operand::Callee(0), Operand::Input(0)],
        results: vec![Type::UInt(32)],
    }];
    n.outputs = vec![Operand::Op(0, 0)];
    assert_has_code(
        &n,
        &|h| (*h == spec_hash).then(|| spec_node.clone()),
        "VG-S0110",
    );

    // VG-S0007 mutual/self call cycle
    let h_a: Hash = [0x11; 32];
    let h_b: Hash = [0x22; 32];
    let mut node_a = base_node();
    node_a.params = vec![Type::UInt(32)];
    node_a.results = vec![Type::UInt(32)];
    node_a.calls = vec![h_b];
    node_a.ops = vec![Op {
        opcode: op_code("CALL_HASH"),
        mode: 0,
        operands: vec![Operand::Callee(0), Operand::Input(0)],
        results: vec![Type::UInt(32)],
    }];
    node_a.outputs = vec![Operand::Op(0, 0)];

    let mut node_b = node_a.clone();
    node_b.calls = vec![h_a];

    let mut map = BTreeMap::new();
    map.insert(h_a, node_a.clone());
    map.insert(h_b, node_b);
    assert_has_code(&node_a, &|h| map.get(h).cloned(), "VG-S0007");
}

#[test]
fn forged_results_cast_learnable_and_effect_bits() {
    // VG-T0115 forged op.results type on ADD
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(64)];
    n.ops = vec![Op {
        opcode: op_code("ADD"),
        mode: 1,
        operands: vec![Operand::Input(0), Operand::Input(0)],
        results: vec![Type::UInt(64)],
    }];
    n.outputs = vec![Operand::Op(0, 0)];
    assert_has_code(&n, &no_callees, "VG-T0115");

    // VG-T0115 OK with non-Result annotation
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.ops = vec![Op {
        opcode: op_code("OK"),
        mode: 0,
        operands: vec![Operand::Input(0)],
        results: vec![Type::UInt(32)],
    }];
    n.outputs = vec![Operand::Op(0, 0)];
    assert_has_code(&n, &no_callees, "VG-T0115");

    // VG-T0108 wrong output count
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32), Type::UInt(32)];
    n.outputs = vec![Operand::Input(0)];
    assert_has_code(&n, &no_callees, "VG-T0108");

    // VG-T0113 int -> Bool and float -> Bool CAST
    for src_ty in [Type::UInt(32), Type::Float(32)] {
        let mut n = base_node();
        n.params = vec![src_ty];
        n.results = vec![Type::Bool];
        n.ops = vec![Op {
            opcode: op_code("CAST"),
            mode: 0,
            operands: vec![Operand::Input(0)],
            results: vec![Type::Bool],
        }];
        n.outputs = vec![Operand::Op(0, 0)];
        assert_has_code(&n, &no_callees, "VG-T0113");
    }

    // VG-T0117 non-float LEARNABLE
    let mut n = base_node();
    n.results = vec![Type::UInt(32)];
    n.consts = vec![u32_const(0), u32_const(10)];
    n.learnable = vec![Learnable {
        ty: Type::UInt(32),
        lo: 0,
        hi: 1,
        loss: None,
    }];
    n.outputs = vec![Operand::Input(0)];
    assert_has_code(&n, &no_callees, "VG-T0117");

    // VG-T0117 NaN and +inf LEARNABLE bounds
    for bad_bits in [0x7FC0_0000u32, 0x7F80_0000u32] {
        let mut n = base_node();
        n.results = vec![Type::Float(32)];
        n.consts = vec![
            f32_const(0.0),
            Const {
                ty: Type::Float(32),
                bytes: bad_bits.to_le_bytes().to_vec(),
            },
        ];
        n.learnable = vec![Learnable {
            ty: Type::Float(32),
            lo: 0,
            hi: 1,
            loss: None,
        }];
        n.outputs = vec![Operand::Input(0)];
        assert_has_code(&n, &no_callees, "VG-T0117");
    }

    // VG-T0119 Const of Handle or Eff, and non-first Eff param in pure node
    let mut n = base_node();
    n.results = vec![Type::Bool];
    n.consts = vec![Const {
        ty: Type::Handle(Handle::Clock),
        bytes: vec![0, 0, 0, 0],
    }];
    n.outputs = vec![Operand::Const(0)];
    assert_has_code(&n, &no_callees, "VG-T0119");

    // VG-E0105 reserved effect bit set
    let mut n = base_node();
    n.effects = EffectMask(0x1000);
    n.params = vec![Type::Eff];
    n.results = vec![Type::Eff];
    n.outputs = vec![Operand::Input(0)];
    assert_has_code(&n, &no_callees, "VG-E0105");
}

#[test]
fn contract_formula_rejections_on_hand_built_nodes() {
    // VG-C0104 Term::Result in PRE
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.outputs = vec![Operand::Input(0)];
    n.contract.pre = vec![Term::Bin(
        Bin::Eq,
        Box::new(Term::Result(0)),
        Box::new(Term::Input(0)),
    )];
    assert_has_code(&n, &no_callees, "VG-C0104");

    // VG-C0104 Term::Result(j) out of range in POST
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.outputs = vec![Operand::Input(0)];
    n.contract.post = vec![Term::Bin(
        Bin::Eq,
        Box::new(Term::Result(1)),
        Box::new(Term::Input(0)),
    )];
    assert_has_code(&n, &no_callees, "VG-C0104");

    // VG-C0105 Term::Port in POST
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.ops = vec![Op {
        opcode: op_code("NOT"),
        mode: 0,
        operands: vec![Operand::Input(0)],
        results: vec![Type::UInt(32)],
    }];
    n.outputs = vec![Operand::Op(0, 0)];
    n.contract.post = vec![Term::Bin(
        Bin::Eq,
        Box::new(Term::Port(0, 0)),
        Box::new(Term::Result(0)),
    )];
    assert_has_code(&n, &no_callees, "VG-C0105");

    // VG-C0106 Term::Resource in PRE
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.consts = vec![Const {
        ty: Type::Int,
        bytes: vec![10],
    }];
    n.outputs = vec![Operand::Input(0)];
    n.contract.pre = vec![Term::Bin(
        Bin::Le,
        Box::new(Term::Resource(Resource::Ops)),
        Box::new(Term::Const(0)),
    )];
    assert_has_code(&n, &no_callees, "VG-C0106");

    // VG-C0106 LatencyP99Us in COST
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.consts = vec![Const {
        ty: Type::Int,
        bytes: vec![10],
    }];
    n.outputs = vec![Operand::Input(0)];
    n.contract.cost = vec![Term::Bin(
        Bin::Le,
        Box::new(Term::Resource(Resource::LatencyP99Us)),
        Box::new(Term::Const(0)),
    )];
    assert_has_code(&n, &no_callees, "VG-C0106");

    // VG-C0107 Term::Spec pointing to non-SPEC node
    let mut non_spec = base_node();
    non_spec.params = vec![Type::UInt(32)];
    non_spec.results = vec![Type::UInt(32)];
    non_spec.outputs = vec![Operand::Input(0)];
    let non_spec_hash = vg_core::canon::hashes(&non_spec).unwrap().node;
    let mut n = base_node();
    n.params = vec![Type::UInt(32)];
    n.results = vec![Type::UInt(32)];
    n.calls = vec![non_spec_hash];
    n.outputs = vec![Operand::Input(0)];
    n.contract.post = vec![Term::Bin(
        Bin::Eq,
        Box::new(Term::Result(0)),
        Box::new(Term::Spec(0, vec![Term::Input(0)])),
    )];
    assert_has_code(
        &n,
        &|h| (*h == non_spec_hash).then(|| non_spec.clone()),
        "VG-C0107",
    );

    // VG-C0101 formula type mismatch (integer term meets float term)
    let mut n = base_node();
    n.params = vec![Type::UInt(32), Type::Float(32)];
    n.results = vec![Type::UInt(32)];
    n.outputs = vec![Operand::Input(0)];
    n.contract.pre = vec![Term::Bin(
        Bin::Eq,
        Box::new(Term::Input(0)),
        Box::new(Term::Input(1)),
    )];
    assert_has_code(&n, &no_callees, "VG-C0101");

    // VG-C0103 builtin wrong arity
    let mut n = base_node();
    n.params = vec![Type::Float(32)];
    n.results = vec![Type::Float(32)];
    n.outputs = vec![Operand::Input(0)];
    n.contract.pre = vec![Term::Builtin(Builtin::IsFinite, vec![])];
    assert_has_code(&n, &no_callees, "VG-C0103");
}

#[test]
fn resource_accounting_ops_and_arena_bytes() {
    // 0-op node
    let mut n0 = base_node();
    n0.params = vec![Type::UInt(32)];
    n0.results = vec![Type::UInt(32)];
    n0.outputs = vec![Operand::Input(0)];
    let s0 = check_node(&n0, &no_callees).unwrap();
    assert_eq!(s0.cost_ops, 0);
    assert_eq!(s0.ops_expr, CostExpr::Const(0));
    assert_eq!(s0.arena_bytes, CostExpr::Const(0));
    assert_eq!(s0.heap_bytes, 0);

    // Combinators: GENERATE literal vs dynamic, FOLD_BOUNDED over Array, ASSERT_GUARD max
    let src = r#"
module costs;

NODE @inc {
  SIGNATURE: (x: u64) -> (u32);
  EFFECTS: [];
  CONTRACT { }
  BODY {
    %0 = CAST.u32(x);
    %1 = ADD.wrap(%0, 1:u32);
    RETURN(%1);
  }
}

NODE @step {
  SIGNATURE: (acc: u32, x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT { }
  BODY {
    %0 = ADD.wrap(acc, x);
    RETURN(%0);
  }
}

NODE @b_fast {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT { }
  BODY {
    RETURN(x);
  }
}

NODE @b_slow {
  SIGNATURE: (x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT { }
  BODY {
    %0 = ADD.wrap(x, 1:u32);
    %1 = MUL.wrap(%0, 2:u32);
    %2 = XOR(%1, 3:u32);
    RETURN(%2);
  }
}

NODE @static_costs {
  SIGNATURE: (c: Bool, x: u32) -> (Array<u32, 4>, u32, u32);
  EFFECTS: [];
  CONTRACT {
    INVARIANT @inv(k, acc): acc >= 0;
  }
  BODY {
    %0 = GENERATE(@inc, 4:u64);
    %1 = FOLD_BOUNDED(@step, %0, init=x, invariant=@inv);
    %2 = ASSERT_GUARD(c, @b_fast, %1, else=@b_slow);
    RETURN(%0, %1, %2);
  }
}

NODE @dynamic_costs {
  SIGNATURE: (n: u64, xs: Slice<u32>) -> (Slice<u32>, Slice<u32>);
  EFFECTS: [ARENA];
  CONTRACT { }
  BODY {
    %0 = GENERATE(@inc, n);
    %1 = CONCAT(xs, %0);
    // %1 has one use and is freshly allocated -> in-place UPDATE costs 0 extra arena bytes!
    %2 = UPDATE(%1, 0:u64, 7:u32);
    // Updating input slice `xs` MUST copy -> allocates align16(len(xs) * 4)!
    %3 = UPDATE(xs, 0:u64, 9:u32);
    RETURN(%2, %3);
  }
}
"#;
    let summaries = check_source(src).unwrap();
    let static_s = &summaries
        .iter()
        .find(|(l, _)| l == "static_costs")
        .unwrap()
        .1;
    // %0 = GENERATE(@inc, 4) -> 1 + 4*2 = 9
    // %1 = FOLD_BOUNDED(@step, %0, ...) -> 1 + 4*1 = 5
    // %2 = ASSERT_GUARD(c, @b_fast (0), ..., else=@b_slow (3)) -> 1 + max(0, 3) = 4
    // total = 9 + 5 + 4 = 18
    assert_eq!(static_s.cost_ops, 18);
    assert_eq!(static_s.ops_expr, CostExpr::Const(18));
    assert_eq!(static_s.arena_bytes, CostExpr::Const(0));

    let dyn_s = &summaries
        .iter()
        .find(|(l, _)| l == "dynamic_costs")
        .unwrap()
        .1;
    // Lower bound: %0 (1 + n*2 -> 1), %1 (1), %2 (1), %3 (1) -> 4
    assert_eq!(dyn_s.cost_ops, 4);
    // %0 allocates align16(Val(n) * 4), %1 allocates align16((Len(xs) + Val(n)) * 4),
    // %2 updates %1 in-place (0 bytes!), %3 copies xs (align16(Len(xs) * 4)).
    assert_eq!(
        dyn_s.arena_bytes,
        CostExpr::add(
            CostExpr::add(
                CostExpr::align16(CostExpr::mul(
                    CostExpr::Val(Operand::Input(0)),
                    CostExpr::Const(4)
                )),
                CostExpr::align16(CostExpr::mul(
                    CostExpr::add(
                        CostExpr::Len(Operand::Input(1)),
                        CostExpr::Val(Operand::Input(0))
                    ),
                    CostExpr::Const(4)
                )),
            ),
            CostExpr::align16(CostExpr::mul(
                CostExpr::Len(Operand::Input(1)),
                CostExpr::Const(4)
            )),
        )
    );

    // FILE_READ literal max 17 -> align16(17) = 32 bytes.
    let file_src = r#"
module file_cost;

NODE @read17 {
  SIGNATURE: (e: Eff, fin: FileIn) -> (Eff, Result<Slice<u8>, u32>);
  EFFECTS: [ARENA, FILE_READ];
  CONTRACT { }
  BODY {
    %0, %1 = FILE_READ(e, fin, 17:u64);
    RETURN(%0, %1);
  }
}
"#;
    let fs = check_source(file_src).unwrap();
    assert_eq!(fs[0].1.cost_ops, 1);
    assert_eq!(fs[0].1.arena_bytes, CostExpr::Const(32));

    // Multi-use arena slice: UPDATE must copy and add another allocation.
    let multi_use_src = r#"
module multi_use;

NODE @copy_update {
  SIGNATURE: (a: Slice<u32>, b: Slice<u32>) -> (Slice<u32>, Slice<u32>);
  EFFECTS: [ARENA];
  CONTRACT { }
  BODY {
    %0 = CONCAT(a, b);
    %1 = UPDATE(%0, 0:u64, 5:u32);
    RETURN(%0, %1);
  }
}
"#;
    let ms = check_source(multi_use_src).unwrap();
    // Both %0 (CONCAT) and %1 (copying UPDATE because %0 has 2 uses) allocate!
    let sum_len = CostExpr::add(
        CostExpr::Len(Operand::Input(0)),
        CostExpr::Len(Operand::Input(1)),
    );
    let concat_bytes = CostExpr::align16(CostExpr::mul(sum_len.clone(), CostExpr::Const(4)));
    let update_bytes = CostExpr::align16(CostExpr::mul(sum_len, CostExpr::Const(4)));
    assert_eq!(
        ms[0].1.arena_bytes,
        CostExpr::add(concat_bytes, update_bytes)
    );
}

#[test]
fn handles_cannot_be_forged_via_select_or_return_mismatch() {
    // Hand-built node trying to pass a Handle through SELECT or return unreached handle.
    let mut n = base_node();
    n.effects = EffectMask(0x0002); // CLOCK
    n.params = vec![
        Type::Eff,
        Type::Bool,
        Type::Handle(Handle::Clock),
        Type::Handle(Handle::Clock),
    ];
    n.results = vec![Type::Eff, Type::UInt(64)];
    n.ops = vec![
        Op {
            opcode: op_code("SELECT"),
            mode: 0,
            operands: vec![Operand::Input(1), Operand::Input(2), Operand::Input(3)],
            results: vec![Type::Handle(Handle::Clock)],
        },
        Op {
            opcode: op_code("CLOCK_READ"),
            mode: 0,
            operands: vec![Operand::Input(0), Operand::Op(0, 0)],
            results: vec![Type::Eff, Type::UInt(64)],
        },
    ];
    n.outputs = vec![Operand::Op(1, 0), Operand::Op(1, 1)];
    assert_has_code(&n, &no_callees, "VG-T0119");
}

#[test]
fn generate_static_count_limit_and_invariant_cycle() {
    // VG-T0118 GENERATE with literal count > 2^32 - 1
    let mut kern = base_node();
    kern.params = vec![Type::UInt(64)];
    kern.results = vec![Type::UInt(8)];
    kern.ops = vec![Op {
        opcode: op_code("CAST"),
        mode: 0,
        operands: vec![Operand::Input(0)],
        results: vec![Type::UInt(8)],
    }];
    kern.outputs = vec![Operand::Op(0, 0)];
    let kern_hash = vg_core::canon::hashes(&kern).unwrap().node;

    let mut n = base_node();
    n.results = vec![Type::Slice(Box::new(Type::UInt(8)))];
    n.consts = vec![u64_const(1u64 << 32)];
    n.calls = vec![kern_hash];
    n.ops = vec![Op {
        opcode: op_code("GENERATE"),
        mode: 0,
        operands: vec![Operand::Callee(0), Operand::Const(0)],
        results: vec![Type::Slice(Box::new(Type::UInt(8)))],
    }];
    n.outputs = vec![Operand::Op(0, 0)];
    assert_has_code(&n, &|h| (*h == kern_hash).then(|| kern.clone()), "VG-T0118");

    // VG-S0007 INVARIANT referencing a port that depends on the FOLD_BOUNDED op itself
    let mut step = base_node();
    step.params = vec![Type::UInt(32), Type::UInt(32)];
    step.results = vec![Type::UInt(32)];
    step.ops = vec![Op {
        opcode: op_code("ADD"),
        mode: 1,
        operands: vec![Operand::Input(0), Operand::Input(1)],
        results: vec![Type::UInt(32)],
    }];
    step.outputs = vec![Operand::Op(0, 0)];
    let step_hash = vg_core::canon::hashes(&step).unwrap().node;

    let mut cyc = base_node();
    cyc.params = vec![Type::Array(Box::new(Type::UInt(32)), 4)];
    cyc.results = vec![Type::UInt(32)];
    cyc.consts = vec![u32_const(0)];
    cyc.calls = vec![step_hash];
    cyc.ops = vec![Op {
        opcode: op_code("FOLD_BOUNDED"),
        mode: 0,
        operands: vec![
            Operand::Callee(0),
            Operand::Input(0),
            Operand::Const(0),
            Operand::Invariant(0),
        ],
        results: vec![Type::UInt(32)],
    }];
    cyc.outputs = vec![Operand::Op(0, 0)];
    cyc.contract.invariants = vec![vec![Term::Bin(
        Bin::Eq,
        Box::new(Term::Bound(0)),
        Box::new(Term::Port(0, 0)),
    )]];
    assert_has_code(
        &cyc,
        &|h| (*h == step_hash).then(|| step.clone()),
        "VG-S0007",
    );
}
