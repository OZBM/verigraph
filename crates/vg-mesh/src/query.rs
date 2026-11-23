//! P7.3 Query pipeline:
//! 1. Exact spec match
//! 2. Structural filter (type skeleton + `(node_mask & !allowed) == 0`)
//! 3. Semantic rank (cosine similarity over intent embeddings)
//! 4. Behavioral subtyping check (`PRE_q => PRE_c` and `PRE_q /\ POST_c => POST_q` via `vg-smt`)
//! 5. Rank by verified cost, then measured benchmarks on the caller's target.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

use rusqlite::params;
use vg_core::ir::{Hash, Node, hex};
use vg_core::types::EffectMask;
use vg_smt::Verdict;

use crate::embed::dot_product;
use crate::store::{IndexedNode, Mesh};
use crate::types::{
    Benchmark, Candidate, Error, Evidence, MeshContractQuery, Result, parse_effects, type_skeleton,
};

const DEFAULT_LIMIT: usize = 16;
const BEHAVIORAL_CHECK_CAP: usize = 32;

/// Entry in a bounded min-heap for fast top-K semantic selection (P7.3 step 3).
#[derive(Clone, Copy)]
struct HeapItem {
    score_bits: u32,
    cost_ops: u64,
    slot: usize,
    node_hash: Hash,
}

impl HeapItem {
    fn score(self) -> f32 {
        f32::from_bits(self.score_bits)
    }

    fn better_than(&self, other: &Self) -> bool {
        // Primary: higher cosine score; secondary: lower cost_ops; tie-breaker: smaller hash.
        match other
            .score()
            .total_cmp(&self.score())
            .then_with(|| self.cost_ops.cmp(&other.cost_ops))
            .then_with(|| self.node_hash.cmp(&other.node_hash))
        {
            Ordering::Less => true,
            Ordering::Greater | Ordering::Equal => false,
        }
    }
}

impl PartialEq for HeapItem {
    fn eq(&self, other: &Self) -> bool {
        self.slot == other.slot
    }
}

impl Eq for HeapItem {}

/// Reverse ordering for `BinaryHeap` so `peek()` is always the WORST element in the top-K set.
impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.better_than(other) {
            Ordering::Less
        } else if other.better_than(self) {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }
}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Parses simple constant `max_cost` filter strings such as `"ops <= 64"`.
fn parse_constant_max_ops(max_cost: Option<&str>) -> Option<u64> {
    let s = max_cost?.trim();
    let rhs = s.strip_prefix("ops")?.trim().strip_prefix("<=")?.trim();
    rhs.parse::<u64>().ok()
}

fn target_benchmark_key(n: &IndexedNode, target: Option<&str>) -> (u8, u64, u64) {
    let bm = match target {
        Some(t) => n.benchmarks.iter().find(|b| b.target == t),
        None => n.benchmarks.first(),
    };
    match bm {
        Some(b) => (0, b.p99_us.to_bits(), b.cycles_per_byte.to_bits()),
        None => (1, u64::MAX, u64::MAX),
    }
}

/// Extracts `spec:<name>` identifiers referenced in a contract formula string.
fn extract_spec_refs(formula: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = formula;
    while let Some(idx) = rest.find("spec:") {
        let after = &rest[idx + "spec:".len()..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.insert(name);
        }
        rest = after;
    }
    out
}

fn is_trivial_true(s: Option<&str>) -> bool {
    match s {
        None => true,
        Some(raw) => {
            let t = raw.trim();
            t.is_empty() || t == "true"
        }
    }
}

fn query_cache_hash(q: &MeshContractQuery) -> Hash {
    let mut buf = Vec::new();
    for t in &q.input_types {
        buf.extend_from_slice(t.as_bytes());
        buf.push(0);
    }
    buf.push(1);
    for t in &q.output_types {
        buf.extend_from_slice(t.as_bytes());
        buf.push(0);
    }
    buf.push(2);
    buf.extend_from_slice(q.pre.as_deref().unwrap_or("true").as_bytes());
    buf.push(3);
    buf.extend_from_slice(q.post.as_deref().unwrap_or("true").as_bytes());
    vg_core::hash::derive(vg_core::hash::Context::Obligation, &buf)
}

impl Mesh {
    /// Fast in-memory query pipeline (steps 1-3 + cost/benchmark ranking).
    /// Meets the 5 ms p99 latency target on 100,000 nodes; never invokes Z3.
    pub fn query_indexed(&self, q: &MeshContractQuery, limit: usize) -> Result<Vec<Candidate>> {
        let limit = if limit == 0 {
            q.limit.unwrap_or(DEFAULT_LIMIT)
        } else {
            limit
        };
        if limit == 0 {
            return Ok(Vec::new());
        }
        let ranked = self.select_indexed_slots(q, limit)?;
        Ok(ranked
            .into_iter()
            .map(|(slot, ev_override, _sim)| self.slot_to_candidate(slot, ev_override))
            .collect())
    }

    /// Full 5-step Mesh query pipeline (docs/09): exact spec, structural filter, semantic rank,
    /// behavioral subtyping via `vg-smt` (with persistent cache), and cost/benchmark ranking.
    pub fn query(&self, q: &MeshContractQuery, limit: usize) -> Result<Vec<Candidate>> {
        let limit = if limit == 0 {
            q.limit.unwrap_or(DEFAULT_LIMIT)
        } else {
            limit
        };
        if limit == 0 {
            return Ok(Vec::new());
        }

        let needs_behavioral =
            !(is_trivial_true(q.pre.as_deref()) && is_trivial_true(q.post.as_deref()));
        let pre_limit = if needs_behavioral {
            limit.saturating_mul(4).max(BEHAVIORAL_CHECK_CAP)
        } else {
            limit
        };

        let shortlist = self.select_indexed_slots(q, pre_limit)?;
        if shortlist.is_empty() {
            return Ok(Vec::new());
        }

        let mut verified_slots: Vec<(usize, Option<Evidence>, f32)> = Vec::new();
        if needs_behavioral {
            let qhash = query_cache_hash(q);
            for (slot, ev_override, sim) in shortlist {
                let cand_hash = self.index.nodes[slot].node_hash;
                if self.check_behavioral_cached(qhash, cand_hash, q)? {
                    verified_slots.push((slot, ev_override, sim));
                }
            }
        } else {
            verified_slots = shortlist;
        }

        // Step 5: rank by verified cost (ops), then target benchmarks, then semantic similarity,
        // then node_hash for complete determinism.
        let target = q.target.as_deref();
        verified_slots.sort_by(|&(sa, _, sim_a), &(sb, _, sim_b)| {
            let na = &self.index.nodes[sa];
            let nb = &self.index.nodes[sb];
            na.cost_ops
                .cmp(&nb.cost_ops)
                .then_with(|| {
                    target_benchmark_key(na, target).cmp(&target_benchmark_key(nb, target))
                })
                .then_with(|| sim_b.total_cmp(&sim_a))
                .then_with(|| na.node_hash.cmp(&nb.node_hash))
        });
        verified_slots.truncate(limit);

        Ok(verified_slots
            .into_iter()
            .map(|(slot, ev_override, _)| {
                let mut c = self.slot_to_candidate(slot, ev_override);
                if needs_behavioral && c.evidence == "none" {
                    c.evidence = "proved".into();
                }
                c
            })
            .collect())
    }

    fn slot_to_candidate(&self, slot: usize, ev_override: Option<Evidence>) -> Candidate {
        let n = &self.index.nodes[slot];
        let evidence = ev_override
            .or(n.best_evidence)
            .map(|e| e.to_string())
            .unwrap_or_else(|| "none".into());
        let benchmarks: Vec<Benchmark> = n.benchmarks.clone();
        Candidate {
            node_hash: n.node_hash,
            spec_hash: n.primary_spec,
            cost: Some(n.cost_expr.clone()),
            evidence,
            benchmarks,
        }
    }

    /// Executes steps 1-3 purely in memory and returns up to `limit` `(slot, spec_evidence, sim)` tuples.
    fn select_indexed_slots(
        &self,
        q: &MeshContractQuery,
        limit: usize,
    ) -> Result<Vec<(usize, Option<Evidence>, f32)>> {
        let allowed_mask = parse_effects(&q.max_effects).map_err(Error::BadQuery)?;
        let max_const_ops = parse_constant_max_ops(q.max_cost.as_deref());
        let min_ev = q.min_evidence;

        let intent_vec = q
            .intent
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| self.embedder.embed(s));

        // Step 1: Exact spec match.
        if let Some(spec_str) = q.spec.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let Some(spec_hash) = self.resolve_spec_id(spec_str) else {
                return Ok(Vec::new());
            };
            let Some(members) = self.index.by_spec.get(&spec_hash) else {
                return Ok(Vec::new());
            };
            let mut matched: Vec<(usize, Option<Evidence>, f32)> = Vec::new();
            for &(slot, ev) in members {
                let n = &self.index.nodes[slot];
                if (n.effect_mask & !allowed_mask) != 0 {
                    continue;
                }
                if let Some(min) = min_ev
                    && !ev.meets(min)
                {
                    continue;
                }
                if let Some(cap) = max_const_ops
                    && n.cost_ops > cap
                {
                    continue;
                }
                let sim = match (&intent_vec, n.has_embedding) {
                    (Some(qv), true) => dot_product(qv, self.index.embedding_slice(slot)),
                    _ => 0.0,
                };
                matched.push((slot, Some(ev), sim));
            }
            let target = q.target.as_deref();
            matched.sort_by(|&(sa, eva, sim_a), &(sb, evb, sim_b)| {
                let na = &self.index.nodes[sa];
                let nb = &self.index.nodes[sb];
                evb.cmp(&eva)
                    .then_with(|| na.cost_ops.cmp(&nb.cost_ops))
                    .then_with(|| {
                        target_benchmark_key(na, target).cmp(&target_benchmark_key(nb, target))
                    })
                    .then_with(|| sim_b.total_cmp(&sim_a))
                    .then_with(|| na.node_hash.cmp(&nb.node_hash))
            });
            matched.truncate(limit);
            return Ok(matched);
        }

        // Step 2: Structural filter via type skeleton + `(node_mask & !allowed) == 0`.
        let has_sig_filter = !q.input_types.is_empty() || !q.output_types.is_empty();
        let skeleton_slots: Option<&[usize]> = if has_sig_filter {
            let skel = type_skeleton(&q.input_types, &q.output_types);
            match self.index.by_skeleton.get(&skel) {
                Some(v) => Some(v.as_slice()),
                None => return Ok(Vec::new()),
            }
        } else {
            None
        };

        let passes = |slot: usize| -> bool {
            let n = &self.index.nodes[slot];
            if n.is_spec {
                return false;
            }
            if (n.effect_mask & !allowed_mask) != 0 {
                return false;
            }
            if let Some(min) = min_ev
                && !n.best_evidence.is_some_and(|e| e.meets(min))
            {
                return false;
            }
            if let Some(cap) = max_const_ops
                && n.cost_ops > cap
            {
                return false;
            }
            true
        };

        // Step 3: Semantic rank (or deterministic cost/benchmark rank when no intent is given).
        if let Some(ref qv) = intent_vec {
            let mut heap: BinaryHeap<HeapItem> = BinaryHeap::with_capacity(limit.saturating_add(1));
            let mut push_slot = |slot: usize| {
                if !passes(slot) {
                    return;
                }
                let n = &self.index.nodes[slot];
                let sim = if n.has_embedding {
                    dot_product(qv, self.index.embedding_slice(slot))
                } else {
                    0.0
                };
                let item = HeapItem {
                    score_bits: sim.to_bits(),
                    cost_ops: n.cost_ops,
                    slot,
                    node_hash: n.node_hash,
                };
                if heap.len() < limit {
                    heap.push(item);
                } else if let Some(worst) = heap.peek()
                    && item.better_than(worst)
                {
                    heap.pop();
                    heap.push(item);
                }
            };

            if let Some(slots) = skeleton_slots {
                for &slot in slots {
                    push_slot(slot);
                }
            } else {
                for slot in 0..self.index.nodes.len() {
                    push_slot(slot);
                }
            }

            let mut items: Vec<HeapItem> = heap.into_vec();
            items.sort_by(|a, b| {
                b.score()
                    .total_cmp(&a.score())
                    .then_with(|| a.cost_ops.cmp(&b.cost_ops))
                    .then_with(|| a.node_hash.cmp(&b.node_hash))
            });
            return Ok(items
                .into_iter()
                .map(|it| (it.slot, None, it.score()))
                .collect());
        }

        // No intent: collect structural matches, sort by cost then benchmarks.
        let mut out: Vec<usize> = Vec::new();
        if let Some(slots) = skeleton_slots {
            for &slot in slots {
                if passes(slot) {
                    out.push(slot);
                }
            }
        } else {
            for slot in 0..self.index.nodes.len() {
                if passes(slot) {
                    out.push(slot);
                }
            }
        }
        let target = q.target.as_deref();
        out.sort_by(|&sa, &sb| {
            let na = &self.index.nodes[sa];
            let nb = &self.index.nodes[sb];
            na.cost_ops
                .cmp(&nb.cost_ops)
                .then_with(|| {
                    target_benchmark_key(na, target).cmp(&target_benchmark_key(nb, target))
                })
                .then_with(|| na.node_hash.cmp(&nb.node_hash))
        });
        out.truncate(limit);
        Ok(out.into_iter().map(|slot| (slot, None, 0.0)).collect())
    }

    fn check_behavioral_cached(
        &self,
        query_hash: Hash,
        cand_hash: Hash,
        q: &MeshContractQuery,
    ) -> Result<bool> {
        {
            let cache = self
                .index
                .behavioral_cache
                .lock()
                .map_err(|_| Error::Invalid("behavioral cache lock poisoned".into()))?;
            if let Some(&cached) = cache.get(&(query_hash, cand_hash)) {
                return Ok(cached);
            }
        }
        let passed = self.run_behavioral_check(cand_hash, q);
        {
            let db = self.lock_db()?;
            db.execute(
                "INSERT OR REPLACE INTO behavioral_cache (query_hash, candidate_hash, passed)
                 VALUES (?1, ?2, ?3)",
                params![&query_hash[..], &cand_hash[..], passed],
            )?;
        }
        let mut cache = self
            .index
            .behavioral_cache
            .lock()
            .map_err(|_| Error::Invalid("behavioral cache lock poisoned".into()))?;
        cache.insert((query_hash, cand_hash), passed);
        Ok(passed)
    }

    /// Step 4: builds a VG-Sym wrapper node with the query's signature, PRE and POST whose body is
    /// a single `CALL_HASH` of the candidate, and runs `vg_smt::verify` on it.
    fn run_behavioral_check(&self, cand_hash: Hash, q: &MeshContractQuery) -> bool {
        let Ok(Some(cand_node)) = self.store.get(&cand_hash) else {
            return false;
        };
        if cand_node.spec {
            return false;
        }
        let Ok(wrapper_src) = self.build_behavioral_wrapper(&cand_node, q) else {
            return false;
        };

        let mut spec_map: BTreeMap<String, Hash> = BTreeMap::new();
        for text in q.pre.iter().chain(q.post.iter()) {
            for name in extract_spec_refs(text) {
                if let Some(sh) = self.resolve_spec_id(&name) {
                    spec_map.insert(name, sh);
                } else {
                    return false;
                }
            }
        }

        let mesh_lookup = |label: &str| -> Option<(Hash, Option<Hash>)> {
            let clean = label.trim_start_matches('@');
            if clean == "cand" {
                return Some((cand_hash, None));
            }
            spec_map.get(clean).map(|&sh| (sh, Some(sh)))
        };

        let Ok(assembled) = vg_core::asm(&wrapper_src, &mesh_lookup, false) else {
            return false;
        };
        let Some(wrapper) = assembled.first() else {
            return false;
        };

        let callee_lookup = |h: &Hash| -> Option<Node> {
            if *h == cand_hash {
                Some(cand_node.clone())
            } else {
                self.store.get(h).ok().flatten()
            }
        };

        let outcomes = vg_smt::verify(
            &wrapper.node,
            &wrapper.label,
            &callee_lookup,
            &self.smt_opts,
        );
        !outcomes.is_empty() && outcomes.iter().all(|o| o.verdict == Verdict::Proven)
    }

    fn build_behavioral_wrapper(
        &self,
        cand: &Node,
        q: &MeshContractQuery,
    ) -> std::result::Result<String, String> {
        let param_tys: Vec<String> = cand.params.iter().map(ToString::to_string).collect();
        let result_tys: Vec<String> = cand.results.iter().map(ToString::to_string).collect();
        let effects_str = EffectMask(cand.effects.0).names().join(", ");

        let mut src = String::from("module mesh_query_check;\n");

        // Declare @MESH_QUERY entries for any `spec:<name>` referenced in query PRE/POST.
        let mut spec_names = BTreeSet::new();
        for text in q.pre.iter().chain(q.post.iter()) {
            spec_names.extend(extract_spec_refs(text));
        }
        for sname in spec_names {
            let sh = self
                .resolve_spec_id(&sname)
                .ok_or_else(|| format!("unknown spec:{sname}"))?;
            let snode = self
                .store
                .get(&sh)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("missing spec {}", hex(&sh)))?;
            let s_ins: Vec<String> = snode.params.iter().map(ToString::to_string).collect();
            let s_outs: Vec<String> = snode.results.iter().map(ToString::to_string).collect();
            src.push_str(&format!(
                "@MESH_QUERY @{sname} {{\n  signature: ({}) -> ({});\n  effects: [];\n  post: true;\n}}\n",
                s_ins.join(", "),
                s_outs.join(", ")
            ));
        }

        src.push_str(&format!(
            "@MESH_QUERY @cand {{\n  signature: ({}) -> ({});\n  effects: [{effects_str}];\n  post: true;\n}}\n",
            param_tys.join(", "),
            result_tys.join(", "),
        ));

        let sig_params: Vec<String> = param_tys
            .iter()
            .enumerate()
            .map(|(i, ty)| format!("arg{i}: {ty}"))
            .collect();
        let arg_names: Vec<String> = (0..param_tys.len()).map(|i| format!("arg{i}")).collect();
        let call_args = if arg_names.is_empty() {
            "@cand".to_string()
        } else {
            format!("@cand, {}", arg_names.join(", "))
        };
        let res_ports: Vec<String> = (0..result_tys.len()).map(|i| format!("%{i}")).collect();
        let bindings = res_ports.join(", ");

        let pre = q
            .pre
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("true");
        let post = q
            .post
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("true");

        src.push_str(&format!(
            "NODE @query_wrapper {{\n  SIGNATURE: ({}) -> ({});\n  EFFECTS: [{effects_str}];\n  CONTRACT {{\n    PRE: {pre};\n    POST: {post};\n  }}\n  BODY {{\n    {bindings} = CALL_HASH({call_args});\n    RETURN({bindings});\n  }}\n}}\n",
            sig_params.join(", "),
            result_tys.join(", "),
        ));

        Ok(src)
    }
}
