//! The concrete side of verification, in vg-interp: the seeded pre-screen of docs/05 step 2, which
//! refutes before any solver runs, and ADR 0006's replay, which classifies a solver's model.
//! Neither ever proves an obligation: only UNSAT does.

use std::collections::BTreeMap;

use vg_core::ir::{Bin, Hash, Node, Operand, Term};
use vg_core::types::Type;
use vg_core::value::Value;
use vg_interp::bytes::decode_const;
use vg_interp::{Clause, NoEffectsHost, holds, holds_invariant, run};
use vg_types::CostExpr;

use crate::Verdict;
use crate::sort::fp_format;

/// Inputs whose static op count exceeds this are not pre-screened: the pre-screen is a fast filter,
/// and a pass proves nothing, so skipping costs only speed.
const PRESCREEN_OPS: u128 = 20_000;
const RANDOM_INPUTS: usize = 256;
const SLICE_LENGTHS: [usize; 5] = [0, 1, 2, 3, 16];

/// ADR 0006 on a model's inputs. Concrete replay runs them with the real callees: a trap or a
/// broken POST reproduces, which is a counterexample. Model-guided replay needs the model's values
/// for abstracted terms, which this replay does not take; it coincides with concrete replay when
/// the node has none, so there a model that does not reproduce is a verifier bug.
pub(crate) fn classify(
    node: &Node,
    id: &str,
    inputs: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> (Verdict, &'static str, String) {
    // A well-definedness obligation (ADR 0010) reproduces when its formula is undefined there.
    let pre_wd = id.contains("/PRE.") && id.contains(".wd.");
    if !node.effects.is_pure() || has_effect_types(node) {
        return (
            Verdict::Unknown,
            "VG-V0007",
            "the model cannot be replayed: vg-interp replays only nodes without effects".into(),
        );
    }
    match holds(node, Clause::Pre, inputs, &[], callee) {
        Ok(true) => {}
        Err(t) if pre_wd => {
            return (
                Verdict::Counterexample,
                "VG-V0001",
                format!(
                    "reproduced in vg-interp: PRE is not defined here: {}",
                    t.message
                ),
            );
        }
        Ok(false) | Err(_) if partly_instantiated(&node.contract.pre) => {
            return (
                Verdict::Unknown,
                "VG-V0006",
                "the model breaks PRE, which the query instantiates only in part (a forall, \
                 spec:, fold: or formula division): split the obligation or guard the op"
                    .into(),
            );
        }
        Ok(false) | Err(_) => {
            return (
                Verdict::Error,
                "VG-V0008",
                "the model breaks PRE, which the query encodes in full: a verifier bug".into(),
            );
        }
    }
    if let Some(m) = fold_base_fails(node, id, inputs, callee) {
        return (Verdict::Counterexample, "VG-V0001", m);
    }
    let reproduced = match run(node, inputs, callee, &mut NoEffectsHost) {
        Err(trap) => Some(format!("reproduced in vg-interp: {}", trap.message)),
        Ok(results) => match post_wd_conjunct(id) {
            Some(j) => {
                let mut one = node.clone();
                one.contract.post = vec![node.contract.post[j].clone()];
                holds(&one, Clause::Post, inputs, &results, callee)
                    .err()
                    .map(|t| {
                        format!(
                            "reproduced in vg-interp: POST.{j} is not defined here: {}",
                            t.message
                        )
                    })
            }
            None => (holds(node, Clause::Post, inputs, &results, callee) == Ok(false))
                .then(|| "reproduced in vg-interp: POST does not hold".to_string()),
        },
    };
    match reproduced {
        Some(m) => (Verdict::Counterexample, "VG-V0001", m),
        None if abstracts(node) => (
            Verdict::ContractGap,
            "VG-V0002",
            "refuted only through values a callee's or kernel's contract, an invariant or a spec \
             allows; these inputs run correctly"
                .into(),
        ),
        None => (
            Verdict::Error,
            "VG-V0008",
            "the query encodes this node exactly, yet its model does not reproduce: a verifier \
             bug"
            .into(),
        ),
    }
}

/// j for `@n/POST.j.wd.w`.
fn post_wd_conjunct(id: &str) -> Option<usize> {
    let rest = id.split("/POST.").nth(1)?;
    rest.split_once(".wd.")?.0.parse().ok()
}

/// For `@n/%i/FOLD.base`: the INVARIANT is false at step 0 on the fold's real initial value,
/// when that value is a constant or an input (vg-interp does not check invariants as it runs).
fn fold_base_fails(
    node: &Node,
    id: &str,
    inputs: &[Value],
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Option<String> {
    let i: usize = id
        .strip_suffix("/FOLD.base")?
        .rsplit('/')
        .next()?
        .strip_prefix('%')?
        .parse()
        .ok()?;
    let ops = &node.ops.get(i)?.operands;
    let (Operand::Invariant(v), init) = (*ops.last()?, ops.get(ops.len().checked_sub(2)?)?) else {
        return None;
    };
    let init = match *init {
        Operand::Const(c) => decode_const(node.consts.get(c as usize)?).ok()?,
        Operand::Input(j) => inputs.get(j as usize)?.clone(),
        _ => return None,
    };
    (holds_invariant(node, v, 0, &init, inputs, &[], callee) == Ok(false))
        .then(|| "reproduced in vg-interp: the INVARIANT is false before the first step".into())
}

fn has_effect_types(node: &Node) -> bool {
    (0..node.input_count() as u32)
        .filter_map(|i| node.input_type(i))
        .any(|t| matches!(t, Type::Eff | Type::Handle(_)))
}

/// Whether the query abstracts part of the node: a callee or kernel known by its contract, a
/// FOLD known by its invariant, or a contract term instantiated in part.
fn abstracts(node: &Node) -> bool {
    !node.calls.is_empty()
        || [&node.contract.pre, &node.contract.post]
            .into_iter()
            .chain(&node.contract.invariants)
            .any(|ts| partly_instantiated(ts))
}

fn partly_instantiated(terms: &[Term]) -> bool {
    fn walk(t: &Term) -> bool {
        match t {
            Term::Forall(..) | Term::Spec(..) | Term::Fold(..) | Term::Apply(..) => true,
            Term::Bin(Bin::Div | Bin::Mod, ..) => true,
            Term::Not(a) | Term::Neg(a) | Term::Proj(a, _) => walk(a),
            Term::Bin(_, a, b) => walk(a) || walk(b),
            Term::Index(a, xs) => walk(a) || xs.iter().any(walk),
            Term::Builtin(_, xs) => xs.iter().any(walk),
            _ => false,
        }
    }
    terms.iter().any(walk)
}

/// A pre-screen refutation: real inputs that satisfy PRE and break obligation `id`.
#[derive(Clone, Debug)]
pub(crate) struct Finding {
    pub inputs: Vec<Value>,
    pub message: String,
}

#[derive(Default)]
pub(crate) struct Prescreen {
    /// The first refuting input per obligation ID, in the deterministic order inputs are tried.
    pub findings: BTreeMap<String, Finding>,
    /// An input that satisfies PRE and lies in the LEARNABLE box: the contract is not vacuous.
    pub pre_met: Option<Vec<Value>>,
}

/// docs/05 step 2: boundary values, first one parameter at a time with the others at a base
/// value, then 256 random combinations seeded with BLAKE3 over the node_hash, so every machine
/// tries the same inputs. Each input that meets PRE runs in vg-interp; a trap refutes the op's
/// obligation of that kind, and a false POST conjunct refutes it. Skips nodes with effects.
///
/// shortcut: FOLD_BOUNDED invariants are not checked step by step here (the solver checks them);
/// add it when vg-interp exposes a fold's accumulators.
pub(crate) fn prescreen(
    node: &Node,
    label: &str,
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Prescreen {
    let mut out = Prescreen::default();
    let n = node.input_count();
    if !node.effects.is_pure() || has_effect_types(node) {
        return out;
    }
    let (Ok(hashes), Ok(summary)) = (
        vg_core::canon::hashes(node),
        vg_types::check_node(node, callee),
    ) else {
        return out;
    };
    let lits = literals(node);
    let cands: Vec<Vec<Value>> = (0..n as u32)
        .map(|i| {
            let ty = node.input_type(i).expect("input in range");
            match node
                .learnable
                .get((i as usize).wrapping_sub(node.params.len()))
            {
                Some(l) if i as usize >= node.params.len() => [l.lo, l.hi]
                    .iter()
                    .filter_map(|&c| decode_const(&node.consts[c as usize]).ok())
                    .collect(),
                _ => candidates(ty, &lits),
            }
        })
        .collect();
    if cands.iter().any(Vec::is_empty) {
        return out;
    }
    let base: Vec<Value> = cands.iter().map(|c| c[0].clone()).collect();
    let mut tries = vec![base.clone()];
    for (i, cs) in cands.iter().enumerate() {
        for c in &cs[1..] {
            let mut t = base.clone();
            t[i] = c.clone();
            tries.push(t);
        }
    }
    let mut rng = SplitMix(u64::from_le_bytes(
        blake3::derive_key("VeriGraph 2026 prescreen v1", &hashes.node)[..8]
            .try_into()
            .expect("8 bytes"),
    ));
    for _ in 0..RANDOM_INPUTS {
        tries.push(
            (0..n as u32)
                .map(|i| {
                    let ty = node.input_type(i).expect("input in range");
                    if i as usize >= node.params.len() {
                        cands[i as usize][rng.below(cands[i as usize].len())].clone()
                    } else {
                        random_value(ty, &lits, &mut rng)
                    }
                })
                .collect(),
        );
    }
    let post_ids = (0..node.contract.post.len()).map(|j| format!("@{label}/POST.{j}"));
    let post_ids: Vec<String> = post_ids.collect();
    for inputs in tries {
        if static_ops(&summary.ops_expr, &inputs).is_none_or(|ops| ops > PRESCREEN_OPS) {
            continue;
        }
        if holds(node, Clause::Pre, &inputs, &[], callee) != Ok(true) {
            continue;
        }
        if out.pre_met.is_none() {
            out.pre_met = Some(inputs.clone());
        }
        match run(node, &inputs, callee, &mut NoEffectsHost) {
            Err(trap) => {
                let Some(i) = trap.op else { continue };
                let kind = trap
                    .message
                    .split(['(', ':', ' '])
                    .next()
                    .unwrap_or_default();
                // The trap must name an obligation of this op, not of one inside a callee.
                let op_name = vg_core::opcode::by_code(node.ops[i as usize].opcode)
                    .map(|o| o.name)
                    .unwrap_or_default();
                if kind.split('.').next() == Some(op_name) && kind.contains('.') {
                    out.findings
                        .entry(format!("@{label}/%{i}/{kind}"))
                        .or_insert(Finding {
                            inputs,
                            message: format!("pre-screen: {}", trap.message),
                        });
                }
            }
            Ok(results) => {
                for (j, id) in post_ids.iter().enumerate() {
                    if out.findings.contains_key(id) {
                        continue;
                    }
                    let mut one = node.clone();
                    one.contract.post = vec![node.contract.post[j].clone()];
                    if holds(&one, Clause::Post, &inputs, &results, callee) == Ok(false) {
                        out.findings.insert(
                            id.clone(),
                            Finding {
                                inputs: inputs.clone(),
                                message: format!("pre-screen: POST.{j} does not hold"),
                            },
                        );
                    }
                }
            }
        }
    }
    out
}

/// The static op count of the node on these inputs, when every term of the cost is known from
/// the inputs alone.
fn static_ops(c: &CostExpr, inputs: &[Value]) -> Option<u128> {
    let input = |o: &Operand| match o {
        Operand::Input(i) => inputs.get(*i as usize),
        _ => None,
    };
    Some(match c {
        CostExpr::Const(k) => u128::from(*k),
        CostExpr::Len(o) => match input(o)? {
            Value::Seq(xs) => xs.len() as u128,
            _ => return None,
        },
        CostExpr::Val(o) => match input(o)? {
            Value::Int(v) => u128::try_from(*v).ok()?,
            _ => return None,
        },
        CostExpr::CalleeOps(_) | CostExpr::CalleeArenaBytes(_) => return None,
        CostExpr::Add(a, b) => static_ops(a, inputs)?.checked_add(static_ops(b, inputs)?)?,
        CostExpr::Mul(a, b) => static_ops(a, inputs)?.checked_mul(static_ops(b, inputs)?)?,
        CostExpr::Max(a, b) => static_ops(a, inputs)?.max(static_ops(b, inputs)?),
        CostExpr::Align16(a) => static_ops(a, inputs)?.checked_add(15)? & !15,
    })
}

/// Every literal of PRE, with its type: the boundaries the contract itself names.
fn literals(node: &Node) -> Vec<(Type, Value)> {
    fn walk(t: &Term, node: &Node, out: &mut Vec<(Type, Value)>) {
        match t {
            Term::Const(c) => {
                let c = &node.consts[*c as usize];
                if let Ok(v) = decode_const(c) {
                    out.push((c.ty.clone(), v));
                }
            }
            Term::Not(a) | Term::Neg(a) | Term::Proj(a, _) => walk(a, node, out),
            Term::Bin(_, a, b) => {
                walk(a, node, out);
                walk(b, node, out);
            }
            Term::Index(a, xs) => {
                walk(a, node, out);
                xs.iter().for_each(|x| walk(x, node, out));
            }
            Term::Forall(rs, b) => {
                rs.iter().for_each(|(lo, hi)| {
                    walk(lo, node, out);
                    walk(hi, node, out);
                });
                walk(b, node, out);
            }
            Term::Builtin(_, xs) | Term::Spec(_, xs) | Term::Fold(_, xs) | Term::Apply(_, xs) => {
                xs.iter().for_each(|x| walk(x, node, out))
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    node.contract
        .pre
        .iter()
        .for_each(|t| walk(t, node, &mut out));
    out
}

/// The boundary values of docs/05 for one type, the base value first.
fn candidates(ty: &Type, lits: &[(Type, Value)]) -> Vec<Value> {
    let mut v: Vec<Value> = match ty {
        Type::Bool => vec![Value::Bool(false), Value::Bool(true)],
        Type::UInt(w) => {
            let max = (1i128 << w) - 1;
            vec![0, 1, 2, max - 1, max]
                .into_iter()
                .map(Value::Int)
                .collect()
        }
        Type::SInt(w) => {
            let (min, max) = (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1);
            vec![0, 1, -1, min, min + 1, max - 1, max]
                .into_iter()
                .map(Value::Int)
                .collect()
        }
        Type::Float(_) | Type::BFloat16 => {
            float_boundaries(ty).into_iter().map(Value::Float).collect()
        }
        Type::Array(t, len) => candidates(t, lits)
            .into_iter()
            .map(|c| Value::Seq(vec![c; *len as usize]))
            .collect(),
        Type::Tensor(t, dims) => {
            let len = dims.iter().map(|&d| d as usize).product::<usize>();
            candidates(t, lits)
                .into_iter()
                .map(|c| Value::Seq(vec![c; len]))
                .collect()
        }
        Type::Slice(t) => {
            let elems = candidates(t, lits);
            let mut v = vec![Value::Seq(vec![])];
            for &l in &SLICE_LENGTHS[1..] {
                v.extend(elems.iter().map(|c| Value::Seq(vec![c.clone(); l])));
            }
            v
        }
        Type::Tuple(ts) => {
            let per: Vec<Vec<Value>> = ts.iter().map(|t| candidates(t, lits)).collect();
            if per.iter().any(Vec::is_empty) {
                return vec![];
            }
            let base: Vec<Value> = per.iter().map(|c| c[0].clone()).collect();
            let mut v = vec![Value::Tuple(base.clone())];
            for (i, cs) in per.iter().enumerate() {
                for c in &cs[1..] {
                    let mut t = base.clone();
                    t[i] = c.clone();
                    v.push(Value::Tuple(t));
                }
            }
            v
        }
        Type::Result(a, b) => candidates(a, lits)
            .into_iter()
            .map(|c| Value::Ok(Box::new(c)))
            .chain(
                candidates(b, lits)
                    .into_iter()
                    .map(|c| Value::Err(Box::new(c))),
            )
            .collect(),
        _ => vec![],
    };
    for (lt, lv) in lits.iter().filter(|(lt, _)| lt == ty) {
        match (lt, lv) {
            (Type::UInt(_) | Type::SInt(_), Value::Int(c)) => {
                for d in [c - 1, *c, c + 1] {
                    if vg_core::value::int_fits(d, ty) {
                        v.push(Value::Int(d));
                    }
                }
            }
            _ => v.push(lv.clone()),
        }
    }
    let mut seen = Vec::new();
    v.retain(|x| {
        let fresh = !seen.contains(x);
        if fresh {
            seen.push(x.clone());
        }
        fresh
    });
    v
}

/// 0, −0, ±1, 0.5, the smallest subnormals, the largest finite values, ±∞ and NaN, as IEEE bits.
fn float_boundaries(ty: &Type) -> Vec<u64> {
    let (eb, sb) = fp_format(ty).expect("a float type");
    let frac = sb - 1;
    let sign = 1u64 << (eb + frac);
    let bias = (1u64 << (eb - 1)) - 1;
    let one = bias << frac;
    let half = (bias - 1) << frac;
    let inf = ((1u64 << eb) - 1) << frac;
    let max = inf - 1;
    let nan = vg_core::float::canonical_nan(ty);
    vec![
        0,
        sign,
        one,
        one | sign,
        half,
        1,
        1 | sign,
        max,
        max | sign,
        inf,
        inf | sign,
        nan,
    ]
}

/// A random value of `ty`: each scalar drawn from its boundary values, or uniformly at random.
fn random_value(ty: &Type, lits: &[(Type, Value)], rng: &mut SplitMix) -> Value {
    match ty {
        Type::UInt(w) if rng.below(2) == 0 => {
            Value::Int(i128::from(rng.next() >> (64 - w)) & ((1i128 << w) - 1))
        }
        Type::SInt(w) if rng.below(2) == 0 => {
            let raw = i128::from(rng.next() >> (64 - w));
            Value::Int(if raw >= 1i128 << (w - 1) {
                raw - (1i128 << w)
            } else {
                raw
            })
        }
        Type::Array(t, len) => Value::Seq((0..*len).map(|_| random_value(t, lits, rng)).collect()),
        Type::Tensor(t, dims) => Value::Seq(
            (0..dims.iter().map(|&d| d as usize).product::<usize>())
                .map(|_| random_value(t, lits, rng))
                .collect(),
        ),
        Type::Slice(t) => {
            let len = SLICE_LENGTHS[rng.below(SLICE_LENGTHS.len())];
            Value::Seq((0..len).map(|_| random_value(t, lits, rng)).collect())
        }
        Type::Tuple(ts) => Value::Tuple(ts.iter().map(|t| random_value(t, lits, rng)).collect()),
        Type::Result(a, b) => {
            if rng.below(2) == 0 {
                Value::Ok(Box::new(random_value(a, lits, rng)))
            } else {
                Value::Err(Box::new(random_value(b, lits, rng)))
            }
        }
        _ => {
            let cs = candidates(ty, lits);
            cs[rng.below(cs.len())].clone()
        }
    }
}

/// SplitMix64, seeded from BLAKE3, so the pre-screen's random inputs are the same everywhere.
struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Options, Outcome};

    fn node(src: &str, label: &str) -> (Node, Vec<vg_core::Assembled>) {
        let asm = vg_core::asm_local(src).unwrap();
        let n = asm.iter().find(|a| a.label == label).unwrap().node.clone();
        (n, asm)
    }

    fn verify_all(src: &str, label: &str) -> Vec<Outcome> {
        let (n, asm) = node(src, label);
        let callee = |h: &Hash| {
            asm.iter()
                .find(|a| a.hashes.node == *h)
                .map(|a| a.node.clone())
        };
        crate::verify(&n, label, &callee, &Options::default())
    }

    #[test]
    fn the_prescreen_refutes_the_soft_knee_draft_without_a_solver() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/soft_knee_draft.vgs"),
        )
        .unwrap();
        let (n, asm) = node(&src, "soft_knee");
        let callee = |h: &Hash| {
            asm.iter()
                .find(|a| a.hashes.node == *h)
                .map(|a| a.node.clone())
        };
        let a = prescreen(&n, "soft_knee", &callee);
        let b = prescreen(&n, "soft_knee", &callee);
        let f = a.findings.get("@soft_knee/POST.0").expect("POST.0 refuted");
        assert_eq!(
            f.inputs, b.findings["@soft_knee/POST.0"].inputs,
            "deterministic"
        );
        assert_eq!(
            classify(&n, "@soft_knee/POST.0", &f.inputs, &callee).0,
            Verdict::Counterexample
        );
        let o = verify_all(&src, "soft_knee");
        let post0 = o.iter().find(|o| o.id == "@soft_knee/POST.0").unwrap();
        assert_eq!(
            (post0.verdict, post0.evidence, post0.solver.as_str()),
            (Verdict::Counterexample, Some("prescreen"), "none")
        );
    }

    #[test]
    fn replay_tells_the_four_verdicts_apart() {
        let src = "module t;
NODE @inc {
  SIGNATURE: (x: u8) -> (u8);
  EFFECTS: [];
  CONTRACT { PRE: x < 100; POST: result > x; }
  BODY { %0 = ADD.checked(x, 1:u8); RETURN(%0); }
}
NODE @caller {
  SIGNATURE: (x: u8) -> (u8);
  EFFECTS: [];
  CONTRACT { PRE: x < 100; POST: result == x + 1; }
  BODY { %0 = CALL_HASH(@inc, x); RETURN(%0); }
}
NODE @plain {
  SIGNATURE: (x: u8) -> (u8);
  EFFECTS: [];
  CONTRACT { PRE: x < 100; POST: result == x + 1; }
  BODY { %0 = ADD.checked(x, 1:u8); RETURN(%0); }
}
NODE @all_small {
  SIGNATURE: (xs: Array<u8, 4>) -> (u8);
  EFFECTS: [];
  CONTRACT { PRE: forall(i in 0..4) { xs[i] < 10 }; }
  BODY { %0 = INDEX(xs, 0:u64); RETURN(%0); }
}";
        let asm = vg_core::asm_local(src).unwrap();
        let get = |l: &str| asm.iter().find(|a| a.label == l).unwrap().node.clone();
        let callee = |h: &Hash| {
            asm.iter()
                .find(|a| a.hashes.node == *h)
                .map(|a| a.node.clone())
        };
        let int = |v| vec![Value::Int(v)];
        // A node with nothing abstracted, and a "model" that runs correctly: an encoding bug.
        assert_eq!(
            classify(&get("plain"), "@plain/POST.0", &int(5), &callee).1,
            "VG-V0008"
        );
        // Through a callee whose POST says only result > x, a correct run is a contract gap.
        assert_eq!(
            classify(&get("caller"), "@caller/POST.0", &int(5), &callee).0,
            Verdict::ContractGap
        );
        // A quantified PRE the model breaks: instantiated only in part.
        let xs = vec![Value::Seq(vec![Value::Int(50); 4])];
        assert_eq!(
            classify(
                &get("all_small"),
                "@all_small/%0/INDEX.in_bounds",
                &xs,
                &callee
            )
            .1,
            "VG-V0006"
        );
        // A trap with the real callees reproduces.
        let wrong = "module t;
NODE @w {
  SIGNATURE: (x: u8) -> (u8);
  EFFECTS: [];
  CONTRACT { POST: result == x + 1; }
  BODY { %0 = ADD.checked(x, 1:u8); RETURN(%0); }
}";
        let (w, _) = node(wrong, "w");
        assert_eq!(
            classify(&w, "@w/%0/ADD.no_overflow", &int(255), &callee).1,
            "VG-V0001"
        );
    }

    /// A refutation the pre-screen cannot find goes to the solver, whose model is shrunk to
    /// slices of length 4 or less (ADR 0006) and reproduces.
    #[test]
    fn solver_models_are_shrunk_and_replayed() {
        let src = "module t;
NODE @third {
  SIGNATURE: (s: Slice<u32>) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE: len(s) > 2 && s[0] > 100 && s[0] < 1000 && s[2] == s[0] * 3 + 7;
    POST: result == 0;
  }
  BODY { %0 = INDEX(s, 2:u64); RETURN(%0); }
}";
        let o = verify_all(src, "third");
        let post = o.iter().find(|o| o.id == "@third/POST.0").unwrap();
        assert_eq!(
            (post.verdict, post.evidence),
            (Verdict::Counterexample, Some("re-solved")),
            "{post:?}"
        );
        let Some([Value::Seq(s)]) = post.inputs.as_deref() else {
            panic!("{post:?}")
        };
        assert!((3..=4).contains(&s.len()), "{s:?}");
    }
}
