//! Canonicalization, steps 1 to 7 of docs/02: validate, hash every op bottom-up (Merkle hashing),
//! sort commutative operands, merge equal ops, drop dead ones, number the rest by a post-order walk
//! from the outputs, and order every table so that equal graphs give equal bytes.

use std::collections::{BTreeMap, HashMap};

use crate::diag::{Diagnostic, Span};
use crate::hash::{Context, derive};
use crate::ir::{Const, Contract, Hash, Node, Op, Operand, Term};
use crate::lower::{MAX_TERM_DEPTH, depth};
use crate::opcode::{self, Comm, Shape};
use crate::types::Type;

type R<T> = Result<T, Diagnostic>;

/// Formula tags of the CONTRACT section (ADR 0001).
pub mod ftag {
    pub const INPUT: u16 = 0x01;
    pub const RESULT: u16 = 0x02;
    pub const PORT: u16 = 0x03;
    pub const BOUND: u16 = 0x04;
    pub const CONST: u16 = 0x05;
    pub const RESOURCE: u16 = 0x06;
    pub const NOT: u16 = 0x10;
    pub const NEG: u16 = 0x11;
    /// AND through MOD: 0x20 + the position of the operator in `Bin`.
    pub const BIN: u16 = 0x20;
    pub const INDEX: u16 = 0x30;
    pub const PROJ: u16 = 0x31;
    pub const FORALL: u16 = 0x32;
    pub const BUILTIN: u16 = 0x40;
    pub const SPEC: u16 = 0x41;
    pub const FOLD: u16 = 0x42;
    pub const APPLY: u16 = 0x43;
}

fn s0203(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error("VG-S0203", Span::default(), msg)
}

/// What canonicalization keeps: the canonical node, and the ops of the input it dropped as dead
/// (VG-W0001), by their index in the input.
pub struct Canonical {
    pub node: Node,
    pub dropped: Vec<u32>,
}

/// Structural checks shared by canonicalization and decoding: every index in range, ops referring
/// only to earlier ops, operand kinds and counts as the opcode's shape requires, formulas closed.
pub fn validate(n: &Node) -> R<()> {
    let value = |o: &Operand, before: usize| -> R<()> {
        let ok = match *o {
            Operand::Input(i) => (i as usize) < n.input_count(),
            Operand::Op(i, s) => {
                (i as usize) < before && (s as usize) < n.ops[i as usize].results.len()
            }
            Operand::Const(c) => {
                (c as usize) < n.consts.len() && n.consts[c as usize].ty != Type::Int
            }
            Operand::Callee(_) | Operand::Invariant(_) => false,
        };
        if ok {
            Ok(())
        } else {
            Err(s0203(format!(
                "operand {o:?} out of range or of the wrong kind"
            )))
        }
    };
    for (i, op) in n.ops.iter().enumerate() {
        let info = opcode::by_code(op.opcode).ok_or_else(|| {
            Diagnostic::error(
                "VG-S0101",
                Span::default(),
                format!("unknown opcode {:#06x}", op.opcode),
            )
        })?;
        if !info.mvp || op.opcode == opcode::IDENTITY {
            return Err(Diagnostic::error(
                "VG-S0104",
                Span::default(),
                format!("{} cannot appear in .vgir", info.name),
            ));
        }
        if info.mode_name(op.mode).is_none() {
            return Err(Diagnostic::error(
                "VG-S0102",
                Span::default(),
                format!("{} has no mode byte {}", info.name, op.mode),
            ));
        }
        let k = op.operands.len();
        let callee_at = |j: usize| match info.shape {
            Shape::Kernel | Shape::Fold | Shape::Call => j == 0,
            Shape::Guard => j == 1 || j + 1 == k,
            _ => false,
        };
        let shape_ok = match info.shape {
            Shape::Fixed(m) => k == m as usize,
            Shape::AtLeast(m) => k >= m as usize,
            Shape::Kernel => k >= 2,
            Shape::Fold => k >= 4,
            Shape::Guard => k >= 3,
            Shape::Call => k >= 1,
        };
        if !shape_ok || op.results.is_empty() && !matches!(info.shape, Shape::Call | Shape::Guard) {
            return Err(Diagnostic::error(
                "VG-S0103",
                Span::default(),
                format!("{} has the wrong number of operands or results", info.name),
            ));
        }
        for (j, o) in op.operands.iter().enumerate() {
            match o {
                Operand::Callee(c) if callee_at(j) && (*c as usize) < n.calls.len() => {}
                Operand::Invariant(v)
                    if info.shape == Shape::Fold
                        && j + 1 == k
                        && (*v as usize) < n.contract.invariants.len() => {}
                _ if callee_at(j) || info.shape == Shape::Fold && j + 1 == k => {
                    return Err(s0203(format!(
                        "{} operand {j} must be a callee or an invariant",
                        info.name
                    )));
                }
                _ => value(o, i)?,
            }
        }
    }
    if n.outputs.len() != n.results.len() {
        return Err(s0203("one output per result"));
    }
    for o in &n.outputs {
        value(o, n.ops.len())?;
    }
    check_widths(n)?;
    check_eff_linear(n)?;
    for l in &n.learnable {
        let fits = |c: u32| n.consts.get(c as usize).is_some_and(|c| c.ty == l.ty);
        if !l.ty.is_float()
            || !fits(l.lo)
            || !fits(l.hi)
            || l.loss.is_some_and(|c| c as usize >= n.calls.len())
        {
            return Err(s0203("LEARNABLE entry out of range"));
        }
    }
    let c = &n.contract;
    for (binders, terms) in [
        (0, &c.pre),
        (0, &c.post),
        (0, &c.cost),
        (0, &c.budget),
        (0, &c.decreases),
    ]
    .into_iter()
    .chain(c.invariants.iter().map(|v| (2, v)))
    {
        for t in terms {
            if depth(t) > MAX_TERM_DEPTH {
                return Err(s0203("formula nested too deep"));
            }
            check_term(n, t, binders, binders == 2)?;
        }
    }
    Ok(())
}

/// The widths the format writes in one byte or one u16 (docs/02, static limits): a Tuple has at
/// most 255 members, a Tensor at most 254 dimensions, an op or a node at most 255 results, a formula
/// term at most 65,535 arguments and a clause at most 65,535 conjuncts. Wider values would be
/// truncated, and two different nodes would share bytes and hashes (G1 review B1-B3).
fn check_widths(n: &Node) -> R<()> {
    fn type_ok(t: &Type) -> bool {
        match t {
            Type::Tuple(ms) => ms.len() <= 255 && ms.iter().all(type_ok),
            Type::Tensor(e, dims) => dims.len() <= 254 && type_ok(e),
            Type::Array(e, _) | Type::Slice(e) => type_ok(e),
            Type::Result(a, b) => type_ok(a) && type_ok(b),
            _ => true,
        }
    }
    fn term_ok(t: &Term) -> bool {
        let wide = match t {
            Term::Forall(rs, _) => 2 * rs.len() + 1 > 65_535,
            Term::Index(_, xs) => xs.len() + 1 > 65_535,
            Term::Builtin(_, xs) | Term::Spec(_, xs) | Term::Fold(_, xs) | Term::Apply(_, xs) => {
                xs.len() > 65_535
            }
            _ => false,
        };
        !wide && children(t).into_iter().all(term_ok)
    }
    let types = n
        .params
        .iter()
        .chain(&n.results)
        .chain(n.learnable.iter().map(|l| &l.ty))
        .chain(n.consts.iter().map(|c| &c.ty))
        .chain(n.ops.iter().flat_map(|op| &op.results));
    let c = &n.contract;
    let clauses = [&c.pre, &c.post, &c.cost, &c.budget, &c.decreases]
        .into_iter()
        .chain(&c.invariants);
    let ok = types.into_iter().all(type_ok)
        && n.results.len() <= 255
        && n.ops.iter().all(|op| op.results.len() <= 255)
        && clauses
            .clone()
            .all(|v| v.len() <= 65_535 && v.iter().all(term_ok));
    if ok {
        Ok(())
    } else {
        Err(Diagnostic::error(
            "VG-T0118",
            Span::default(),
            "a type, result list or formula is wider than docs/02's static limits",
        ))
    }
}

/// Each Eff value is used exactly once (VG-E0102), for nodes from any source: lowering checks it with
/// a span, and this check covers decoded and API-built nodes (G1 review B5). It also keeps step 5
/// from merging two effect ops, which would need one Eff value used twice.
fn check_eff_linear(n: &Node) -> R<()> {
    let mut uses: HashMap<Operand, u32> = HashMap::new();
    for o in n.ops.iter().flat_map(|op| &op.operands).chain(&n.outputs) {
        *uses.entry(*o).or_default() += 1;
    }
    let inputs = (0..n.input_count() as u32)
        .filter(|&i| n.input_type(i) == Some(&Type::Eff))
        .map(Operand::Input);
    let results = n.ops.iter().enumerate().flat_map(|(i, op)| {
        op.results
            .iter()
            .enumerate()
            .filter(|(_, t)| **t == Type::Eff)
            .map(move |(s, _)| Operand::Op(i as u32, s as u8))
    });
    for v in inputs.chain(results) {
        let k = uses.get(&v).copied().unwrap_or(0);
        if k != 1 {
            return Err(Diagnostic::error(
                "VG-E0102",
                Span::default(),
                format!("Eff value {v:?} is used {k} times; each Eff is used exactly once"),
            ));
        }
    }
    Ok(())
}

/// `ports`: an INVARIANT, the only clause that may name ports (VG-C0105).
fn check_term(n: &Node, t: &Term, binders: u32, ports: bool) -> R<()> {
    let ok = match t {
        Term::Input(i) => (*i as usize) < n.input_count(),
        Term::Result(j) => (*j as usize) < n.results.len(),
        Term::Port(i, s) => {
            ports
                && *i < 1 << 24
                && n.ops
                    .get(*i as usize)
                    .is_some_and(|op| (*s as usize) < op.results.len())
        }
        Term::Bound(d) => *d < binders,
        Term::Const(c) => (*c as usize) < n.consts.len(),
        Term::Spec(c, _) | Term::Fold(c, _) | Term::Apply(c, _) => (*c as usize) < n.calls.len(),
        _ => true,
    };
    if !ok {
        return Err(s0203(format!("formula term {t:?} out of range")));
    }
    match t {
        Term::Forall(rs, b) => {
            for (k, (lo, hi)) in rs.iter().enumerate() {
                check_term(n, lo, binders + k as u32, ports)?;
                check_term(n, hi, binders + k as u32, ports)?;
            }
            check_term(n, b, binders + rs.len() as u32, ports)
        }
        _ => children(t)
            .into_iter()
            .try_for_each(|c| check_term(n, c, binders, ports)),
    }
}

/// A term's sub-terms, in pre-order position.
pub fn children(t: &Term) -> Vec<&Term> {
    match t {
        Term::Not(a) | Term::Neg(a) | Term::Proj(a, _) => vec![a],
        Term::Bin(_, a, b) => vec![a, b],
        Term::Index(a, v) => std::iter::once(&**a).chain(v).collect(),
        Term::Forall(rs, b) => rs
            .iter()
            .flat_map(|(l, h)| [l, h])
            .chain(std::iter::once(&**b))
            .collect(),
        Term::Builtin(_, v) | Term::Spec(_, v) | Term::Fold(_, v) | Term::Apply(_, v) => {
            v.iter().collect()
        }
        _ => vec![],
    }
}

fn children_mut(t: &mut Term) -> Vec<&mut Term> {
    match t {
        Term::Not(a) | Term::Neg(a) | Term::Proj(a, _) => vec![a],
        Term::Bin(_, a, b) => vec![a, b],
        Term::Index(a, v) => std::iter::once(&mut **a).chain(v.iter_mut()).collect(),
        Term::Forall(rs, b) => rs
            .iter_mut()
            .flat_map(|(l, h)| [l, h])
            .chain(std::iter::once(&mut **b))
            .collect(),
        Term::Builtin(_, v) | Term::Spec(_, v) | Term::Fold(_, v) | Term::Apply(_, v) => {
            v.iter_mut().collect()
        }
        _ => vec![],
    }
}

/// (tag, argc, payload) of a term's CONTRACT node; the payload of PORT, CONST and the calls is
/// the table index, replaced by a hash when hashing.
pub fn term_head(t: &Term) -> (u16, u16, u32) {
    let n = |v: &Vec<Term>| v.len() as u16;
    match t {
        Term::Input(i) => (ftag::INPUT, 0, *i),
        Term::Result(j) => (ftag::RESULT, 0, *j),
        Term::Port(i, s) => (ftag::PORT, 0, *i << 8 | u32::from(*s)),
        Term::Bound(d) => (ftag::BOUND, 0, *d),
        Term::Const(c) => (ftag::CONST, 0, *c),
        Term::Resource(r) => (ftag::RESOURCE, 0, *r as u32),
        Term::Not(_) => (ftag::NOT, 1, 0),
        Term::Neg(_) => (ftag::NEG, 1, 0),
        Term::Bin(b, ..) => (ftag::BIN + *b as u16, 2, 0),
        Term::Index(_, v) => (ftag::INDEX, 1 + n(v), 0),
        Term::Proj(_, j) => (ftag::PROJ, 1, *j),
        Term::Forall(rs, _) => (ftag::FORALL, 2 * rs.len() as u16 + 1, rs.len() as u32),
        Term::Builtin(b, v) => (ftag::BUILTIN, n(v), *b as u32),
        Term::Spec(c, v) => (ftag::SPEC, n(v), *c),
        Term::Fold(c, v) => (ftag::FOLD, n(v), *c),
        Term::Apply(c, v) => (ftag::APPLY, n(v), *c),
    }
}

struct Hasher<'a> {
    n: &'a Node,
    op_hash: Vec<Option<Hash>>,
    inv_hash: HashMap<u32, Hash>,
}

impl Hasher<'_> {
    fn input_hash(&self, i: u32) -> Hash {
        let mut b = vec![0x00];
        b.extend(i.to_le_bytes());
        self.n
            .input_type(i)
            .expect("validated")
            .structural_bytes(&mut b);
        derive(Context::Op, &b)
    }

    fn result_hash(&self, i: u32, s: u8) -> Hash {
        let mut b = vec![0x01];
        b.extend(self.op_hash[i as usize].expect("dependencies are hashed first"));
        b.push(s);
        derive(Context::Op, &b)
    }

    fn const_bytes(c: &Const, out: &mut Vec<u8>) {
        c.ty.structural_bytes(out);
        out.extend((c.bytes.len() as u32).to_le_bytes());
        out.extend(&c.bytes);
    }

    fn operand_hash(&mut self, o: Operand) -> Hash {
        match o {
            Operand::Input(i) => self.input_hash(i),
            Operand::Op(i, s) => self.result_hash(i, s),
            Operand::Const(c) => {
                let mut b = vec![0x02];
                Self::const_bytes(&self.n.consts[c as usize], &mut b);
                derive(Context::Op, &b)
            }
            Operand::Callee(c) => {
                let mut b = vec![0x03];
                b.extend(self.n.calls[c as usize]);
                derive(Context::Op, &b)
            }
            Operand::Invariant(v) => {
                let mut b = vec![0x04];
                b.extend(self.invariant_hash(v));
                derive(Context::Op, &b)
            }
        }
    }

    /// The hash of an INVARIANT clause's canonical formula, with ports, constants and callees
    /// replaced by their hashes, so it never depends on numbering (ADR 0007).
    fn invariant_hash(&mut self, v: u32) -> Hash {
        if let Some(h) = self.inv_hash.get(&v) {
            return *h;
        }
        let mut b = vec![0x49];
        let conj = &self.n.contract.invariants[v as usize];
        b.extend((conj.len() as u32).to_le_bytes());
        for t in conj {
            self.term_bytes(t, &mut b);
        }
        let h = derive(Context::Contract, &b);
        self.inv_hash.insert(v, h);
        h
    }

    fn term_bytes(&self, t: &Term, out: &mut Vec<u8>) {
        let (tag, argc, payload) = term_head(t);
        out.extend(tag.to_le_bytes());
        out.extend(argc.to_le_bytes());
        match t {
            Term::Port(i, s) => out.extend(self.result_hash(*i, *s)),
            Term::Const(c) => Self::const_bytes(&self.n.consts[*c as usize], out),
            Term::Spec(c, _) | Term::Fold(c, _) | Term::Apply(c, _) => {
                out.extend(self.n.calls[*c as usize])
            }
            _ => out.extend(payload.to_le_bytes()),
        }
        for c in children(t) {
            self.term_bytes(c, out);
        }
    }
}

/// Op indices a term names through ports, in pre-order.
fn ports(t: &Term, out: &mut Vec<u32>) {
    if let Term::Port(i, _) = t {
        out.push(*i);
    }
    for c in children(t) {
        ports(c, out);
    }
}

fn fold_invariant(op: &Op) -> Option<u32> {
    match (op.opcode, op.operands.last()) {
        (opcode::FOLD_BOUNDED, Some(Operand::Invariant(v))) => Some(*v),
        _ => None,
    }
}

/// Op hashes, bottom-up (step 3), with the operands of commutative ops sorted by hash (step 4).
struct Merkle<'a> {
    h: Hasher<'a>,
    /// Each op's operands in canonical order.
    sorted: Vec<Vec<Operand>>,
    inv_ports: Vec<Vec<u32>>,
}

fn merkle(n: &Node) -> R<Merkle<'_>> {
    let count = n.ops.len();
    // Dependencies: operands, and for a fold the ports its invariant names (ADR 0007, liveness).
    let inv_ports: Vec<Vec<u32>> = n
        .contract
        .invariants
        .iter()
        .map(|conj| {
            let mut v = Vec::new();
            conj.iter().for_each(|t| ports(t, &mut v));
            v
        })
        .collect();
    let deps = |i: usize| -> Vec<u32> {
        let op = &n.ops[i];
        let mut d: Vec<u32> = op
            .operands
            .iter()
            .filter_map(|o| match o {
                Operand::Op(j, _) => Some(*j),
                _ => None,
            })
            .collect();
        if let Some(v) = fold_invariant(op) {
            d.extend(&inv_ports[v as usize]);
        }
        d
    };
    // A dependency order, found without recursion.
    let mut order = Vec::with_capacity(count);
    let mut state = vec![0u8; count];
    for root in 0..count {
        if state[root] != 0 {
            continue;
        }
        let mut stack = vec![(root, deps(root), 0usize)];
        state[root] = 1;
        while let Some((i, ds, k)) = stack.last_mut() {
            if *k < ds.len() {
                let j = ds[*k] as usize;
                *k += 1;
                match state[j] {
                    0 => {
                        state[j] = 1;
                        let dj = deps(j);
                        stack.push((j, dj, 0));
                    }
                    1 => {
                        return Err(Diagnostic::error(
                            "VG-S0007",
                            Span::default(),
                            "an INVARIANT names a port that depends on the fold that uses it",
                        ));
                    }
                    _ => {}
                }
            } else {
                state[*i] = 2;
                order.push(*i);
                stack.pop();
            }
        }
    }
    let mut h = Hasher {
        n,
        op_hash: vec![None; count],
        inv_hash: HashMap::new(),
    };
    let mut sorted: Vec<Vec<Operand>> = n.ops.iter().map(|op| op.operands.clone()).collect();
    for &i in &order {
        let op = &n.ops[i];
        let mut pairs: Vec<(Hash, Operand)> = op
            .operands
            .iter()
            .map(|o| (h.operand_hash(*o), *o))
            .collect();
        let k = match opcode::by_code(op.opcode).expect("validated").comm {
            Comm::No => 0,
            Comm::All => pairs.len(),
            Comm::FirstTwo => 2,
        };
        pairs[..k].sort_by(|a, b| a.0.cmp(&b.0));
        sorted[i] = pairs.iter().map(|p| p.1).collect();
        let mut b = vec![0x05];
        b.extend(op.opcode.to_le_bytes());
        b.push(op.mode);
        b.extend(0u32.to_le_bytes());
        b.push(op.results.len() as u8);
        for t in &op.results {
            t.structural_bytes(&mut b);
        }
        b.extend((pairs.len() as u32).to_le_bytes());
        for p in &pairs {
            b.extend(p.0);
        }
        h.op_hash[i] = Some(derive(Context::Op, &b));
    }
    Ok(Merkle {
        h,
        sorted,
        inv_ports,
    })
}

/// The three hashes of a node, computed from its structure, never from table positions (ADR 0001):
/// body_hash from the Merkle hashes of the outputs and the LEARNABLE box, so equal bodies share it
/// exactly when they unravel to the same terms (docs/13); contract_hash from the clauses with
/// constants, callees and ports by hash; node_hash over the format version, the effect mask, the
/// signature and both. A SPEC node's node_hash is its spec_hash.
pub fn hashes(n: &Node) -> R<Hashes> {
    validate(n)?;
    let mut m = merkle(n)?;
    let mut b = vec![b'B'];
    b.extend((n.outputs.len() as u32).to_le_bytes());
    for o in &n.outputs {
        b.extend(m.h.operand_hash(*o));
    }
    b.extend((n.learnable.len() as u32).to_le_bytes());
    for l in &n.learnable {
        l.ty.structural_bytes(&mut b);
        Hasher::const_bytes(&n.consts[l.lo as usize], &mut b);
        Hasher::const_bytes(&n.consts[l.hi as usize], &mut b);
        match l.loss {
            Some(c) => {
                b.push(1);
                b.extend(n.calls[c as usize]);
            }
            None => b.push(0),
        }
    }
    let body = derive(Context::Body, &b);
    let c = &n.contract;
    let mut b = vec![b'C'];
    for (kind, conj) in [
        (1u8, &c.pre),
        (2, &c.post),
        (3, &c.cost),
        (4, &c.budget),
        (5, &c.decreases),
    ]
    .into_iter()
    .chain(c.invariants.iter().map(|v| (6u8, v)))
    {
        if conj.is_empty() {
            continue;
        }
        b.push(kind);
        b.extend((conj.len() as u32).to_le_bytes());
        for t in conj {
            m.h.term_bytes(t, &mut b);
        }
    }
    let contract = derive(Context::Contract, &b);
    let mut b = Vec::new();
    b.extend(crate::codec::FORMAT_VERSION.to_le_bytes());
    b.extend(n.effects.0.to_le_bytes());
    for ts in [&n.params, &n.results] {
        b.extend((ts.len() as u32).to_le_bytes());
        ts.iter().for_each(|t| t.structural_bytes(&mut b));
    }
    b.extend((n.learnable.len() as u32).to_le_bytes());
    n.learnable
        .iter()
        .for_each(|l| l.ty.structural_bytes(&mut b));
    b.extend(body);
    b.extend(contract);
    let node = derive(if n.spec { Context::Spec } else { Context::Node }, &b);
    Ok(Hashes {
        node,
        body,
        contract,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hashes {
    /// The node_hash, or the spec_hash of a SPEC node.
    pub node: Hash,
    pub body: Hash,
    pub contract: Hash,
}

pub fn canonicalize(n: &Node) -> R<Canonical> {
    validate(n)?;
    let count = n.ops.len();
    let Merkle {
        mut h,
        sorted,
        inv_ports,
    } = merkle(n)?;
    // Step 5: merge equal pure ops; effect ops are never merged (docs/13).
    let mut first: HashMap<Hash, u32> = HashMap::new();
    let rep: Vec<u32> = (0..count)
        .map(|i| {
            if opcode::by_code(n.ops[i].opcode).is_some_and(|o| o.is_effect()) {
                return i as u32;
            }
            *first
                .entry(h.op_hash[i].expect("hashed"))
                .or_insert(i as u32)
        })
        .collect();
    // Step 6: number live ops by a post-order walk from the outputs, in port order; a fold's
    // invariant ports come after its operands.
    let walk_deps = |i: usize| -> Vec<u32> {
        let mut d: Vec<u32> = sorted[i]
            .iter()
            .filter_map(|o| match o {
                Operand::Op(j, _) => Some(rep[*j as usize]),
                _ => None,
            })
            .collect();
        if let Some(v) = fold_invariant(&n.ops[i]) {
            d.extend(inv_ports[v as usize].iter().map(|j| rep[*j as usize]));
        }
        d
    };
    let mut new_ix: Vec<Option<u32>> = vec![None; count];
    let mut numbered: Vec<u32> = Vec::new();
    let mut seen = vec![false; count];
    for o in &n.outputs {
        let Operand::Op(r, _) = o else { continue };
        let r = rep[*r as usize] as usize;
        if seen[r] {
            continue;
        }
        seen[r] = true;
        let mut stack = vec![(r, walk_deps(r), 0usize)];
        while let Some((i, ds, k)) = stack.last_mut() {
            if *k < ds.len() {
                let j = ds[*k] as usize;
                *k += 1;
                if !seen[j] {
                    seen[j] = true;
                    let dj = walk_deps(j);
                    stack.push((j, dj, 0));
                }
            } else {
                new_ix[*i] = Some(numbered.len() as u32);
                numbered.push(*i as u32);
                stack.pop();
            }
        }
    }
    let dropped: Vec<u32> = (0..count as u32)
        .filter(|&i| new_ix[rep[i as usize] as usize].is_none())
        .collect();
    // Live invariants, without duplicates, sorted by hash.
    let mut inv_by_hash: BTreeMap<Hash, u32> = BTreeMap::new();
    for &i in &numbered {
        if let Some(v) = fold_invariant(&n.ops[i as usize]) {
            inv_by_hash.entry(h.invariant_hash(v)).or_insert(v);
        }
    }
    let inv_new: HashMap<Hash, u32> = inv_by_hash
        .keys()
        .enumerate()
        .map(|(k, hh)| (*hh, k as u32))
        .collect();
    let remap_op = |o: Operand| -> Operand {
        match o {
            Operand::Op(i, s) => Operand::Op(new_ix[rep[i as usize] as usize].expect("live"), s),
            o => o,
        }
    };
    let mut ops: Vec<Op> = numbered
        .iter()
        .map(|&i| {
            let op = &n.ops[i as usize];
            Op {
                opcode: op.opcode,
                mode: op.mode,
                operands: sorted[i as usize]
                    .iter()
                    .map(|o| match *o {
                        Operand::Invariant(v) => Operand::Invariant(inv_new[&h.inv_hash[&v]]),
                        o => remap_op(o),
                    })
                    .collect(),
                results: op.results.clone(),
            }
        })
        .collect();
    let mut outputs: Vec<Operand> = n.outputs.iter().map(|o| remap_op(*o)).collect();
    let remap_ports = |t: &mut Term| {
        visit_mut(t, &mut |t| {
            if let Term::Port(i, s) = t {
                *t = Term::Port(
                    new_ix[rep[*i as usize] as usize].expect("invariant ports are live"),
                    *s,
                );
            }
        })
    };
    let old = &n.contract;
    let mut contract = Contract {
        pre: old.pre.clone(),
        post: old.post.clone(),
        cost: old.cost.clone(),
        budget: old.budget.clone(),
        decreases: old.decreases.clone(),
        invariants: inv_by_hash
            .values()
            .map(|v| old.invariants[*v as usize].clone())
            .collect(),
    };
    contract
        .invariants
        .iter_mut()
        .flatten()
        .for_each(remap_ports);
    let mut learnable = n.learnable.clone();
    // CALLS: sorted by hash, without duplicates.
    let mut callees: Vec<Hash> = Vec::new();
    for op in &ops {
        for o in &op.operands {
            if let Operand::Callee(c) = o {
                callees.push(n.calls[*c as usize]);
            }
        }
    }
    callees.extend(
        learnable
            .iter()
            .filter_map(|l| l.loss.map(|c| n.calls[c as usize])),
    );
    for t in all_terms(&contract) {
        visit(t, &mut |t| {
            if let Term::Spec(c, _) | Term::Fold(c, _) | Term::Apply(c, _) = t {
                callees.push(n.calls[*c as usize]);
            }
        });
    }
    callees.sort();
    callees.dedup();
    let call_new = |c: u32| {
        callees
            .binary_search(&n.calls[c as usize])
            .expect("collected") as u32
    };
    // CONSTS: numbered by first reference in serialization order (docs/02).
    let mut consts: Vec<Const> = Vec::new();
    let mut const_new: HashMap<u32, u32> = HashMap::new();
    let mut by_value: HashMap<&Const, u32> = HashMap::new();
    let mut take = |c: u32| -> u32 {
        *const_new.entry(c).or_insert_with(|| {
            let v = &n.consts[c as usize];
            *by_value.entry(v).or_insert_with(|| {
                consts.push(v.clone());
                consts.len() as u32 - 1
            })
        })
    };
    for l in &mut learnable {
        l.lo = take(l.lo);
        l.hi = take(l.hi);
        l.loss = l.loss.map(call_new);
    }
    for op in &mut ops {
        for o in &mut op.operands {
            match o {
                Operand::Const(c) => *c = take(*c),
                Operand::Callee(c) => *c = call_new(*c),
                _ => {}
            }
        }
    }
    for o in &mut outputs {
        if let Operand::Const(c) = o {
            *c = take(*c);
        }
    }
    for t in all_terms_mut(&mut contract) {
        visit_mut(t, &mut |t| match t {
            Term::Const(c) => *c = take(*c),
            Term::Spec(c, _) | Term::Fold(c, _) | Term::Apply(c, _) => *c = call_new(*c),
            _ => {}
        });
    }
    Ok(Canonical {
        node: Node {
            spec: n.spec,
            effects: n.effects,
            params: n.params.clone(),
            results: n.results.clone(),
            learnable,
            consts,
            calls: callees,
            ops,
            outputs,
            contract,
            debug: n.debug.clone(),
        },
        dropped,
    })
}

/// Every formula of a contract, in serialization order: PRE, POST, COST, BUDGET, DECREASES, then
/// each INVARIANT.
pub fn all_terms(c: &Contract) -> impl Iterator<Item = &Term> {
    c.pre
        .iter()
        .chain(&c.post)
        .chain(&c.cost)
        .chain(&c.budget)
        .chain(&c.decreases)
        .chain(c.invariants.iter().flatten())
}

fn all_terms_mut(c: &mut Contract) -> impl Iterator<Item = &mut Term> {
    c.pre
        .iter_mut()
        .chain(c.post.iter_mut())
        .chain(c.cost.iter_mut())
        .chain(c.budget.iter_mut())
        .chain(c.decreases.iter_mut())
        .chain(c.invariants.iter_mut().flatten())
}

/// Pre-order visit.
pub fn visit(t: &Term, f: &mut impl FnMut(&Term)) {
    f(t);
    for c in children(t) {
        visit(c, f);
    }
}

fn visit_mut(t: &mut Term, f: &mut impl FnMut(&mut Term)) {
    f(t);
    for c in children_mut(t) {
        visit_mut(c, f);
    }
}
