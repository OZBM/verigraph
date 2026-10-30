//! The .vgir binary format: a 128-byte header, a section directory and 16-byte-aligned sections
//! (docs/02; byte encodings in ADR 0001). Decoding accepts only canonical bytes: it rebuilds the
//! node, canonicalizes it and requires that encoding gives back exactly the input.

use std::collections::HashMap;

use crate::canon::{self, all_terms, children, ftag, term_head};
use crate::diag::{Diagnostic, Span};
use crate::ir::{
    Bin, Builtin, Const, Contract, Hash, Learnable, Node, Op, Operand, Resource, Term,
};
use crate::lower::{MAX_TERM_DEPTH, int_value};
use crate::types::{EffectMask, Handle, Type, tag};

pub const MAGIC: &[u8; 4] = b"VGIR";
pub const FORMAT_VERSION: u16 = 2;
pub const HEADER_LEN: usize = 128;

pub mod kind {
    pub const TYPES: u16 = 0x01;
    pub const SIGNATURE: u16 = 0x02;
    pub const CONSTS: u16 = 0x03;
    pub const OPS: u16 = 0x04;
    pub const OPERANDS: u16 = 0x05;
    pub const RESULTS: u16 = 0x06;
    pub const CALLS: u16 = 0x07;
    pub const CONTRACT: u16 = 0x08;
    pub const LEARNABLE: u16 = 0x09;
    pub const DEBUG: u16 = 0x7F;
}

/// CONTRACT clause kinds, in their canonical order.
pub mod clause {
    pub const PRE: u8 = 1;
    pub const POST: u8 = 2;
    pub const COST: u8 = 3;
    pub const BUDGET: u8 = 4;
    pub const DECREASES: u8 = 5;
    pub const INVARIANT: u8 = 6;
}

const NO_LOSS: u32 = u32::MAX;
const DEBUG_VERSION: u32 = 1;

pub use crate::canon::Hashes;

struct Section {
    kind: u16,
    count: u32,
    bytes: Vec<u8>,
}

fn align16(x: usize) -> usize {
    x.div_ceil(16) * 16
}

/// TYPES numbering: each distinct type once, nested types first, in order of first reference.
#[derive(Default)]
struct Types {
    entries: Vec<(u8, Vec<u32>)>,
    index: HashMap<Type, u32>,
}

impl Types {
    fn id(&mut self, t: &Type) -> u32 {
        if let Some(&i) = self.index.get(t) {
            return i;
        }
        let (tg, args) = match t {
            Type::Bool => (tag::BOOL, vec![]),
            Type::UInt(b) => (tag::UINT, vec![u32::from(*b)]),
            Type::SInt(b) => (tag::SINT, vec![u32::from(*b)]),
            Type::Float(b) => (tag::FLOAT, vec![u32::from(*b)]),
            Type::BFloat16 => (tag::BFLOAT16, vec![]),
            Type::Int => (tag::INT, vec![]),
            Type::Array(e, n) => (tag::ARRAY, vec![self.id(e), *n]),
            Type::Slice(e) => (tag::SLICE, vec![self.id(e)]),
            Type::Tensor(e, d) => {
                let mut v = vec![self.id(e)];
                v.extend(d);
                (tag::TENSOR, v)
            }
            Type::Tuple(ms) => (tag::TUPLE, ms.iter().map(|m| self.id(m)).collect()),
            Type::Result(a, b) => (tag::RESULT, vec![self.id(a), self.id(b)]),
            Type::Arena(n) => (tag::ARENA, vec![*n]),
            Type::Eff => (tag::EFF, vec![]),
            Type::Handle(h) => (tag::HANDLE, vec![*h as u32]),
        };
        let i = self.entries.len() as u32;
        self.entries.push((tg, args));
        self.index.insert(t.clone(), i);
        i
    }
}

fn u16le(v: u16, out: &mut Vec<u8>) {
    out.extend(v.to_le_bytes());
}

fn u32le(v: u32, out: &mut Vec<u8>) {
    out.extend(v.to_le_bytes());
}

fn term_nodes(t: &Term, out: &mut Vec<u8>) {
    let (tg, argc, payload) = term_head(t);
    u16le(tg, out);
    u16le(argc, out);
    u32le(payload, out);
    for c in children(t) {
        term_nodes(c, out);
    }
}

/// Encodes a node as it is; a canonical node gives canonical bytes.
pub fn encode(n: &Node) -> (Vec<u8>, Hashes) {
    let mut types = Types::default();
    let thetas: Vec<&Type> = n.learnable.iter().map(|l| &l.ty).collect();
    for t in n
        .params
        .iter()
        .chain(&n.results)
        .chain(thetas.iter().copied())
    {
        types.id(t);
    }
    let const_ty = |types: &mut Types, c: u32| {
        types.id(&n.consts[c as usize].ty);
    };
    for l in &n.learnable {
        const_ty(&mut types, l.lo);
        const_ty(&mut types, l.hi);
    }
    for op in &n.ops {
        for t in &op.results {
            types.id(t);
        }
        for o in &op.operands {
            if let Operand::Const(c) = o {
                const_ty(&mut types, *c);
            }
        }
    }
    for o in &n.outputs {
        if let Operand::Const(c) = o {
            const_ty(&mut types, *c);
        }
    }
    for t in all_terms(&n.contract) {
        canon::visit(t, &mut |t| {
            if let Term::Const(c) = t {
                const_ty(&mut types, *c);
            }
        });
    }
    for c in &n.consts {
        types.id(&c.ty);
    }

    let mut sections: Vec<Section> = Vec::new();
    let mut b = Vec::new();
    for (tg, args) in &types.entries {
        b.extend([*tg, args.len() as u8, 0, 0]);
        args.iter().for_each(|a| u32le(*a, &mut b));
    }
    sections.push(Section {
        kind: kind::TYPES,
        count: types.entries.len() as u32,
        bytes: b,
    });

    let mut b = Vec::new();
    let sig: Vec<(u32, u16)> = n
        .params
        .iter()
        .map(|t| (types.index[t], 0))
        .chain(n.results.iter().map(|t| (types.index[t], 1)))
        .chain(thetas.iter().map(|t| (types.index[*t], 2)))
        .collect();
    for (t, role) in &sig {
        u32le(*t, &mut b);
        u16le(*role, &mut b);
        u16le(0, &mut b);
    }
    sections.push(Section {
        kind: kind::SIGNATURE,
        count: sig.len() as u32,
        bytes: b,
    });

    let mut b = Vec::new();
    for c in &n.consts {
        u32le(types.index[&c.ty], &mut b);
        u32le(c.bytes.len() as u32, &mut b);
        b.extend(&c.bytes);
        b.resize(b.len().div_ceil(4) * 4, 0);
    }
    sections.push(Section {
        kind: kind::CONSTS,
        count: n.consts.len() as u32,
        bytes: b,
    });

    let (mut ops, mut operands, mut results) = (Vec::new(), Vec::new(), Vec::new());
    let mut start = 0u32;
    let operand = |o: &Operand, out: &mut Vec<u8>| {
        let (k, slot, ix) = match *o {
            Operand::Input(i) => (0, 0, i),
            Operand::Op(i, s) => (1, s, i),
            Operand::Const(c) => (2, 0, c),
            Operand::Callee(c) => (3, 0, c),
            Operand::Invariant(v) => (4, 0, v),
        };
        out.extend([k, slot, 0, 0]);
        u32le(ix, out);
    };
    for op in &n.ops {
        u16le(op.opcode, &mut ops);
        ops.extend([op.mode, op.results.len() as u8]);
        u32le(start, &mut ops);
        u32le(op.operands.len() as u32, &mut ops);
        u32le(0, &mut ops);
        start += op.operands.len() as u32;
        op.operands.iter().for_each(|o| operand(o, &mut operands));
        op.results
            .iter()
            .for_each(|t| u32le(types.index[t], &mut results));
    }
    n.outputs.iter().for_each(|o| operand(o, &mut operands));
    let n_results: usize = n.ops.iter().map(|o| o.results.len()).sum();
    sections.push(Section {
        kind: kind::OPS,
        count: n.ops.len() as u32,
        bytes: ops,
    });
    sections.push(Section {
        kind: kind::OPERANDS,
        count: start + n.outputs.len() as u32,
        bytes: operands,
    });
    sections.push(Section {
        kind: kind::RESULTS,
        count: n_results as u32,
        bytes: results,
    });
    sections.push(Section {
        kind: kind::CALLS,
        count: n.calls.len() as u32,
        bytes: n.calls.concat(),
    });

    let c = &n.contract;
    let mut clauses: Vec<(u8, &Vec<Term>)> = [
        (clause::PRE, &c.pre),
        (clause::POST, &c.post),
        (clause::COST, &c.cost),
        (clause::BUDGET, &c.budget),
        (clause::DECREASES, &c.decreases),
    ]
    .into_iter()
    .filter(|(_, v)| !v.is_empty())
    .collect();
    clauses.extend(c.invariants.iter().map(|v| (clause::INVARIANT, v)));
    let (mut table, mut nodes) = (Vec::new(), Vec::new());
    for (k, conj) in &clauses {
        table.extend([*k, 0, 0, 0]);
        u32le((nodes.len() / 8) as u32, &mut table);
        u16le(ftag::BIN + Bin::And as u16, &mut nodes);
        u16le(conj.len() as u16, &mut nodes);
        u32le(0, &mut nodes);
        conj.iter().for_each(|t| term_nodes(t, &mut nodes));
    }
    table.extend(nodes);
    sections.push(Section {
        kind: kind::CONTRACT,
        count: clauses.len() as u32,
        bytes: table,
    });

    let mut b = Vec::new();
    let first_theta = (n.params.len() + n.results.len()) as u32;
    for (j, l) in n.learnable.iter().enumerate() {
        u32le(first_theta + j as u32, &mut b);
        u32le(types.index[&l.ty], &mut b);
        u32le(l.lo, &mut b);
        u32le(l.hi, &mut b);
        u32le(l.loss.unwrap_or(NO_LOSS), &mut b);
    }
    sections.push(Section {
        kind: kind::LEARNABLE,
        count: n.learnable.len() as u32,
        bytes: b,
    });
    if let Some(d) = &n.debug {
        let mut b = Vec::new();
        u32le(DEBUG_VERSION, &mut b);
        b.extend(d.as_bytes());
        sections.push(Section {
            kind: kind::DEBUG,
            count: 1,
            bytes: b,
        });
    }

    let hashes = canon::hashes(n).expect("encode takes a valid node");
    let (node_hash, body_hash, contract_hash) = (hashes.node, hashes.body, hashes.contract);

    // Empty sections are left out.
    sections.retain(|s| s.count > 0);
    let mut out = Vec::new();
    out.extend(MAGIC);
    u16le(FORMAT_VERSION, &mut out);
    u16le(
        u16::from(n.debug.is_some()) | u16::from(n.spec) << 2,
        &mut out,
    );
    u32le(n.effects.0, &mut out);
    u32le(sections.len() as u32, &mut out);
    out.extend(node_hash);
    out.extend(body_hash);
    out.extend(contract_hash);
    out.resize(HEADER_LEN, 0);
    let mut offset = align16(HEADER_LEN + 16 * sections.len());
    let mut layout = Vec::new();
    for s in &sections {
        layout.push(offset);
        offset = align16(offset + s.bytes.len());
    }
    for (s, off) in sections.iter().zip(&layout) {
        u16le(s.kind, &mut out);
        u16le(0, &mut out);
        u32le(s.count, &mut out);
        u32le(*off as u32, &mut out);
        u32le(s.bytes.len() as u32, &mut out);
    }
    for (s, off) in sections.iter().zip(&layout) {
        out.resize(*off, 0);
        out.extend(&s.bytes);
    }
    out.resize(align16(out.len()), 0);
    (out, hashes)
}

fn bad(code: &'static str, msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error(code, Span::default(), msg)
}

/// A bounds-checked little-endian reader over one section.
struct Rd<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Diagnostic> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.b.len());
        let end = end.ok_or_else(|| bad("VG-S0202", "section shorter than its entries"))?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, Diagnostic> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, Diagnostic> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }
    fn u32(&mut self) -> Result<u32, Diagnostic> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
    fn done(&self) -> Result<(), Diagnostic> {
        if self.at == self.b.len() {
            Ok(())
        } else {
            Err(bad("VG-S0202", "section longer than its entries"))
        }
    }
}

fn get<'a, T>(v: &'a [T], i: u32, what: &str) -> Result<&'a T, Diagnostic> {
    v.get(i as usize)
        .ok_or_else(|| bad("VG-S0203", format!("{what} index {i} out of range")))
}

/// Decodes canonical .vgir bytes. Anything else, including a node that is well formed but not in
/// canonical form, is rejected.
pub fn decode(bytes: &[u8]) -> Result<Node, Diagnostic> {
    if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
        return Err(bad("VG-S0201", "not a .vgir file: bad magic or too short"));
    }
    let h = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let w = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().expect("4 bytes"));
    if h(4) != FORMAT_VERSION {
        return Err(bad(
            "VG-S0201",
            format!("format_version {:#06x}; this toolchain reads 0x0002", h(4)),
        ));
    }
    let flags = h(6);
    if flags & !0b101 != 0 {
        return Err(bad(
            "VG-S0201",
            "unknown header flags, or scc_member, which is [ALPHA]",
        ));
    }
    let effects = w(8);
    if effects & EffectMask::RESERVED != 0 {
        return Err(bad("VG-E0105", "reserved effect bit set"));
    }
    if bytes[112..128].iter().any(|b| *b != 0) {
        return Err(bad("VG-S0201", "reserved header bytes are not zero"));
    }
    let nsec = w(12) as usize;
    if nsec > 10 || HEADER_LEN + 16 * nsec > bytes.len() {
        return Err(bad("VG-S0202", "section directory out of range"));
    }
    let mut sec: HashMap<u16, (u32, &[u8])> = HashMap::new();
    let mut last_kind = 0u16;
    for i in 0..nsec {
        let d = HEADER_LEN + 16 * i;
        let (k, fl, count, off, len) = (
            h(d),
            h(d + 2),
            w(d + 4),
            w(d + 8) as usize,
            w(d + 12) as usize,
        );
        let known = (kind::TYPES..=kind::LEARNABLE).contains(&k) || k == kind::DEBUG;
        if !known || k <= last_kind || fl != 0 {
            return Err(bad(
                "VG-S0202",
                format!("section kind {k:#04x} unknown, out of order or flagged"),
            ));
        }
        last_kind = k;
        let end = off.checked_add(len).filter(|e| *e <= bytes.len());
        if off % 16 != 0 || end.is_none() {
            return Err(bad(
                "VG-S0202",
                format!("section {k:#04x} misaligned or out of range"),
            ));
        }
        sec.insert(k, (count, &bytes[off..off + len]));
    }
    let section = |k: u16| sec.get(&k).copied().unwrap_or((0, &[][..]));

    // TYPES: nested types precede the types built from them.
    let (count, b) = section(kind::TYPES);
    let mut rd = Rd { b, at: 0 };
    let mut types: Vec<Type> = Vec::new();
    let mut depths: Vec<u32> = Vec::new();
    for _ in 0..count {
        let (tg, argc) = (rd.u8()?, rd.u8()?);
        if rd.u16()? != 0 {
            return Err(bad("VG-S0202", "reserved TYPES bytes are not zero"));
        }
        let mut args = Vec::with_capacity(usize::from(argc));
        for _ in 0..argc {
            args.push(rd.u32()?);
        }
        let ty = |i: u32| get(&types, i, "type").cloned();
        let depth_of = |i: u32| depths.get(i as usize).copied().unwrap_or(0);
        let arity = |n: usize| {
            if args.len() == n {
                Ok(())
            } else {
                Err(bad("VG-S0202", "type with the wrong argument count"))
            }
        };
        let width = |b: u32, ok: &[u32]| {
            if ok.contains(&b) {
                Ok(b as u8)
            } else {
                Err(bad("VG-S0202", "bad bit width"))
            }
        };
        let (t, kids): (Type, Vec<u32>) = match tg {
            tag::BOOL => (arity(0).map(|_| Type::Bool)?, vec![]),
            tag::UINT => (
                arity(1)
                    .and_then(|_| width(args[0], &[8, 16, 32, 64]))
                    .map(Type::UInt)?,
                vec![],
            ),
            tag::SINT => (
                arity(1)
                    .and_then(|_| width(args[0], &[8, 16, 32, 64]))
                    .map(Type::SInt)?,
                vec![],
            ),
            tag::FLOAT => (
                arity(1)
                    .and_then(|_| width(args[0], &[16, 32, 64]))
                    .map(Type::Float)?,
                vec![],
            ),
            tag::BFLOAT16 => (arity(0).map(|_| Type::BFloat16)?, vec![]),
            tag::INT => (arity(0).map(|_| Type::Int)?, vec![]),
            tag::ARRAY => {
                arity(2)?;
                (Type::Array(Box::new(ty(args[0])?), args[1]), vec![args[0]])
            }
            tag::SLICE => {
                arity(1)?;
                (Type::Slice(Box::new(ty(args[0])?)), vec![args[0]])
            }
            tag::TENSOR if args.len() >= 2 => (
                Type::Tensor(Box::new(ty(args[0])?), args[1..].to_vec()),
                vec![args[0]],
            ),
            tag::TUPLE if !args.is_empty() => (
                Type::Tuple(args.iter().map(|a| ty(*a)).collect::<Result<_, _>>()?),
                args.clone(),
            ),
            tag::RESULT => {
                arity(2)?;
                (
                    Type::Result(Box::new(ty(args[0])?), Box::new(ty(args[1])?)),
                    args.clone(),
                )
            }
            tag::ARENA => (arity(1).map(|_| Type::Arena(args[0]))?, vec![]),
            tag::EFF => (arity(0).map(|_| Type::Eff)?, vec![]),
            tag::HANDLE => {
                arity(1)?;
                let hk = Handle::from_u32(args[0])
                    .ok_or_else(|| bad("VG-S0202", "unknown handle kind"))?;
                (Type::Handle(hk), vec![])
            }
            _ => return Err(bad("VG-S0202", format!("unknown type tag {tg:#04x}"))),
        };
        let d = 1 + kids.iter().map(|k| depth_of(*k)).max().unwrap_or(0);
        if d > 64 {
            return Err(bad("VG-S0202", "type nested deeper than 64 levels"));
        }
        depths.push(d);
        types.push(t);
    }
    rd.done()?;

    let (count, b) = section(kind::SIGNATURE);
    let mut rd = Rd { b, at: 0 };
    let (mut params, mut results, mut thetas) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..count {
        let t = get(&types, rd.u32()?, "type")?.clone();
        let (role, fl) = (rd.u16()?, rd.u16()?);
        let in_order = match role {
            0 => results.is_empty() && thetas.is_empty(),
            1 => thetas.is_empty(),
            2 => true,
            _ => false,
        };
        if fl != 0 || !in_order {
            return Err(bad("VG-S0202", "SIGNATURE entries out of order or flagged"));
        }
        [&mut params, &mut results, &mut thetas][role as usize].push(t);
    }
    rd.done()?;

    let (count, b) = section(kind::CONSTS);
    let mut rd = Rd { b, at: 0 };
    let mut consts = Vec::new();
    for _ in 0..count {
        let ty = get(&types, rd.u32()?, "type")?.clone();
        let len = rd.u32()? as usize;
        let bytes = rd.take(len)?.to_vec();
        if rd.take((4 - len % 4) % 4)?.iter().any(|b| *b != 0) {
            return Err(bad("VG-S0202", "CONSTS padding is not zero"));
        }
        check_const(&ty, &bytes)?;
        consts.push(Const { ty, bytes });
    }
    rd.done()?;

    let (count, b) = section(kind::RESULTS);
    let mut rd = Rd { b, at: 0 };
    let mut result_types = Vec::new();
    for _ in 0..count {
        result_types.push(get(&types, rd.u32()?, "type")?.clone());
    }
    rd.done()?;

    let (count, b) = section(kind::OPERANDS);
    let mut rd = Rd { b, at: 0 };
    let mut operands = Vec::new();
    for _ in 0..count {
        let (k, slot) = (rd.u8()?, rd.u8()?);
        let reserved = rd.u16()?;
        let ix = rd.u32()?;
        if reserved != 0 || slot != 0 && k != 1 {
            return Err(bad("VG-S0202", "OPERANDS reserved bytes or slot set"));
        }
        operands.push(match k {
            0 => Operand::Input(ix),
            1 => Operand::Op(ix, slot),
            2 => Operand::Const(ix),
            3 => Operand::Callee(ix),
            4 => Operand::Invariant(ix),
            _ => return Err(bad("VG-S0202", format!("unknown operand kind {k}"))),
        });
    }
    rd.done()?;

    let (count, b) = section(kind::OPS);
    let mut rd = Rd { b, at: 0 };
    let mut ops = Vec::new();
    let (mut next_operand, mut next_result) = (0usize, 0usize);
    for _ in 0..count {
        let opcode = rd.u16()?;
        let (mode, nres) = (rd.u8()?, usize::from(rd.u8()?));
        let (start, n) = (rd.u32()? as usize, rd.u32()? as usize);
        if rd.u32()? != 0
            || start != next_operand
            || start + n > operands.len()
            || next_result + nres > result_types.len()
        {
            return Err(bad("VG-S0202", "OPS entry out of place"));
        }
        ops.push(Op {
            opcode,
            mode,
            operands: operands[start..start + n].to_vec(),
            results: result_types[next_result..next_result + nres].to_vec(),
        });
        next_operand += n;
        next_result += nres;
    }
    rd.done()?;
    if next_result != result_types.len() {
        return Err(bad("VG-S0202", "RESULTS longer than the ops' results"));
    }
    let outputs = operands[next_operand..].to_vec();

    let (count, b) = section(kind::CALLS);
    if b.len() != count as usize * 32 {
        return Err(bad("VG-S0202", "CALLS length is not 32 bytes per hash"));
    }
    let calls: Vec<Hash> = b
        .chunks(32)
        .map(|c| c.try_into().expect("32 bytes"))
        .collect();

    let (count, b) = section(kind::CONTRACT);
    let contract = decode_contract(count, b)?;

    let (count, b) = section(kind::LEARNABLE);
    let mut rd = Rd { b, at: 0 };
    let mut learnable = Vec::new();
    let first_theta = (params.len() + results.len()) as u32;
    for j in 0..count {
        let (p, t, lo, hi, loss) = (rd.u32()?, rd.u32()?, rd.u32()?, rd.u32()?, rd.u32()?);
        let ty = get(&types, t, "type")?.clone();
        if p != first_theta + j || thetas.get(j as usize) != Some(&ty) {
            return Err(bad(
                "VG-S0202",
                "LEARNABLE entry does not match its SIGNATURE entry",
            ));
        }
        learnable.push(Learnable {
            ty,
            lo,
            hi,
            loss: (loss != NO_LOSS).then_some(loss),
        });
    }
    rd.done()?;
    if learnable.len() != thetas.len() {
        return Err(bad("VG-S0202", "one LEARNABLE entry per θ"));
    }

    let debug = match sec.get(&kind::DEBUG) {
        Some((1, b)) if b.len() >= 4 && b[..4] == DEBUG_VERSION.to_le_bytes() => Some(
            String::from_utf8(b[4..].to_vec())
                .map_err(|_| bad("VG-S0202", "DEBUG is not UTF-8"))?,
        ),
        Some(_) => return Err(bad("VG-S0202", "DEBUG section of an unknown version")),
        None => None,
    };
    let node = Node {
        spec: flags & 0b100 != 0,
        effects: EffectMask(effects),
        params,
        results,
        learnable,
        consts,
        calls,
        ops,
        outputs,
        contract,
        debug,
    };
    let canonical = canon::canonicalize(&node)?.node;
    if encode(&canonical).0 != bytes {
        return Err(bad(
            "VG-S0204",
            "not canonical: re-canonicalizing or re-encoding changes the bytes, or a hash does not match",
        ));
    }
    Ok(node)
}

/// Constant bytes must have their type's size; an Int is non-negative in its shortest form; a float
/// NaN is the canonical one (ADR 0002); a Bool is 0 or 1.
fn check_const(t: &Type, b: &[u8]) -> Result<(), Diagnostic> {
    let ok = match t {
        Type::Int => int_value(b).is_some_and(|v| v >= 0 && crate::lower::int_const(v).bytes == b),
        _ => match t.byte_size() {
            Some(n) if n == b.len() as u64 => match t {
                Type::Bool => b[0] <= 1,
                t if t.is_float() => {
                    let mut w = [0u8; 8];
                    w[..b.len()].copy_from_slice(b);
                    let bits = u64::from_le_bytes(w);
                    !crate::float::is_nan(t, bits) || bits == crate::float::canonical_nan(t)
                }
                Type::Array(..) | Type::Tensor(..) => {
                    let e = t.elem().expect("collection");
                    let size = e.byte_size().unwrap_or(0) as usize;
                    size > 0
                        && e.is_scalar_or_bool()
                        && b.chunks(size).all(|c| check_const(e, c).is_ok())
                }
                _ => true,
            },
            _ => false,
        },
    };
    if ok {
        Ok(())
    } else {
        Err(bad(
            "VG-S0204",
            format!("constant of type {t} is not canonical"),
        ))
    }
}

fn decode_contract(count: u32, b: &[u8]) -> Result<Contract, Diagnostic> {
    let mut rd = Rd { b, at: 0 };
    let mut table = Vec::new();
    for _ in 0..count {
        let k = rd.u8()?;
        let (r1, r2) = (rd.u8()?, rd.u16()?);
        let root = rd.u32()?;
        if r1 != 0 || r2 != 0 || !(clause::PRE..=clause::INVARIANT).contains(&k) {
            return Err(bad("VG-S0202", "bad CONTRACT clause entry"));
        }
        table.push((k, root));
    }
    let nodes_bytes = &b[rd.at..];
    if !nodes_bytes.len().is_multiple_of(8) {
        return Err(bad(
            "VG-S0202",
            "CONTRACT node array is not 8 bytes per node",
        ));
    }
    let nodes: Vec<(u16, u16, u32)> = nodes_bytes
        .chunks(8)
        .map(|c| {
            (
                u16::from_le_bytes([c[0], c[1]]),
                u16::from_le_bytes([c[2], c[3]]),
                u32::from_le_bytes([c[4], c[5], c[6], c[7]]),
            )
        })
        .collect();
    let mut at = 0usize;
    let mut c = Contract::default();
    let mut last = 0u8;
    for (k, root) in table {
        if root as usize != at || k < last || k == last && k != clause::INVARIANT {
            return Err(bad("VG-S0202", "CONTRACT clauses out of order"));
        }
        last = k;
        let &(tg, argc, payload) = nodes
            .get(at)
            .ok_or_else(|| bad("VG-S0202", "CONTRACT root out of range"))?;
        if tg != ftag::BIN + Bin::And as u16 || argc == 0 || payload != 0 {
            return Err(bad("VG-S0202", "a clause root is an AND of its conjuncts"));
        }
        at += 1;
        let mut conj = Vec::new();
        for _ in 0..argc {
            conj.push(read_term(&nodes, &mut at, 0)?);
        }
        match k {
            clause::PRE => c.pre = conj,
            clause::POST => c.post = conj,
            clause::COST => c.cost = conj,
            clause::BUDGET => c.budget = conj,
            clause::DECREASES => c.decreases = conj,
            _ => c.invariants.push(conj),
        }
    }
    if at != nodes.len() {
        return Err(bad("VG-S0202", "CONTRACT nodes left over"));
    }
    Ok(c)
}

fn read_term(nodes: &[(u16, u16, u32)], at: &mut usize, depth: usize) -> Result<Term, Diagnostic> {
    if depth > MAX_TERM_DEPTH {
        return Err(bad("VG-S0202", "formula nested too deep"));
    }
    let &(tg, argc, p) = nodes
        .get(*at)
        .ok_or_else(|| bad("VG-S0202", "formula runs past the CONTRACT section"))?;
    *at += 1;
    let mut kids = Vec::with_capacity(usize::from(argc).min(nodes.len()));
    for _ in 0..argc {
        kids.push(read_term(nodes, at, depth + 1)?);
    }
    let bx = Box::new;
    let malformed = || bad("VG-S0202", format!("malformed formula node {tg:#06x}"));
    let mut it = kids.into_iter();
    let n = usize::from(argc);
    let leaf = |t: Term| if n == 0 { Ok(t) } else { Err(malformed()) };
    Ok(match tg {
        ftag::INPUT => leaf(Term::Input(p))?,
        ftag::RESULT => leaf(Term::Result(p))?,
        ftag::PORT => leaf(Term::Port(p >> 8, p as u8))?,
        ftag::BOUND => leaf(Term::Bound(p))?,
        ftag::CONST => leaf(Term::Const(p))?,
        ftag::RESOURCE => leaf(Term::Resource(
            *Resource::ALL.get(p as usize).ok_or_else(malformed)?,
        ))?,
        ftag::NOT if n == 1 && p == 0 => Term::Not(bx(it.next().expect("one"))),
        ftag::NEG if n == 1 && p == 0 => Term::Neg(bx(it.next().expect("one"))),
        t if (ftag::BIN..ftag::BIN + 14).contains(&t) && n == 2 && p == 0 => {
            const BINS: [Bin; 14] = [
                Bin::And,
                Bin::Or,
                Bin::Implies,
                Bin::Eq,
                Bin::Ne,
                Bin::Lt,
                Bin::Le,
                Bin::Gt,
                Bin::Ge,
                Bin::Add,
                Bin::Sub,
                Bin::Mul,
                Bin::Div,
                Bin::Mod,
            ];
            let (a, b) = (it.next().expect("two"), it.next().expect("two"));
            Term::Bin(BINS[usize::from(t - ftag::BIN)], bx(a), bx(b))
        }
        ftag::INDEX if n >= 2 && p == 0 => {
            let base = it.next().expect("base");
            Term::Index(bx(base), it.collect())
        }
        ftag::PROJ if n == 1 => Term::Proj(bx(it.next().expect("one")), p),
        ftag::FORALL if p >= 1 && n as u64 == 2 * u64::from(p) + 1 => {
            let mut v: Vec<Term> = it.collect();
            let body = v.pop().expect("body");
            let mut rs = Vec::new();
            let mut v = v.into_iter();
            while let (Some(lo), Some(hi)) = (v.next(), v.next()) {
                rs.push((lo, hi));
            }
            Term::Forall(rs, bx(body))
        }
        ftag::BUILTIN => {
            let b = Builtin::ALL
                .iter()
                .find(|b| **b as u32 == p && b.arity() == n)
                .ok_or_else(malformed)?;
            Term::Builtin(*b, it.collect())
        }
        ftag::SPEC => Term::Spec(p, it.collect()),
        ftag::FOLD => Term::Fold(p, it.collect()),
        ftag::APPLY => Term::Apply(p, it.collect()),
        _ => return Err(malformed()),
    })
}
