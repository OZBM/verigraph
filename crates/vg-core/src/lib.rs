//! Data model, VG-Sym parser and printer, .vgir codec, canonicalizer, BLAKE3 hashing, validator.

pub mod ast;
pub mod canon;
pub mod codec;
pub mod diag;
pub mod disasm;
pub mod float;
#[cfg(feature = "gen")]
pub mod generate;
pub mod hash;
pub mod ir;
pub mod lexer;
pub mod lower;
pub mod opcode;
pub mod parser;
pub mod printer;
pub mod types;

use std::collections::HashMap;

use ast::{Item, TypeExpr};
use codec::Hashes;
use diag::Diagnostic;
use ir::{Hash, Node};
use lower::{Callable, Kind, Labels};
use types::EffectMask;

/// One assembled node: canonical, encoded and hashed.
#[derive(Clone, Debug)]
pub struct Assembled {
    pub label: String,
    pub node: Node,
    pub hashes: Hashes,
    pub bytes: Vec<u8>,
    pub warnings: Vec<Diagnostic>,
}

/// How a @MESH_QUERY label resolves: vg.lock's node hash and spec hash for it, if pinned.
pub type MeshLookup<'a> = &'a dyn Fn(&str) -> Option<(Hash, Option<Hash>)>;

/// `vg asm`: parse, lower, canonicalize, encode and hash every NODE and SPEC of a module, in order.
/// With `keep_debug`, each node's source text goes into its DEBUG section.
pub fn asm(src: &str, mesh: MeshLookup, keep_debug: bool) -> Result<Vec<Assembled>, Diagnostic> {
    let m = parser::parse(src)?;
    asm_module(&m, src, mesh, keep_debug)
}

pub fn asm_module(
    m: &ast::Module,
    src: &str,
    mesh: MeshLookup,
    keep_debug: bool,
) -> Result<Vec<Assembled>, Diagnostic> {
    let base = |t: &TypeExpr, span| match t {
        TypeExpr::Refine(_, b, _) => lower::lower_type(b, span),
        t => lower::lower_type(t, span),
    };
    let mut labels: Labels = HashMap::new();
    for (k, item) in m.items.iter().enumerate() {
        let (label, c) = match item {
            Item::Node(n) => {
                let mut effects = 0;
                for e in &n.effects {
                    effects |= EffectMask::bit(&e.text).expect("effect keyword");
                }
                (
                    &n.label,
                    Callable {
                        kind: if n.spec { Kind::Spec } else { Kind::Node },
                        item: k,
                        params: n
                            .params
                            .iter()
                            .map(|p| base(&p.ty, p.name.span))
                            .collect::<Result<_, _>>()?,
                        results: n
                            .results
                            .iter()
                            .map(|t| base(t, n.span))
                            .collect::<Result<_, _>>()?,
                        effects: EffectMask(effects),
                        hash: None,
                        spec: None,
                    },
                )
            }
            Item::Query(q) => {
                let mut effects = 0;
                for e in &q.effects {
                    effects |= EffectMask::bit(&e.text).expect("effect keyword");
                }
                let resolved = mesh(&q.label);
                (
                    &q.label,
                    Callable {
                        kind: Kind::Query,
                        item: k,
                        params: q
                            .params
                            .iter()
                            .map(|t| lower::lower_type(t, q.span))
                            .collect::<Result<_, _>>()?,
                        results: q
                            .results
                            .iter()
                            .map(|t| lower::lower_type(t, q.span))
                            .collect::<Result<_, _>>()?,
                        effects: EffectMask(effects),
                        hash: resolved.map(|r| r.0),
                        spec: resolved.and_then(|r| r.1),
                    },
                )
            }
        };
        if labels.insert(label.clone(), c).is_some() {
            let span = match item {
                Item::Node(n) => n.span,
                Item::Query(q) => q.span,
            };
            return Err(Diagnostic::error(
                "VG-S0006",
                span,
                format!("label @{label} is defined twice"),
            ));
        }
    }
    let chars: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    for (k, item) in m.items.iter().enumerate() {
        let Item::Node(def) = item else { continue };
        let here = format!("@{}", def.label);
        let lowered = lower::NodeLowering::new(&labels, k, def).lower()?;
        let canonical = canon::canonicalize(&lowered.node).map_err(|e| e.at_node(&here))?;
        let mut node = canonical.node;
        if keep_debug {
            let s = def.span;
            node.debug = chars
                .get(s.offset as usize..(s.offset + s.len) as usize)
                .map(|c| c.iter().collect());
        }
        let (bytes, hashes) = codec::encode(&node);
        let mut warnings = lowered.warnings;
        for &i in &canonical.dropped {
            let mut w = Diagnostic::warning(
                "VG-W0001",
                def.span,
                "op dropped by canonicalization: no output, effect or invariant uses it",
            )
            .at_node(&here);
            w.op = lowered.op_ports.get(i as usize).map(|p| format!("%{p}"));
            warnings.push(w);
        }
        let entry = labels.get_mut(&def.label).expect("declared above");
        entry.hash = Some(hashes.node);
        if def.spec {
            entry.spec = Some(hashes.node);
        }
        out.push(Assembled {
            label: def.label.clone(),
            node,
            hashes,
            bytes,
            warnings,
        });
    }
    Ok(out)
}

/// Assembles a module that calls no @MESH_QUERY, as tests and tools do.
pub fn asm_local(src: &str) -> Result<Vec<Assembled>, Diagnostic> {
    asm(src, &|_| None, false)
}
