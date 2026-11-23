//! Type checker, effect and capability checker, resource accounting.

mod check;
pub mod cost;
mod formula;

use std::collections::BTreeMap;

use vg_core::diag::Diagnostic;
use vg_core::ir::{Hash, Node};
use vg_core::types::EffectMask;

pub use cost::CostExpr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Effects the node may perform, its callees' included.
    pub effects: EffectMask,
    /// Static resource terms of docs/14 (P2.3), at least the op count.
    pub cost_ops: u64,
    /// Exact symbolic static `ops` cost expression (docs/05, docs/14).
    pub ops_expr: CostExpr,
    /// Exact symbolic static `arena_bytes` cost expression (docs/05, ADR 0003).
    pub arena_bytes: CostExpr,
    /// Static `heap_bytes` (always 0 in the MVP).
    pub heap_bytes: u64,
    /// Effects actually required by the node's ops and its callees.
    pub used_effects: EffectMask,
}

/// Checks one node. `callee` resolves a CALLS hash to its node; an unresolvable callee is an error.
/// Diagnostics name the op by its index in `node.ops` in `Diagnostic::op` (e.g. "op 3").
pub fn check_node(
    node: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
) -> Result<Summary, Vec<Diagnostic>> {
    check::check_node_entry(node, callee)
}

/// Assembles a VG-Sym module with vg_core::asm_local and checks every node, resolving calls among
/// the module's own nodes. Returns (label, summary) per node, in module order.
pub fn check_source(src: &str) -> Result<Vec<(String, Summary)>, Vec<Diagnostic>> {
    let assembled = vg_core::asm_local(src).map_err(|d| vec![d])?;
    let mut by_hash: BTreeMap<Hash, Node> = BTreeMap::new();
    let mut cache: BTreeMap<Hash, check::CalleeInfo> = BTreeMap::new();
    let mut out = Vec::with_capacity(assembled.len());
    let mut diags = Vec::new();

    for a in assembled {
        let label = format!("@{}", a.label);
        match check::check_node_with_cache(
            &a.node,
            a.hashes.node,
            &|h| by_hash.get(h).cloned(),
            &mut cache,
        ) {
            Ok(summary) => {
                by_hash.insert(a.hashes.node, a.node);
                out.push((a.label, summary));
            }
            Err(mut node_diags) => {
                for d in &mut node_diags {
                    if d.node.is_none() {
                        d.node = Some(label.clone());
                    }
                }
                diags.extend(node_diags);
                by_hash.insert(a.hashes.node, a.node);
            }
        }
    }

    if diags.is_empty() {
        Ok(out)
    } else {
        Err(diags)
    }
}
