//! Mesh storage (`<dir>/.vg/objects`, `<dir>/mesh.sqlite`, `<dir>/log/`) and in-memory index.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rusqlite::{Connection, OptionalExtension, params};
use vg_core::ir::{Hash, Node, hex, parse_hex};
use vg_core::types::Type;
use vg_store::Store;

use crate::embed::{DefaultEmbedder, Embedder, decode_vec, encode_vec};
use crate::tlog::{Checkpoint, TileLog};
use crate::types::{
    Attestation, Benchmark, Error, Evidence, NameRef, ProofRecord, Provenance, Result, SpecRecord,
    type_skeleton,
};

const DEFAULT_ORIGIN: &str = "vg-mesh.local/v1";
const DEFAULT_KEY_NAME: &str = "vg-mesh-log";

/// One entry in the dense in-memory query index (P7.2 / P7.3).
#[derive(Clone, Debug)]
pub(crate) struct IndexedNode {
    pub node_hash: Hash,
    pub is_spec: bool,
    pub effect_mask: u32,
    pub cost_ops: u64,
    pub cost_expr: String,
    pub primary_spec: Option<Hash>,
    pub best_evidence: Option<Evidence>,
    pub benchmarks: Vec<Benchmark>,
    pub has_embedding: bool,
}

/// In-memory index supporting sub-5ms queries over 100,000+ nodes.
pub(crate) struct MemIndex {
    pub nodes: Vec<IndexedNode>,
    pub hash_to_slot: BTreeMap<Hash, usize>,
    pub by_spec: BTreeMap<Hash, Vec<(usize, Evidence)>>,
    pub spec_name_to_hash: BTreeMap<String, Hash>,
    pub by_skeleton: BTreeMap<String, Vec<usize>>,
    pub embeddings_flat: Vec<f32>,
    pub embed_dim: usize,
    pub behavioral_cache: Mutex<BTreeMap<(Hash, Hash), bool>>,
    pub logged_statements: BTreeSet<String>,
}

impl MemIndex {
    fn new(embed_dim: usize) -> Self {
        Self {
            nodes: Vec::new(),
            hash_to_slot: BTreeMap::new(),
            by_spec: BTreeMap::new(),
            spec_name_to_hash: BTreeMap::new(),
            by_skeleton: BTreeMap::new(),
            embeddings_flat: Vec::new(),
            embed_dim,
            behavioral_cache: Mutex::new(BTreeMap::new()),
            logged_statements: BTreeSet::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn add_node(
        &mut self,
        node_hash: Hash,
        is_spec: bool,
        effect_mask: u32,
        skeleton: &str,
        cost_ops: u64,
        cost_expr: String,
        embedding: Option<&[f32]>,
    ) -> usize {
        if let Some(&slot) = self.hash_to_slot.get(&node_hash) {
            return slot;
        }
        let slot = self.nodes.len();
        self.hash_to_slot.insert(node_hash, slot);
        if !is_spec {
            self.by_skeleton
                .entry(skeleton.to_string())
                .or_default()
                .push(slot);
        }
        let has_embedding = if let Some(vec) = embedding {
            debug_assert_eq!(vec.len(), self.embed_dim);
            self.embeddings_flat.extend_from_slice(vec);
            true
        } else {
            self.embeddings_flat
                .resize(self.embeddings_flat.len() + self.embed_dim, 0.0);
            false
        };
        self.nodes.push(IndexedNode {
            node_hash,
            is_spec,
            effect_mask,
            cost_ops,
            cost_expr,
            primary_spec: None,
            best_evidence: None,
            benchmarks: Vec::new(),
            has_embedding,
        });
        slot
    }

    pub(crate) fn add_spec_member(&mut self, spec_hash: Hash, slot: usize, evidence: Evidence) {
        let members = self.by_spec.entry(spec_hash).or_default();
        if let Some((_, existing)) = members.iter_mut().find(|(s, _)| *s == slot) {
            if evidence > *existing {
                *existing = evidence;
            }
        } else {
            members.push((slot, evidence));
        }
        let n = &mut self.nodes[slot];
        if n.best_evidence.is_none_or(|cur| evidence > cur) {
            n.best_evidence = Some(evidence);
            n.primary_spec = Some(spec_hash);
        }
    }

    pub(crate) fn embedding_slice(&self, slot: usize) -> &[f32] {
        let start = slot * self.embed_dim;
        &self.embeddings_flat[start..start + self.embed_dim]
    }
}

/// Entry for bulk-populating synthetic nodes in benchmarks (P7.6).
#[derive(Clone, Debug)]
pub struct SyntheticEntry {
    pub node_hash: Hash,
    pub is_spec: bool,
    pub effect_mask: u32,
    pub input_types: Vec<String>,
    pub output_types: Vec<String>,
    pub cost_ops: u64,
    pub cost_expr: String,
    pub intent: String,
    pub spec_memberships: Vec<(Hash, Evidence)>,
    pub spec_name: Option<String>,
}

/// Local VG-Mesh registry: content-addressed store, SQLite metadata, in-memory query index,
/// and C2SP transparency log.
pub struct Mesh {
    pub(crate) dir: PathBuf,
    pub(crate) store: Store,
    pub(crate) db: Mutex<Connection>,
    pub(crate) tlog: TileLog,
    pub(crate) embedder: Box<dyn Embedder>,
    pub(crate) smt_opts: vg_smt::Options,
    pub(crate) checker_sk: SigningKey,
    pub(crate) index: MemIndex,
}

impl Mesh {
    pub(crate) fn lock_db(&self) -> Result<MutexGuard<'_, Connection>> {
        self.db
            .lock()
            .map_err(|_| Error::Invalid("sqlite connection lock poisoned".into()))
    }

    /// Opens (or initializes) a Mesh registry at `dir` with the default embedder.
    pub fn open(dir: &Path) -> Result<Self> {
        Self::open_with_embedder(dir, Box::new(DefaultEmbedder::default()))
    }

    /// Opens (or initializes) a Mesh registry at `dir` with a custom `Embedder`.
    pub fn open_with_embedder(dir: &Path, embedder: Box<dyn Embedder>) -> Result<Self> {
        fs::create_dir_all(dir).map_err(|e| Error::Io(dir.to_path_buf(), e))?;
        let store = Store::open(dir)?;
        let db = Connection::open(dir.join("mesh.sqlite"))?;
        init_schema(&db)?;

        let (origin, key_name, seed) = load_or_init_log_meta(&db)?;
        let sk = SigningKey::from_bytes(&seed);

        let entries = load_log_entries(&db)?;
        let leaves: Vec<[u8; 32]> = entries.iter().map(|(_, lh, _)| *lh).collect();
        let tlog = TileLog::open(&dir.join("log"), origin, key_name, sk.clone(), leaves)
            .map_err(Error::Log)?;

        let mut mesh = Self {
            dir: dir.to_path_buf(),
            store,
            db: Mutex::new(db),
            tlog,
            embedder,
            smt_opts: vg_smt::Options::default(),
            checker_sk: sk,
            index: MemIndex::new(0),
        };
        mesh.reload_index()?;
        Ok(mesh)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn node_count(&self) -> usize {
        self.index.nodes.len()
    }

    pub fn set_smt_options(&mut self, opts: vg_smt::Options) {
        self.smt_opts = opts;
    }

    pub fn log_verifying_key(&self) -> VerifyingKey {
        self.tlog.verifying_key()
    }

    pub fn log_key_name(&self) -> &str {
        self.tlog.key_name()
    }

    pub fn log_origin(&self) -> &str {
        self.tlog.origin()
    }

    pub fn log_size(&self) -> u64 {
        self.tlog.size()
    }

    pub fn checkpoint(&self) -> Checkpoint {
        self.tlog.checkpoint()
    }

    pub fn checkpoint_note(&self) -> String {
        self.tlog
            .read_checkpoint()
            .unwrap_or_else(|_| self.tlog.signed_checkpoint())
    }

    pub fn read_tile(&self, rel_path: &str) -> Result<Option<Vec<u8>>> {
        self.tlog.read_tile(rel_path).map_err(Error::Log)
    }

    /// Re-verifies every log entry, leaf hash, level-0 tile, and checkpoint signature on disk.
    pub fn verify_log_integrity(&self) -> Result<Checkpoint> {
        let db = self.lock_db()?;
        let entries = load_log_entries(&db)?;
        self.tlog.verify_integrity(&entries).map_err(Error::Log)
    }

    /// Verifies consistency from a previously saved checkpoint to the current log state.
    pub fn verify_log_consistency(&self, old_cp: &Checkpoint) -> Result<()> {
        self.tlog
            .verify_consistency_from(old_cp)
            .map_err(Error::Log)
    }

    /// Fetches a canonical node from the content-addressed store.
    pub fn get_node(&self, hash: &Hash) -> Result<Option<Node>> {
        Ok(self.store.get(hash)?)
    }

    /// Registers a named Ed25519 public key for future submissions / name updates.
    pub fn register_signer(&mut self, signer: &str, public_key: [u8; 32]) -> Result<()> {
        crate::tlog::validate_key_name(signer).map_err(Error::Invalid)?;
        let _ = VerifyingKey::from_bytes(&public_key)
            .map_err(|e| Error::Invalid(format!("bad Ed25519 public key: {e}")))?;
        self.lock_db()?.execute(
            "INSERT INTO signers (signer, public_key) VALUES (?1, ?2)
             ON CONFLICT(signer) DO UPDATE SET public_key = excluded.public_key",
            params![signer, &public_key[..]],
        )?;
        Ok(())
    }

    pub fn get_signer(&self, signer: &str) -> Result<Option<[u8; 32]>> {
        let row: Option<Vec<u8>> = self
            .lock_db()?
            .query_row(
                "SELECT public_key FROM signers WHERE signer = ?1",
                params![signer],
                |r| r.get(0),
            )
            .optional()?;
        row.map(to_32).transpose()
    }

    /// Records a measured target benchmark for a node already in the Mesh.
    pub fn add_benchmark(&mut self, bm: Benchmark) -> Result<()> {
        let slot = self
            .index
            .hash_to_slot
            .get(&bm.node_hash)
            .copied()
            .ok_or_else(|| Error::Invalid("cannot benchmark unknown node".into()))?;
        self.lock_db()?.execute(
            "INSERT INTO benchmarks (node_hash, target, cycles_per_byte, p50_us, p99_us)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(node_hash, target) DO UPDATE SET
               cycles_per_byte = excluded.cycles_per_byte,
               p50_us = excluded.p50_us,
               p99_us = excluded.p99_us",
            params![
                &bm.node_hash[..],
                bm.target,
                bm.cycles_per_byte,
                bm.p50_us,
                bm.p99_us
            ],
        )?;
        let list = &mut self.index.nodes[slot].benchmarks;
        if let Some(existing) = list.iter_mut().find(|b| b.target == bm.target) {
            *existing = bm;
        } else {
            list.push(bm);
        }
        list.sort_by(|a, b| a.target.cmp(&b.target));
        Ok(())
    }

    /// Records a signed attestation ("node N refines spec S, checked by C").
    pub fn add_attestation(&mut self, att: Attestation) -> Result<()> {
        verify_attestation_sig(&att)?;
        let slot = self
            .index
            .hash_to_slot
            .get(&att.node_hash)
            .copied()
            .ok_or_else(|| Error::Invalid("attested node is not in Mesh".into()))?;
        {
            let db = self.lock_db()?;
            db.execute(
                "INSERT INTO attestations
                 (node_hash, spec_hash, checker, evidence, signer, public_key, statement, signature)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(node_hash, spec_hash, checker, signer) DO UPDATE SET
                   evidence = excluded.evidence,
                   public_key = excluded.public_key,
                   statement = excluded.statement,
                   signature = excluded.signature",
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
            // Also ensure spec_members tracks the strongest evidence if spec is known.
            db.execute(
                "INSERT INTO spec_members (spec_hash, node_hash, evidence)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(spec_hash, node_hash) DO UPDATE SET evidence = excluded.evidence",
                params![
                    &att.spec_hash[..],
                    &att.node_hash[..],
                    att.evidence.to_string()
                ],
            )?;
        }
        self.index
            .add_spec_member(att.spec_hash, slot, att.evidence);
        Ok(())
    }

    pub fn attestations_for(&self, node_hash: &Hash) -> Result<Vec<Attestation>> {
        let db = self.lock_db()?;
        let mut st = db.prepare(
            "SELECT node_hash, spec_hash, checker, evidence, signer, public_key, statement, signature
             FROM attestations WHERE node_hash = ?1 ORDER BY spec_hash, checker, signer",
        )?;
        let rows = st.query_map(params![&node_hash[..]], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, Vec<u8>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Vec<u8>>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, Vec<u8>>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (nh, sh, checker, ev, signer, pk, stmt, sig) = row?;
            out.push(Attestation {
                node_hash: to_32(nh)?,
                spec_hash: to_32(sh)?,
                checker,
                evidence: Evidence::parse_str(&ev).map_err(Error::Invalid)?,
                signer,
                public_key: to_32(pk)?,
                statement: stmt,
                signature: to_64(sig)?,
            });
        }
        Ok(out)
    }

    pub fn proofs_for(&self, node_hash: &Hash) -> Result<Vec<ProofRecord>> {
        let db = self.lock_db()?;
        let mut st = db.prepare(
            "SELECT obligation_hash, obligation_id, node_hash, verdict, solver, solver_version, certificate
             FROM proof_records WHERE node_hash = ?1 ORDER BY obligation_id, obligation_hash",
        )?;
        let rows = st.query_map(params![&node_hash[..]], |r| {
            Ok((
                r.get::<_, Vec<u8>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, Option<Vec<u8>>>(6)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (oh, oid, nh, verdict, solver, ver, cert) = row?;
            out.push(ProofRecord {
                obligation_hash: to_32(oh)?,
                obligation_id: oid,
                node_hash: to_32(nh)?,
                verdict,
                solver,
                solver_version: ver,
                certificate: cert,
            });
        }
        Ok(out)
    }

    pub fn provenance_for(&self, node_hash: &Hash) -> Result<Option<Provenance>> {
        let row = self
            .lock_db()?
            .query_row(
                "SELECT source, commit_rev, compiler, flags FROM provenance WHERE node_hash = ?1",
                params![&node_hash[..]],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        Ok(row.map(|(source, commit, compiler, flags)| Provenance {
            node_hash: *node_hash,
            source,
            commit,
            compiler,
            flags,
        }))
    }

    pub fn spec(&self, spec_hash: &Hash) -> Result<Option<SpecRecord>> {
        let row = self
            .lock_db()?
            .query_row(
                "SELECT name, input_types, output_types, effect_mask, intent FROM specs WHERE spec_hash = ?1",
                params![&spec_hash[..]],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, u32>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(name, ins, outs, effect_mask, intent)| {
            Ok(SpecRecord {
                spec_hash: *spec_hash,
                name,
                input_types: parse_json_str_vec(&ins)?,
                output_types: parse_json_str_vec(&outs)?,
                effect_mask,
                intent,
            })
        })
        .transpose()
    }

    /// Sets or moves a signed human Name pointer (`Name` object of P7.1).
    pub fn set_name(&mut self, nr: NameRef) -> Result<()> {
        verify_name_ref_sig(&nr)?;
        let is_spec_target = {
            let db = self.lock_db()?;
            db.execute(
                "INSERT INTO names (name, target_hash, signer, public_key, statement, signature)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(name) DO UPDATE SET
                   target_hash = excluded.target_hash,
                   signer = excluded.signer,
                   public_key = excluded.public_key,
                   statement = excluded.statement,
                   signature = excluded.signature",
                params![
                    nr.name,
                    &nr.target_hash[..],
                    nr.signer,
                    &nr.public_key[..],
                    nr.statement,
                    &nr.signature[..],
                ],
            )?;
            db.query_row(
                "SELECT 1 FROM specs WHERE spec_hash = ?1",
                params![&nr.target_hash[..]],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        };
        if is_spec_target {
            self.index
                .spec_name_to_hash
                .insert(nr.name.trim_start_matches('@').to_string(), nr.target_hash);
        }
        Ok(())
    }

    pub fn resolve_name(&self, name: &str) -> Result<Option<NameRef>> {
        let row = self
            .lock_db()?
            .query_row(
                "SELECT target_hash, signer, public_key, statement, signature FROM names WHERE name = ?1",
                params![name],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Vec<u8>>(4)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(th, signer, pk, statement, sig)| {
            Ok(NameRef {
                name: name.to_string(),
                target_hash: to_32(th)?,
                signer,
                public_key: to_32(pk)?,
                statement,
                signature: to_64(sig)?,
            })
        })
        .transpose()
    }

    /// Bulk-inserts synthetic entries for benchmarking (P7.6) in a single SQLite transaction
    /// and updates the in-memory index directly.
    pub fn insert_synthetic_batch(&mut self, entries: &[SyntheticEntry]) -> Result<()> {
        let model_id = self.embedder.model_id().to_string();
        let mut db = self
            .db
            .lock()
            .map_err(|_| Error::Invalid("sqlite connection lock poisoned".into()))?;
        let tx = db.transaction()?;
        {
            let mut st_node = tx.prepare(
                "INSERT OR IGNORE INTO mesh_nodes
                 (node_hash, is_spec, effect_mask, input_types, output_types, type_skeleton, cost_ops, cost_expr, intent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            let mut st_spec = tx.prepare(
                "INSERT OR IGNORE INTO specs (spec_hash, name, input_types, output_types, effect_mask, intent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            let mut st_mem = tx.prepare(
                "INSERT OR IGNORE INTO spec_members (spec_hash, node_hash, evidence)
                 VALUES (?1, ?2, ?3)",
            )?;
            let mut st_emb = tx.prepare(
                "INSERT OR IGNORE INTO embeddings (node_hash, model_id, vec)
                 VALUES (?1, ?2, ?3)",
            )?;

            for e in entries {
                if self.index.hash_to_slot.contains_key(&e.node_hash) {
                    continue;
                }
                let ins_json = serde_json::to_string(&e.input_types)
                    .map_err(|err| Error::Invalid(err.to_string()))?;
                let outs_json = serde_json::to_string(&e.output_types)
                    .map_err(|err| Error::Invalid(err.to_string()))?;
                let skel = type_skeleton(&e.input_types, &e.output_types);
                st_node.execute(params![
                    &e.node_hash[..],
                    e.is_spec,
                    e.effect_mask,
                    ins_json,
                    outs_json,
                    skel,
                    e.cost_ops as i64,
                    e.cost_expr,
                    e.intent,
                ])?;
                if e.is_spec {
                    let clean_name = e
                        .spec_name
                        .as_deref()
                        .map(|s| s.trim_start_matches('@').to_string());
                    st_spec.execute(params![
                        &e.node_hash[..],
                        clean_name,
                        ins_json,
                        outs_json,
                        e.effect_mask,
                        e.intent,
                    ])?;
                    if let Some(name) = clean_name {
                        self.index.spec_name_to_hash.insert(name, e.node_hash);
                    }
                }
                let emb_opt = if e.intent.trim().is_empty() {
                    None
                } else {
                    let v = self.embedder.embed(&e.intent);
                    st_emb.execute(params![&e.node_hash[..], model_id, encode_vec(&v)])?;
                    Some(v)
                };
                let slot = self.index.add_node(
                    e.node_hash,
                    e.is_spec,
                    e.effect_mask,
                    &skel,
                    e.cost_ops,
                    e.cost_expr.clone(),
                    emb_opt.as_deref(),
                );
                for (sh, ev) in &e.spec_memberships {
                    st_mem.execute(params![&sh[..], &e.node_hash[..], ev.to_string()])?;
                    self.index.add_spec_member(*sh, slot, *ev);
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn reload_index(&mut self) -> Result<()> {
        let dim = self.embedder.dim();
        let model_id = self.embedder.model_id().to_string();
        let mut idx = MemIndex::new(dim);
        let db = self.lock_db()?;

        // Load embeddings for active model first into map by hash.
        let mut emb_map: BTreeMap<Hash, Vec<f32>> = BTreeMap::new();
        {
            let mut st = db.prepare("SELECT node_hash, vec FROM embeddings WHERE model_id = ?1")?;
            let rows = st.query_map(params![model_id], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?;
            for row in rows {
                let (hb, vb) = row?;
                let v = decode_vec(&vb);
                if v.len() == dim {
                    emb_map.insert(to_32(hb)?, v);
                }
            }
        }

        // Load nodes in deterministic rowid order.
        {
            let mut st = db.prepare(
                "SELECT node_hash, is_spec, effect_mask, type_skeleton, cost_ops, cost_expr
                 FROM mesh_nodes ORDER BY rowid ASC",
            )?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, bool>(1)?,
                    r.get::<_, u32>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?;
            for row in rows {
                let (hb, is_spec, mask, skel, cost_ops, cost_expr) = row?;
                let h = to_32(hb)?;
                let emb = emb_map.get(&h).map(Vec::as_slice);
                idx.add_node(
                    h,
                    is_spec,
                    mask,
                    &skel,
                    cost_ops.max(0) as u64,
                    cost_expr,
                    emb,
                );
            }
        }

        // Load spec names.
        {
            let mut st = db.prepare("SELECT spec_hash, name FROM specs WHERE name IS NOT NULL")?;
            let rows = st.query_map([], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (sh, name) = row?;
                idx.spec_name_to_hash
                    .insert(name.trim_start_matches('@').to_string(), to_32(sh)?);
            }
        }

        // Load named pointers to specs.
        {
            let mut st = db.prepare(
                "SELECT n.name, n.target_hash FROM names n JOIN specs s ON n.target_hash = s.spec_hash",
            )?;
            let rows = st.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?;
            for row in rows {
                let (name, th) = row?;
                idx.spec_name_to_hash
                    .insert(name.trim_start_matches('@').to_string(), to_32(th)?);
            }
        }

        // Load spec_members.
        {
            let mut st = db.prepare("SELECT spec_hash, node_hash, evidence FROM spec_members")?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (sh, nh, ev_str) = row?;
                let sh = to_32(sh)?;
                let nh = to_32(nh)?;
                if let Some(&slot) = idx.hash_to_slot.get(&nh) {
                    let ev = Evidence::parse_str(&ev_str).map_err(Error::Invalid)?;
                    idx.add_spec_member(sh, slot, ev);
                }
            }
        }

        // Load benchmarks.
        {
            let mut st = db.prepare(
                "SELECT node_hash, target, cycles_per_byte, p50_us, p99_us
                 FROM benchmarks ORDER BY target ASC",
            )?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                ))
            })?;
            for row in rows {
                let (nh, target, cpb, p50, p99) = row?;
                let node_hash = to_32(nh)?;
                if let Some(&slot) = idx.hash_to_slot.get(&node_hash) {
                    idx.nodes[slot].benchmarks.push(Benchmark {
                        node_hash,
                        target,
                        cycles_per_byte: cpb,
                        p50_us: p50,
                        p99_us: p99,
                    });
                }
            }
        }

        // Load behavioral cache.
        {
            let mut st =
                db.prepare("SELECT query_hash, candidate_hash, passed FROM behavioral_cache")?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, bool>(2)?,
                ))
            })?;
            let mut cache = idx
                .behavioral_cache
                .lock()
                .map_err(|_| Error::Invalid("behavioral cache lock poisoned".into()))?;
            for row in rows {
                let (qh, ch, passed) = row?;
                cache.insert((to_32(qh)?, to_32(ch)?), passed);
            }
        }

        // Load logged statement strings for replay detection.
        {
            let mut st = db.prepare("SELECT payload FROM log_entries")?;
            let rows = st.query_map([], |r| r.get::<_, Vec<u8>>(0))?;
            for row in rows {
                let bytes = row?;
                if let Ok(s) = String::from_utf8(bytes) {
                    idx.logged_statements.insert(s);
                }
            }
        }

        drop(db);
        self.index = idx;
        Ok(())
    }

    /// Resolves a spec identifier (64-hex hash or `@`-optional name) to its `spec_hash`.
    pub fn resolve_spec_id(&self, spec: &str) -> Option<Hash> {
        let trimmed = spec.trim();
        if let Some(h) = parse_hex(trimmed) {
            return Some(h);
        }
        let clean = trimmed.trim_start_matches('@');
        self.index.spec_name_to_hash.get(clean).copied()
    }
}

/// Canonical statement signed by a submitter when publishing a node.
pub fn submission_statement(node_hash: &Hash, signer: &str) -> String {
    format!(
        "vg-mesh-publish-v1\nnode:{}\nsigner:{signer}\n",
        hex(node_hash)
    )
}

/// Canonical statement signed when setting or moving a `NameRef`.
pub fn name_ref_statement(name: &str, target_hash: &Hash, signer: &str) -> String {
    format!(
        "vg-mesh-name-v1\nname:{name}\ntarget:{}\nsigner:{signer}\n",
        hex(target_hash)
    )
}

/// Canonical statement signed in a refinement `Attestation`.
pub fn attestation_statement(
    node_hash: &Hash,
    spec_hash: &Hash,
    checker: &str,
    evidence: Evidence,
    signer: &str,
) -> String {
    format!(
        "vg-mesh-attestation-v1\nnode:{}\nspec:{}\nchecker:{checker}\nevidence:{evidence}\nsigner:{signer}\n",
        hex(node_hash),
        hex(spec_hash)
    )
}

/// Helper for tests and clients: signs a `NameRef` with `sk`.
pub fn sign_name_ref(name: &str, target_hash: Hash, signer: &str, sk: &SigningKey) -> NameRef {
    let statement = name_ref_statement(name, &target_hash, signer);
    let sig = sk.sign(statement.as_bytes());
    NameRef {
        name: name.to_string(),
        target_hash,
        signer: signer.to_string(),
        public_key: sk.verifying_key().to_bytes(),
        statement,
        signature: sig.to_bytes(),
    }
}

/// Helper for tests and clients: signs an `Attestation` with `sk`.
pub fn sign_attestation(
    node_hash: Hash,
    spec_hash: Hash,
    checker: &str,
    evidence: Evidence,
    signer: &str,
    sk: &SigningKey,
) -> Attestation {
    let statement = attestation_statement(&node_hash, &spec_hash, checker, evidence, signer);
    let sig = sk.sign(statement.as_bytes());
    Attestation {
        node_hash,
        spec_hash,
        checker: checker.to_string(),
        evidence,
        signer: signer.to_string(),
        public_key: sk.verifying_key().to_bytes(),
        statement,
        signature: sig.to_bytes(),
    }
}

pub(crate) fn verify_attestation_sig(att: &Attestation) -> Result<()> {
    let expected = attestation_statement(
        &att.node_hash,
        &att.spec_hash,
        &att.checker,
        att.evidence,
        &att.signer,
    );
    if att.statement != expected {
        return Err(Error::Invalid("attestation statement mismatch".into()));
    }
    let vk = VerifyingKey::from_bytes(&att.public_key)
        .map_err(|e| Error::Invalid(format!("bad attestation public key: {e}")))?;
    let sig = Signature::from_bytes(&att.signature);
    vk.verify_strict(att.statement.as_bytes(), &sig)
        .map_err(|e| Error::Invalid(format!("invalid attestation signature: {e}")))
}

pub(crate) fn verify_name_ref_sig(nr: &NameRef) -> Result<()> {
    let expected = name_ref_statement(&nr.name, &nr.target_hash, &nr.signer);
    if nr.statement != expected {
        return Err(Error::Invalid("name reference statement mismatch".into()));
    }
    let vk = VerifyingKey::from_bytes(&nr.public_key)
        .map_err(|e| Error::Invalid(format!("bad name public key: {e}")))?;
    let sig = Signature::from_bytes(&nr.signature);
    vk.verify_strict(nr.statement.as_bytes(), &sig)
        .map_err(|e| Error::Invalid(format!("invalid name signature: {e}")))
}

pub(crate) fn type_names(tys: &[Type]) -> Vec<String> {
    tys.iter().map(ToString::to_string).collect()
}

fn init_schema(db: &Connection) -> Result<()> {
    db.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;

         CREATE TABLE IF NOT EXISTS mesh_nodes (
             node_hash BLOB PRIMARY KEY,
             is_spec INTEGER NOT NULL,
             effect_mask INTEGER NOT NULL,
             input_types TEXT NOT NULL,
             output_types TEXT NOT NULL,
             type_skeleton TEXT NOT NULL,
             cost_ops INTEGER NOT NULL,
             cost_expr TEXT NOT NULL,
             intent TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_mesh_nodes_skeleton ON mesh_nodes (type_skeleton);

         CREATE TABLE IF NOT EXISTS specs (
             spec_hash BLOB PRIMARY KEY,
             name TEXT,
             input_types TEXT NOT NULL,
             output_types TEXT NOT NULL,
             effect_mask INTEGER NOT NULL,
             intent TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_specs_name ON specs (name);

         CREATE TABLE IF NOT EXISTS spec_members (
             spec_hash BLOB NOT NULL,
             node_hash BLOB NOT NULL,
             evidence TEXT NOT NULL,
             PRIMARY KEY (spec_hash, node_hash)
         ) WITHOUT ROWID;

         CREATE TABLE IF NOT EXISTS proof_records (
             obligation_hash BLOB NOT NULL,
             obligation_id TEXT NOT NULL,
             node_hash BLOB NOT NULL,
             verdict TEXT NOT NULL,
             solver TEXT NOT NULL,
             solver_version TEXT NOT NULL,
             certificate BLOB,
             PRIMARY KEY (node_hash, obligation_id, obligation_hash)
         );

         CREATE TABLE IF NOT EXISTS attestations (
             node_hash BLOB NOT NULL,
             spec_hash BLOB NOT NULL,
             checker TEXT NOT NULL,
             evidence TEXT NOT NULL,
             signer TEXT NOT NULL,
             public_key BLOB NOT NULL,
             statement TEXT NOT NULL,
             signature BLOB NOT NULL,
             PRIMARY KEY (node_hash, spec_hash, checker, signer)
         );

         CREATE TABLE IF NOT EXISTS benchmarks (
             node_hash BLOB NOT NULL,
             target TEXT NOT NULL,
             cycles_per_byte REAL NOT NULL,
             p50_us REAL NOT NULL,
             p99_us REAL NOT NULL,
             PRIMARY KEY (node_hash, target)
         ) WITHOUT ROWID;

         CREATE TABLE IF NOT EXISTS provenance (
             node_hash BLOB PRIMARY KEY,
             source TEXT NOT NULL,
             commit_rev TEXT NOT NULL,
             compiler TEXT NOT NULL,
             flags TEXT NOT NULL
         ) WITHOUT ROWID;

         CREATE TABLE IF NOT EXISTS names (
             name TEXT PRIMARY KEY,
             target_hash BLOB NOT NULL,
             signer TEXT NOT NULL,
             public_key BLOB NOT NULL,
             statement TEXT NOT NULL,
             signature BLOB NOT NULL
         ) WITHOUT ROWID;

         CREATE TABLE IF NOT EXISTS embeddings (
             node_hash BLOB NOT NULL,
             model_id TEXT NOT NULL,
             vec BLOB NOT NULL,
             PRIMARY KEY (node_hash, model_id)
         ) WITHOUT ROWID;

         CREATE TABLE IF NOT EXISTS signers (
             signer TEXT PRIMARY KEY,
             public_key BLOB NOT NULL
         ) WITHOUT ROWID;

         CREATE TABLE IF NOT EXISTS behavioral_cache (
             query_hash BLOB NOT NULL,
             candidate_hash BLOB NOT NULL,
             passed INTEGER NOT NULL,
             PRIMARY KEY (query_hash, candidate_hash)
         ) WITHOUT ROWID;

         CREATE TABLE IF NOT EXISTS log_entries (
             entry_index INTEGER PRIMARY KEY,
             node_hash BLOB NOT NULL,
             leaf_hash BLOB NOT NULL,
             payload BLOB NOT NULL
         );

         CREATE TABLE IF NOT EXISTS log_meta (
             k TEXT PRIMARY KEY,
             v BLOB NOT NULL
         ) WITHOUT ROWID;",
    )?;
    Ok(())
}

fn load_or_init_log_meta(db: &Connection) -> Result<(String, String, [u8; 32])> {
    let get = |k: &str| -> Result<Option<Vec<u8>>> {
        Ok(db
            .query_row("SELECT v FROM log_meta WHERE k = ?1", params![k], |r| {
                r.get(0)
            })
            .optional()?)
    };
    let origin = match get("origin")? {
        Some(v) => String::from_utf8(v).map_err(|e| Error::Log(e.to_string()))?,
        None => {
            db.execute(
                "INSERT INTO log_meta (k, v) VALUES ('origin', ?1)",
                params![DEFAULT_ORIGIN.as_bytes()],
            )?;
            DEFAULT_ORIGIN.to_string()
        }
    };
    let key_name = match get("key_name")? {
        Some(v) => String::from_utf8(v).map_err(|e| Error::Log(e.to_string()))?,
        None => {
            db.execute(
                "INSERT INTO log_meta (k, v) VALUES ('key_name', ?1)",
                params![DEFAULT_KEY_NAME.as_bytes()],
            )?;
            DEFAULT_KEY_NAME.to_string()
        }
    };
    let seed = match get("signing_seed")? {
        Some(v) => to_32(v)?,
        None => {
            // Each registry gets its own log key, from the operating system's generator: a key
            // shared by every registry would let anyone sign checkpoints for any of them. Ed25519
            // signatures stay deterministic for a given key.
            let seed = fresh_seed()?;
            db.execute(
                "INSERT INTO log_meta (k, v) VALUES ('signing_seed', ?1)",
                params![&seed[..]],
            )?;
            seed
        }
    };
    Ok((origin, key_name, seed))
}

/// 32 bytes from the operating system's random number generator.
fn fresh_seed() -> Result<[u8; 32]> {
    use std::io::Read;
    let path = Path::new("/dev/urandom");
    let mut seed = [0u8; 32];
    fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut seed))
        .map_err(|e| Error::Io(path.to_path_buf(), e))?;
    Ok(seed)
}

type LogEntryRow = (u64, [u8; 32], Vec<u8>);

fn load_log_entries(db: &Connection) -> Result<Vec<LogEntryRow>> {
    let mut st = db.prepare(
        "SELECT entry_index, leaf_hash, payload FROM log_entries ORDER BY entry_index ASC",
    )?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, Vec<u8>>(1)?,
            r.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (idx, lh, payload) = row?;
        if idx < 0 {
            return Err(Error::Log("negative log entry index".into()));
        }
        out.push((idx as u64, to_32(lh)?, payload));
    }
    Ok(out)
}

fn parse_json_str_vec(s: &str) -> Result<Vec<String>> {
    let v: serde_json::Value =
        serde_json::from_str(s).map_err(|e| Error::Invalid(format!("bad json string vec: {e}")))?;
    let arr = v
        .as_array()
        .ok_or_else(|| Error::Invalid("expected json array".into()))?;
    arr.iter()
        .map(|x| {
            x.as_str()
                .map(ToString::to_string)
                .ok_or_else(|| Error::Invalid("expected json string element".into()))
        })
        .collect()
}

pub(crate) fn to_32(v: Vec<u8>) -> Result<[u8; 32]> {
    v.try_into()
        .map_err(|_| Error::Invalid("expected 32-byte blob".into()))
}

pub(crate) fn to_64(v: Vec<u8>) -> Result<[u8; 64]> {
    v.try_into()
        .map_err(|_| Error::Invalid("expected 64-byte blob".into()))
}
