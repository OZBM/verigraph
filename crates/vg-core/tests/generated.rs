//! P1.11 and the last law of docs/16: generated programs assemble and keep the asm laws, and two
//! bodies share a body_hash exactly when they unravel to the same terms, up to the commutative
//! swaps. The reference is deliberately slow and independent of the hashing: a memoized pairwise
//! comparison of the two graphs (bisimulation), never a hash.

mod common;

use std::collections::HashMap;

use common::summary;
use vg_core::ast::{self, ArgVal, Item, Stmt, Value};
use vg_core::generate::{self, Class, Rng};
use vg_core::ir::{Node, Operand};
use vg_core::lower::{Labels, NodeLowering};
use vg_core::opcode::{self, Comm};
use vg_core::{asm_local, canon, codec, parser, printer};

const SEED: u64 = 0x0d0c_2026_1030;

/// The root node lowered but not canonicalized: source order, duplicates and dead ops included.
fn lowered(src: &str) -> Node {
    let a = asm_local(src).unwrap_or_else(|e| panic!("{e}\n{src}"));
    let m = parser::parse(src).unwrap();
    let mut labels: Labels = HashMap::new();
    for (k, item) in m.items.iter().enumerate() {
        let Item::Node(n) = item else { continue };
        let node = &a.iter().find(|x| x.label == n.label).unwrap().node;
        labels.insert(
            n.label.clone(),
            vg_core::lower::Callable {
                kind: vg_core::lower::Kind::Node,
                item: k,
                params: node.params.clone(),
                results: node.results.clone(),
                effects: node.effects,
                hash: Some(a.iter().find(|x| x.label == n.label).unwrap().hashes.node),
                spec: None,
            },
        );
    }
    let k = m.items.len() - 1;
    let Item::Node(root) = &m.items[k] else {
        panic!()
    };
    NodeLowering::new(&labels, k, root).lower().unwrap().node
}

/// Do operand a of node x and operand b of node y unravel to the same term?
struct Bisim<'a> {
    x: &'a Node,
    y: &'a Node,
    memo: HashMap<(Operand, Operand), bool>,
}

impl Bisim<'_> {
    fn eq(&mut self, a: Operand, b: Operand) -> bool {
        if let Some(r) = self.memo.get(&(a, b)) {
            return *r;
        }
        let r = match (a, b) {
            (Operand::Input(i), Operand::Input(j)) => {
                i == j && self.x.input_type(i) == self.y.input_type(j)
            }
            (Operand::Const(i), Operand::Const(j)) => {
                self.x.consts[i as usize] == self.y.consts[j as usize]
            }
            (Operand::Callee(i), Operand::Callee(j)) => {
                self.x.calls[i as usize] == self.y.calls[j as usize]
            }
            (Operand::Op(i, s), Operand::Op(j, t)) => {
                let (p, q) = (&self.x.ops[i as usize], &self.y.ops[j as usize]);
                s == t
                    && p.opcode == q.opcode
                    && p.mode == q.mode
                    && p.results == q.results
                    && p.operands.len() == q.operands.len()
                    && {
                        let n = p.operands.len();
                        let comm = match opcode::by_code(p.opcode).unwrap().comm {
                            Comm::No => 0,
                            Comm::All => n,
                            Comm::FirstTwo => 2,
                        };
                        let rest = (comm..n).all(|k| self.eq(p.operands[k], q.operands[k]));
                        rest && if comm == 2 {
                            (self.eq(p.operands[0], q.operands[0])
                                && self.eq(p.operands[1], q.operands[1]))
                                || (self.eq(p.operands[0], q.operands[1])
                                    && self.eq(p.operands[1], q.operands[0]))
                        } else {
                            (0..comm).all(|k| self.eq(p.operands[k], q.operands[k]))
                        }
                    }
            }
            _ => false,
        };
        self.memo.insert((a, b), r);
        r
    }
}

fn same_terms(x: &Node, y: &Node) -> bool {
    let mut b = Bisim {
        x,
        y,
        memo: HashMap::new(),
    };
    x.outputs.len() == y.outputs.len()
        && x.outputs.iter().zip(&y.outputs).all(|(a, c)| b.eq(*a, *c))
}

fn body_hash(src: &str) -> vg_core::ir::Hash {
    asm_local(src).unwrap().last().unwrap().hashes.body
}

/// A near-miss: one small change that may or may not change the terms.
fn mutate(src: &str, rng: &mut Rng) -> Option<String> {
    let mut m = parser::parse(src).unwrap();
    let k = m.items.len() - 1;
    let Item::Node(n) = &mut m.items[k] else {
        return None;
    };
    let stmts: Vec<usize> = (0..n.body.len() - 1).collect();
    let i = stmts[rng.below(stmts.len())];
    let Stmt::Assign { op, mode, args, .. } = &mut n.body[i] else {
        return None;
    };
    match rng.below(4) {
        0 => {
            let m = mode.as_mut()?;
            m.text = match m.text.as_str() {
                "wrap" => "sat".into(),
                "sat" => "wrap".into(),
                _ => return None,
            };
        }
        1 if args.len() >= 2
            && op.text != "SELECT"
            && op.text != "FCLAMP"
            && op.text != "FCOPYSIGN"
            && op.text != "INDEX" =>
        {
            args.swap(0, 1)
        }
        2 => {
            let swaps = [
                ("ADD", "SUB"),
                ("AND", "OR"),
                ("FADD", "FSUB"),
                ("FMIN", "FMAX"),
                ("XOR", "OR"),
                ("ROTL", "ROTR"),
                ("FABS", "FNEG"),
            ];
            let to = swaps.iter().find_map(|(f, t)| match op.text.as_str() {
                s if s == *f => Some(*t),
                s if s == *t => Some(*f),
                _ => None,
            })?;
            op.text = to.to_string();
        }
        _ => {
            let a = args
                .iter_mut()
                .find(|a| matches!(a.val, ArgVal::Value(Value::Lit(ast::Literal::Num(..), _))))?;
            let ArgVal::Value(Value::Lit(ast::Literal::Num(n, _), _)) = &mut a.val else {
                return None;
            };
            if n.text.contains('.') {
                n.neg = !n.neg;
            } else {
                n.text = if n.text == "0" {
                    "1".into()
                } else {
                    "0".into()
                };
            }
        }
    }
    Some(printer::print(&m))
}

#[test]
fn generated_programs_keep_the_asm_laws() {
    let mut nodes = 0;
    for (class, n) in [(Class::A, 300), (Class::B, 300)] {
        for s in 0..n {
            let src = generate::program(SEED ^ s, class, 5 + (s as usize % 56));
            for a in asm_local(&src).unwrap_or_else(|e| panic!("seed {s}: {e}\n{src}")) {
                assert_eq!(canon::canonicalize(&a.node).unwrap().node, a.node);
                assert_eq!(codec::decode(&a.bytes).unwrap(), a.node);
                nodes += 1;
            }
        }
    }
    summary(
        "generated.txt",
        &format!(
            "600 generated programs (300 class A, 300 class B, 5 to 60 ops), {nodes} nodes, seed {SEED:#x}: assembled, canonical, decoded back\n"
        ),
    );
}

/// Fuzzing found the generator overflowing i128 on the interval of a product of two u64 values
/// (fuzz/artifacts/canon/crash-e91ad9b5…): the input that crashed it.
#[test]
fn class_b_intervals_do_not_overflow() {
    let src = generate::program(0x0123_000d_0000_0000, Class::B, 53);
    asm_local(&src).unwrap();
}

#[test]
fn hashes_follow_unraveling() {
    let mut rng = Rng(SEED);
    let (mut equal, mut different, mut variants) = (0, 0, 0);
    for s in 0..400u64 {
        let src = generate::program(SEED.wrapping_add(s), Class::A, 3 + (s as usize % 28));
        let x = lowered(&src);
        let hx = body_hash(&src);
        // An equivalence variant unravels the same and hashes the same.
        let (v, log) = generate::variant(&parser::parse(&src).unwrap(), rng.next_u64());
        let vsrc = printer::print(&v);
        assert!(
            same_terms(&x, &lowered(&vsrc)),
            "seed {s}: variant {log:?} unravels differently"
        );
        assert_eq!(
            body_hash(&vsrc),
            hx,
            "seed {s}: variant {log:?} hashes differently\n{src}\n{vsrc}"
        );
        variants += 1;
        // A near-miss hashes the same exactly when it unravels the same.
        for _ in 0..4 {
            let Some(msrc) = mutate(&src, &mut rng) else {
                continue;
            };
            let Ok(out) = asm_local(&msrc) else { continue };
            let same = same_terms(&x, &lowered(&msrc));
            let hm = out.last().unwrap().hashes.body;
            assert_eq!(
                hm == hx,
                same,
                "seed {s}: hash equality {} but term equality {same}\n{src}\n{msrc}",
                hm == hx
            );
            if same { equal += 1 } else { different += 1 }
        }
    }
    summary(
        "unravel.txt",
        &format!(
            "400 generated bodies (3 to 30 ops), seed {SEED:#x}: {variants} variants hash equal and unravel equal; near-misses: {different} unravel differently and hash differently, {equal} unravel equally and hash equally\n"
        ),
    );
}
