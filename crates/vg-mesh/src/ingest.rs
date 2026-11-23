//! P7.4 Ingestion gate: the 6 checks of docs/09 (VG-M0001..VG-M0006), Ed25519 verification,
//! spec-class detection, and append-only C2SP transparency log recording.

use std::collections::BTreeSet;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rusqlite::params;
use vg_core::codec;
use vg_core::ir::{Bin, Builtin, Hash, Node, Term, hex};
use vg_core::types::EffectMask;
use vg_smt::Verdict;

use crate::embed::encode_vec;
use crate::store::{Mesh, sign_attestation, submission_statement, type_names};
use crate::tlog::{format_signed_note, validate_key_name};
use crate::types::{
    Evidence, ProofRecord, Provenance, PublishOut, RejectedNode, Result, Submission, type_skeleton,
};

const EFFECT_DIAG_CODES: &[&str] = &[
    "VG-E0101", "VG-E0104", "VG-E0106", "VG-S0005", "VG-S0009", "VG-S0204",
];

fn reject(node_name: String, check: u8, code: &'static str, reason: String) -> RejectedNode {
    RejectedNode {
        node: node_name,
        check,
        code,
        reason,
    }
}

/// Extracts intent text from a VG-Sym source DEBUG block if `[intent: "..."]` is present.
fn extract_intent_from_debug(debug: Option<&str>) -> Option<String> {
    let src = debug?;
    let idx = src.find("intent:")?;
    let rest = src[idx + "intent:".len()..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => {
                if let Some(esc) = chars.next() {
                    match esc {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        '\\' => out.push('\\'),
                        '"' => out.push('"'),
                        other => out.push(other),
                    }
                }
            }
            other => out.push(other),
        }
    }
    None
}

/// Detects `SPEC` nodes proven to be refined by `node` via `result == spec:S(arg0, ...)`
/// or `same(result, spec:S(arg0, ...))` in `node.contract.post` (P7.4 check 6, docs/09).
fn detect_proved_specs(node: &Node, is_spec_fn: &dyn Fn(&Hash) -> bool) -> Vec<Hash> {
    if node.spec || node.results.len() != 1 {
        return Vec::new();
    }
    let mut specs = BTreeSet::new();
    let is_identity_args = |args: &[Term]| -> bool {
        args.len() == node.params.len()
            && args
                .iter()
                .enumerate()
                .all(|(i, a)| matches!(a, Term::Input(p) if *p as usize == i))
    };
    let is_spec_call = |t: &Term| -> Option<Hash> {
        if let Term::Spec(call_idx, args) = t
            && is_identity_args(args)
            && let Some(&h) = node.calls.get(*call_idx as usize)
            && is_spec_fn(&h)
        {
            return Some(h);
        }
        None
    };

    for conj in &node.contract.post {
        match conj {
            Term::Bin(Bin::Eq, lhs, rhs) => {
                if matches!(lhs.as_ref(), Term::Result(0))
                    && let Some(sh) = is_spec_call(rhs)
                {
                    specs.insert(sh);
                } else if matches!(rhs.as_ref(), Term::Result(0))
                    && let Some(sh) = is_spec_call(lhs)
                {
                    specs.insert(sh);
                }
            }
            Term::Builtin(Builtin::Same, args) if args.len() == 2 => {
                if matches!(args[0], Term::Result(0))
                    && let Some(sh) = is_spec_call(&args[1])
                {
                    specs.insert(sh);
                } else if matches!(args[1], Term::Result(0))
                    && let Some(sh) = is_spec_call(&args[0])
                {
                    specs.insert(sh);
                }
            }
            _ => {}
        }
    }
    specs.into_iter().collect()
}

/// Signs a `.vgir` node submission for `Mesh::publish`.
pub fn sign_submission(vgir: Vec<u8>, signer: &str, sk: &SigningKey) -> Submission {
    let claimed_hash = codec::decode(&vgir).ok().map(|n| codec::encode(&n).1.node);
    let statement_hash = claimed_hash.unwrap_or([0u8; 32]);
    let stmt = submission_statement(&statement_hash, signer);
    let sig = sk.sign(stmt.as_bytes());
    Submission {
        vgir,
        claimed_hash,
        label: None,
        intent: None,
        spec_name: None,
        signer: signer.to_string(),
        public_key: sk.verifying_key().to_bytes(),
        signature: Some(sig.to_bytes()),
        provenance: None,
    }
}

impl Mesh {
    /// Runs the 6-check ingestion gate of docs/09 on one submission.
    pub fn publish(&mut self, sub: Submission) -> PublishOut {
        self.publish_many(vec![sub])
    }

    /// Runs the 6-check ingestion gate on a batch of submissions in order.
    pub fn publish_many(&mut self, subs: Vec<Submission>) -> PublishOut {
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();
        for sub in subs {
            let fallback_name = sub
                .label
                .clone()
                .or_else(|| sub.claimed_hash.map(|h| hex(&h)))
                .unwrap_or_else(|| "<unknown>".into());
            match self.ingest_one(sub, &fallback_name) {
                Ok(h) => accepted.push(h),
                Err(rej) => rejected.push(rej),
            }
        }
        PublishOut { accepted, rejected }
    }

    fn ingest_one(
        &mut self,
        sub: Submission,
        fallback_name: &str,
    ) -> std::result::Result<Hash, RejectedNode> {
        // Check 1 (VG-M0001): decode canonical .vgir and recompute canonical node_hash.
        let node = codec::decode(&sub.vgir).map_err(|d| {
            reject(
                fallback_name.to_string(),
                1,
                "VG-M0001",
                format!("{} {}", d.code, d.message),
            )
        })?;
        let (reencoded, hashes) = codec::encode(&node);
        if reencoded != sub.vgir {
            return Err(reject(
                fallback_name.to_string(),
                1,
                "VG-M0001",
                "re-encoding canonical node produced different bytes".into(),
            ));
        }
        if sub.vgir.len() < 48 || sub.vgir[16..48] != hashes.node {
            return Err(reject(
                fallback_name.to_string(),
                1,
                "VG-M0001",
                "header node_hash does not match recomputed canonical hash".into(),
            ));
        }
        let node_id = sub.label.clone().unwrap_or_else(|| hex(&hashes.node));
        if let Some(claimed) = sub.claimed_hash
            && claimed != hashes.node
        {
            return Err(reject(
                node_id,
                1,
                "VG-M0001",
                format!(
                    "claimed hash {} disagrees with canonical hash {}",
                    hex(&claimed),
                    hex(&hashes.node)
                ),
            ));
        }

        // Check 4 pre-check: effect transitivity against callees already stored in Mesh.
        let effect_transitivity_err = self.check_effect_transitivity(&node);

        let callee_lookup = |h: &Hash| -> Option<Node> { self.store.get(h).ok().flatten() };

        // Check 2 (VG-M0002): validate structure and re-run vg-types.
        // If vg-types fails solely due to a missing callee or understated effect mask already
        // detected by effect transitivity, report Check 4 (VG-M0004).
        if let Err(diags) = vg_types::check_node(&node, &callee_lookup) {
            let only_effect_or_missing = effect_transitivity_err.is_some()
                && !diags.is_empty()
                && diags.iter().all(|d| EFFECT_DIAG_CODES.contains(&d.code));
            if only_effect_or_missing {
                return Err(reject(
                    node_id,
                    4,
                    "VG-M0004",
                    effect_transitivity_err.unwrap(),
                ));
            }
            let msg = diags
                .iter()
                .map(|d| format!("{} {}", d.code, d.message))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(reject(node_id, 2, "VG-M0002", msg));
        }

        // Also summarize costs from vg-types.
        let summary = vg_types::check_node(&node, &callee_lookup).map_err(|diags| {
            let msg = diags
                .iter()
                .map(|d| format!("{} {}", d.code, d.message))
                .collect::<Vec<_>>()
                .join("; ");
            reject(node_id.clone(), 2, "VG-M0002", msg)
        })?;

        // Check 4 (VG-M0004): effect transitivity (every callee in Mesh, callee mask subset of caller).
        if let Some(reason) = effect_transitivity_err {
            return Err(reject(node_id, 4, "VG-M0004", reason));
        }

        // Check 3 (VG-M0003): re-verify every obligation with vg_smt (all must be Proven).
        let verify_label = sub
            .label
            .as_deref()
            .unwrap_or("node")
            .trim_start_matches('@');
        let outcomes = vg_smt::verify(&node, verify_label, &callee_lookup, &self.smt_opts);
        let mut proof_records = Vec::with_capacity(outcomes.len());
        for o in &outcomes {
            if o.verdict != Verdict::Proven {
                let code = o.code.unwrap_or("VG-V0005");
                return Err(reject(
                    node_id,
                    3,
                    "VG-M0003",
                    format!("obligation {} failed ({code}): {}", o.id, o.message),
                ));
            }
            let ob_hash = vg_core::hash::derive(
                vg_core::hash::Context::Obligation,
                format!("{}:{}", hex(&hashes.node), o.id).as_bytes(),
            );
            proof_records.push(ProofRecord {
                obligation_hash: ob_hash,
                obligation_id: o.id.clone(),
                node_hash: hashes.node,
                verdict: "unsat".into(),
                solver: o.solver.clone(),
                solver_version: "z3".into(),
                certificate: None,
            });
        }

        // Check 5 (VG-M0005): verify Ed25519 signature over submission statement.
        if let Err(reason) = validate_key_name(&sub.signer) {
            return Err(reject(node_id, 5, "VG-M0005", reason));
        }
        let sig_bytes = sub.signature.ok_or_else(|| {
            reject(
                node_id.clone(),
                5,
                "VG-M0005",
                "missing Ed25519 signature".into(),
            )
        })?;
        let vk = VerifyingKey::from_bytes(&sub.public_key).map_err(|e| {
            reject(
                node_id.clone(),
                5,
                "VG-M0005",
                format!("invalid Ed25519 public key: {e}"),
            )
        })?;
        let sig = Signature::from_bytes(&sig_bytes);
        let stmt = submission_statement(&hashes.node, &sub.signer);
        vk.verify_strict(stmt.as_bytes(), &sig).map_err(|e| {
            reject(
                node_id.clone(),
                5,
                "VG-M0005",
                format!("Ed25519 signature verification failed: {e}"),
            )
        })?;
        let signed_note = format_signed_note(&stmt, &sub.signer, &vk, &sig);

        // Check 6 (VG-M0006): deduplicate by node_hash (and replayed statements) BEFORE
        // appending to the transparency log!
        if self.index.hash_to_slot.contains_key(&hashes.node)
            || self.index.logged_statements.contains(&signed_note)
        {
            return Err(reject(
                node_id,
                6,
                "VG-M0006",
                format!("duplicate or replayed submission for {}", hex(&hashes.node)),
            ));
        }

        // Detect spec classes refined by this node (`result == spec:S(args)` in POST).
        let is_spec_fn = |h: &Hash| -> bool {
            self.index
                .hash_to_slot
                .get(h)
                .and_then(|&slot| self.index.nodes.get(slot))
                .is_some_and(|n| n.is_spec)
        };
        let proved_specs = detect_proved_specs(&node, &is_spec_fn);

        // Persist canonical .vgir bytes in vg_store.
        let stored_hash = self
            .store
            .put(&sub.vgir)
            .map_err(|e| reject(node_id.clone(), 1, "VG-M0001", format!("store error: {e}")))?;
        debug_assert_eq!(stored_hash, hashes.node);

        // Append signed statement to the C2SP transparency log.
        let (entry_idx, lh, _) = self
            .tlog
            .append(signed_note.as_bytes())
            .map_err(|e| reject(node_id.clone(), 5, "VG-M0005", format!("log append: {e}")))?;

        let input_types = type_names(&node.params);
        let output_types = type_names(&node.results);
        let skel = type_skeleton(&input_types, &output_types);
        let intent = sub
            .intent
            .clone()
            .or_else(|| extract_intent_from_debug(node.debug.as_deref()))
            .unwrap_or_default();
        let cost_expr = format!("ops <= {}", summary.cost_ops);
        let ins_json = serde_json::to_string(&input_types)
            .map_err(|e| reject(node_id.clone(), 2, "VG-M0002", e.to_string()))?;
        let outs_json = serde_json::to_string(&output_types)
            .map_err(|e| reject(node_id.clone(), 2, "VG-M0002", e.to_string()))?;
        let spec_name = if node.spec {
            sub.spec_name
                .clone()
                .or_else(|| sub.label.clone())
                .map(|s| s.trim_start_matches('@').to_string())
        } else {
            None
        };

        let emb_opt = if intent.trim().is_empty() {
            None
        } else {
            Some(self.embedder.embed(&intent))
        };
        let model_id = self.embedder.model_id().to_string();

        // Commit SQLite rows in one transaction.
        self.persist_ingested_node(
            &hashes.node,
            &node,
            &ins_json,
            &outs_json,
            &skel,
            summary.cost_ops,
            &cost_expr,
            &intent,
            spec_name.as_deref(),
            &proved_specs,
            &proof_records,
            sub.provenance.as_ref(),
            &sub.signer,
            &sub.public_key,
            emb_opt.as_deref(),
            &model_id,
            entry_idx,
            &lh,
            signed_note.as_bytes(),
        )
        .map_err(|e| reject(node_id.clone(), 6, "VG-M0006", e.to_string()))?;

        // Update in-memory index.
        self.index.logged_statements.insert(signed_note);
        let slot = self.index.add_node(
            hashes.node,
            node.spec,
            node.effects.0,
            &skel,
            summary.cost_ops,
            cost_expr,
            emb_opt.as_deref(),
        );
        if let Some(name) = spec_name {
            self.index.spec_name_to_hash.insert(name, hashes.node);
        }
        for sh in proved_specs {
            self.index.add_spec_member(sh, slot, Evidence::Proved);
        }

        Ok(hashes.node)
    }

    fn check_effect_transitivity(&self, node: &Node) -> Option<String> {
        for ch in &node.calls {
            match self.store.get(ch) {
                Ok(Some(callee)) => {
                    let extra = callee.effects.0 & !node.effects.0;
                    if extra != 0 {
                        return Some(format!(
                            "callee {} performs effects {:?} not declared in caller",
                            hex(ch),
                            EffectMask(extra).names()
                        ));
                    }
                }
                Ok(None) => {
                    return Some(format!("callee {} is not present in Mesh", hex(ch)));
                }
                Err(e) => {
                    return Some(format!("failed reading callee {}: {e}", hex(ch)));
                }
            }
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn persist_ingested_node(
        &mut self,
        node_hash: &Hash,
        node: &Node,
        ins_json: &str,
        outs_json: &str,
        skel: &str,
        cost_ops: u64,
        cost_expr: &str,
        intent: &str,
        spec_name: Option<&str>,
        proved_specs: &[Hash],
        proof_records: &[ProofRecord],
        provenance: Option<&Provenance>,
        signer: &str,
        public_key: &[u8; 32],
        emb_opt: Option<&[f32]>,
        model_id: &str,
        entry_idx: u64,
        leaf_hash: &Hash,
        log_payload: &[u8],
    ) -> Result<()> {
        let checker_name = self.tlog.key_name().to_string();
        let mut db = self.lock_db()?;
        let tx = db.transaction()?;

        tx.execute(
            "INSERT INTO mesh_nodes
             (node_hash, is_spec, effect_mask, input_types, output_types, type_skeleton, cost_ops, cost_expr, intent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                &node_hash[..],
                node.spec,
                node.effects.0,
                ins_json,
                outs_json,
                skel,
                cost_ops as i64,
                cost_expr,
                intent,
            ],
        )?;

        if node.spec {
            tx.execute(
                "INSERT INTO specs (spec_hash, name, input_types, output_types, effect_mask, intent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(spec_hash) DO UPDATE SET name = COALESCE(excluded.name, specs.name)",
                params![
                    &node_hash[..],
                    spec_name,
                    ins_json,
                    outs_json,
                    node.effects.0,
                    intent,
                ],
            )?;
        }

        for sh in proved_specs {
            tx.execute(
                "INSERT INTO spec_members (spec_hash, node_hash, evidence)
                 VALUES (?1, ?2, 'proved')
                 ON CONFLICT(spec_hash, node_hash) DO UPDATE SET evidence = 'proved'",
                params![&sh[..], &node_hash[..]],
            )?;
            let att = sign_attestation(
                *node_hash,
                *sh,
                &checker_name,
                Evidence::Proved,
                &checker_name,
                &self.checker_sk,
            );
            tx.execute(
                "INSERT OR REPLACE INTO attestations
                 (node_hash, spec_hash, checker, evidence, signer, public_key, statement, signature)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    &att.node_hash[..],
                    &att.spec_hash[..],
                    att.checker,
                    att.evidence.to_string(),
                    att.signer,
                    &att.public_key[..],
                    att.statement,
                    &att.signature[..],
                ],
            )?;
        }

        for pr in proof_records {
            tx.execute(
                "INSERT OR REPLACE INTO proof_records
                 (obligation_hash, obligation_id, node_hash, verdict, solver, solver_version, certificate)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    &pr.obligation_hash[..],
                    pr.obligation_id,
                    &pr.node_hash[..],
                    pr.verdict,
                    pr.solver,
                    pr.solver_version,
                    pr.certificate,
                ],
            )?;
        }

        if let Some(prov) = provenance {
            tx.execute(
                "INSERT OR REPLACE INTO provenance (node_hash, source, commit_rev, compiler, flags)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    &node_hash[..],
                    prov.source,
                    prov.commit,
                    prov.compiler,
                    prov.flags,
                ],
            )?;
        }

        tx.execute(
            "INSERT INTO signers (signer, public_key) VALUES (?1, ?2)
             ON CONFLICT(signer) DO UPDATE SET public_key = excluded.public_key",
            params![signer, &public_key[..]],
        )?;

        if let Some(v) = emb_opt {
            tx.execute(
                "INSERT OR REPLACE INTO embeddings (node_hash, model_id, vec)
                 VALUES (?1, ?2, ?3)",
                params![&node_hash[..], model_id, encode_vec(v)],
            )?;
        }

        tx.execute(
            "INSERT INTO log_entries (entry_index, node_hash, leaf_hash, payload)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                entry_idx as i64,
                &node_hash[..],
                &leaf_hash[..],
                log_payload
            ],
        )?;

        tx.commit()?;
        Ok(())
    }
}
