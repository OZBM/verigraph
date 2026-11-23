//! Core data types, docs/09 & docs/15 JSON conversions, and error definitions for VG-Mesh.

use std::fmt;
use std::path::PathBuf;

use serde_json::{Map, Value as J, json};
use vg_core::ir::{Hash, hex, parse_hex};
use vg_core::types::EffectMask;

/// Errors returned by Mesh storage, indexing, query parsing, and log operations.
#[derive(Debug)]
pub enum Error {
    Io(PathBuf, std::io::Error),
    Store(vg_store::Error),
    Db(rusqlite::Error),
    BadQuery(String),
    Log(String),
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(p, e) => write!(f, "{}: {e}", p.display()),
            Error::Store(e) => write!(f, "store: {e}"),
            Error::Db(e) => write!(f, "sqlite: {e}"),
            Error::BadQuery(m) => write!(f, "bad query: {m}"),
            Error::Log(m) => write!(f, "transparency log: {m}"),
            Error::Invalid(m) => write!(f, "invalid: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<vg_store::Error> for Error {
    fn from(e: vg_store::Error) -> Self {
        Error::Store(e)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Error::Db(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// How refinement of a spec class was shown (docs/09).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    Proved,
    Bounded(u64),
    Tested(u64),
}

impl Evidence {
    /// Ordering key: higher is stronger (`proved > bounded(n) > tested(m)`).
    pub fn strength_key(self) -> (u8, u64) {
        match self {
            Evidence::Proved => (3, u64::MAX),
            Evidence::Bounded(n) => (2, n),
            Evidence::Tested(m) => (1, m),
        }
    }

    pub fn meets(self, min: Evidence) -> bool {
        self.strength_key() >= min.strength_key()
    }

    pub fn parse_str(s: &str) -> std::result::Result<Self, String> {
        let s = s.trim();
        if s == "proved" {
            return Ok(Evidence::Proved);
        }
        if let Some(inner) = s.strip_prefix("bounded(").and_then(|r| r.strip_suffix(')')) {
            let n: u64 = inner
                .trim()
                .parse()
                .map_err(|_| format!("invalid bounded(n) evidence: {s}"))?;
            return Ok(Evidence::Bounded(n));
        }
        if let Some(inner) = s.strip_prefix("tested(").and_then(|r| r.strip_suffix(')')) {
            let m: u64 = inner
                .trim()
                .parse()
                .map_err(|_| format!("invalid tested(m) evidence: {s}"))?;
            return Ok(Evidence::Tested(m));
        }
        Err(format!("unknown evidence string: {s}"))
    }
}

impl Ord for Evidence {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.strength_key().cmp(&other.strength_key())
    }
}

impl PartialOrd for Evidence {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Evidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Evidence::Proved => f.write_str("proved"),
            Evidence::Bounded(n) => write!(f, "bounded({n})"),
            Evidence::Tested(m) => write!(f, "tested({m})"),
        }
    }
}

/// P7.1 Spec object: a reference SPEC node and its interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpecRecord {
    pub spec_hash: Hash,
    pub name: Option<String>,
    pub input_types: Vec<String>,
    pub output_types: Vec<String>,
    pub effect_mask: u32,
    pub intent: String,
}

/// P7.1 Proof record object: obligation hash -> verdict, solver, version, optional certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProofRecord {
    pub obligation_hash: Hash,
    pub obligation_id: String,
    pub node_hash: Hash,
    pub verdict: String,
    pub solver: String,
    pub solver_version: String,
    pub certificate: Option<Vec<u8>>,
}

/// P7.1 Attestation object: Ed25519-signed claim that node N refines spec S.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attestation {
    pub node_hash: Hash,
    pub spec_hash: Hash,
    pub checker: String,
    pub evidence: Evidence,
    pub signer: String,
    pub public_key: [u8; 32],
    pub statement: String,
    pub signature: [u8; 64],
}

/// P7.1 Benchmark object: measured target performance for a node.
#[derive(Clone, Debug, PartialEq)]
pub struct Benchmark {
    pub node_hash: Hash,
    pub target: String,
    pub cycles_per_byte: f64,
    pub p50_us: f64,
    pub p99_us: f64,
}

impl Benchmark {
    pub fn to_json(&self) -> J {
        let mut m = Map::new();
        m.insert("cycles_per_byte".into(), json!(self.cycles_per_byte));
        m.insert("node_hash".into(), json!(hex(&self.node_hash)));
        m.insert("p50_us".into(), json!(self.p50_us));
        m.insert("p99_us".into(), json!(self.p99_us));
        m.insert("target".into(), json!(self.target));
        J::Object(m)
    }
}

/// P7.1 Provenance object: lifting origin metadata for a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub node_hash: Hash,
    pub source: String,
    pub commit: String,
    pub compiler: String,
    pub flags: String,
}

/// P7.1 Name object: signed, movable human pointer to a hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameRef {
    pub name: String,
    pub target_hash: Hash,
    pub signer: String,
    pub public_key: [u8; 32],
    pub statement: String,
    pub signature: [u8; 64],
}

/// Contract query shape matching docs/09 (`MeshContractQuery`) and docs/15 (`MeshQueryIn`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MeshContractQuery {
    pub input_types: Vec<String>,
    pub output_types: Vec<String>,
    pub max_effects: Vec<String>,
    pub pre: Option<String>,
    pub post: Option<String>,
    pub max_cost: Option<String>,
    pub intent: Option<String>,
    pub spec: Option<String>,
    pub min_evidence: Option<Evidence>,
    pub target: Option<String>,
    pub limit: Option<usize>,
}

fn str_array_field(obj: &Map<String, J>, key: &str) -> Result<Vec<String>> {
    let Some(v) = obj.get(key) else {
        return Ok(Vec::new());
    };
    let arr = v
        .as_array()
        .ok_or_else(|| Error::BadQuery(format!("{key} must be an array of strings")))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| Error::BadQuery(format!("{key} elements must be strings")))?;
        out.push(s.to_string());
    }
    Ok(out)
}

fn opt_str_field(obj: &Map<String, J>, key: &str) -> Result<Option<String>> {
    let Some(v) = obj.get(key) else {
        return Ok(None);
    };
    if v.is_null() {
        return Ok(None);
    }
    v.as_str()
        .map(|s| Some(s.to_string()))
        .ok_or_else(|| Error::BadQuery(format!("{key} must be a string")))
}

impl MeshContractQuery {
    pub fn from_json(val: &J) -> Result<Self> {
        let obj = val
            .as_object()
            .ok_or_else(|| Error::BadQuery("query must be a JSON object".into()))?;
        let input_types = str_array_field(obj, "input_types")?;
        let output_types = str_array_field(obj, "output_types")?;
        let max_effects = str_array_field(obj, "max_effects")?;
        // Validate effect names eagerly so malformed queries fail with BadQuery.
        parse_effects(&max_effects).map_err(Error::BadQuery)?;

        let pre = opt_str_field(obj, "pre")?;
        let post = opt_str_field(obj, "post")?;
        let max_cost = opt_str_field(obj, "max_cost")?;
        let intent = opt_str_field(obj, "intent")?;
        let spec = opt_str_field(obj, "spec")?;
        let min_evidence = match opt_str_field(obj, "min_evidence")? {
            Some(s) => Some(Evidence::parse_str(&s).map_err(Error::BadQuery)?),
            None => None,
        };
        let target = opt_str_field(obj, "target")?;
        let limit = match obj.get("limit") {
            None | Some(J::Null) => None,
            Some(v) => {
                let n = v.as_u64().ok_or_else(|| {
                    Error::BadQuery("limit must be a non-negative integer".into())
                })?;
                Some(usize::try_from(n).unwrap_or(usize::MAX))
            }
        };

        Ok(Self {
            input_types,
            output_types,
            max_effects,
            pre,
            post,
            max_cost,
            intent,
            spec,
            min_evidence,
            target,
            limit,
        })
    }

    pub fn to_json(&self) -> J {
        let mut m = Map::new();
        m.insert("input_types".into(), json!(self.input_types));
        if let Some(intent) = &self.intent {
            m.insert("intent".into(), json!(intent));
        }
        if let Some(limit) = self.limit {
            m.insert("limit".into(), json!(limit));
        }
        if let Some(max_cost) = &self.max_cost {
            m.insert("max_cost".into(), json!(max_cost));
        }
        m.insert("max_effects".into(), json!(self.max_effects));
        if let Some(ev) = &self.min_evidence {
            m.insert("min_evidence".into(), json!(ev.to_string()));
        }
        m.insert("output_types".into(), json!(self.output_types));
        if let Some(post) = &self.post {
            m.insert("post".into(), json!(post));
        }
        if let Some(pre) = &self.pre {
            m.insert("pre".into(), json!(pre));
        }
        if let Some(spec) = &self.spec {
            m.insert("spec".into(), json!(spec));
        }
        if let Some(target) = &self.target {
            m.insert("target".into(), json!(target));
        }
        J::Object(m)
    }
}

/// One candidate returned by `Mesh::query` (docs/15 `MeshQueryOut.candidates[*]`).
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub node_hash: Hash,
    pub spec_hash: Option<Hash>,
    pub cost: Option<String>,
    pub evidence: String,
    pub benchmarks: Vec<Benchmark>,
}

impl Candidate {
    pub fn to_json(&self) -> J {
        let mut m = Map::new();
        let bms: Vec<J> = self.benchmarks.iter().map(Benchmark::to_json).collect();
        m.insert("benchmarks".into(), J::Array(bms));
        if let Some(cost) = &self.cost {
            m.insert("cost".into(), json!(cost));
        }
        m.insert("evidence".into(), json!(self.evidence));
        m.insert("node_hash".into(), json!(hex(&self.node_hash)));
        if let Some(sh) = &self.spec_hash {
            m.insert("spec_hash".into(), json!(hex(sh)));
        }
        J::Object(m)
    }
}

/// docs/15 `MeshQueryOut`.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshQueryOut {
    pub candidates: Vec<Candidate>,
}

impl MeshQueryOut {
    pub fn to_json(&self) -> J {
        let cands: Vec<J> = self.candidates.iter().map(Candidate::to_json).collect();
        json!({ "candidates": cands })
    }
}

/// Submission payload to `Mesh::publish`.
#[derive(Clone, Debug)]
pub struct Submission {
    pub vgir: Vec<u8>,
    pub claimed_hash: Option<Hash>,
    pub label: Option<String>,
    pub intent: Option<String>,
    pub spec_name: Option<String>,
    pub signer: String,
    pub public_key: [u8; 32],
    pub signature: Option<[u8; 64]>,
    pub provenance: Option<Provenance>,
}

/// One rejected entry in `PublishOut` (docs/15).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RejectedNode {
    pub node: String,
    pub check: u8,
    pub code: &'static str,
    pub reason: String,
}

impl RejectedNode {
    pub fn to_json(&self) -> J {
        let mut m = Map::new();
        m.insert("check".into(), json!(self.check));
        m.insert("code".into(), json!(self.code));
        m.insert("node".into(), json!(self.node));
        m.insert("reason".into(), json!(self.reason));
        J::Object(m)
    }
}

/// docs/15 `PublishOut`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishOut {
    pub accepted: Vec<Hash>,
    pub rejected: Vec<RejectedNode>,
}

impl PublishOut {
    pub fn to_json(&self) -> J {
        let acc: Vec<J> = self.accepted.iter().map(|h| json!(hex(h))).collect();
        let rej: Vec<J> = self.rejected.iter().map(RejectedNode::to_json).collect();
        let mut m = Map::new();
        m.insert("accepted".into(), J::Array(acc));
        m.insert("rejected".into(), J::Array(rej));
        J::Object(m)
    }
}

/// Normalizes one VG type string by stripping whitespace so `"Array<u8, 32>"` and
/// `"Array<u8,32>"` share the same type skeleton key.
pub fn normalize_type_str(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Canonical type skeleton key for fast structural index lookup.
pub fn type_skeleton(inputs: &[String], outputs: &[String]) -> String {
    let ins: Vec<String> = inputs.iter().map(|s| normalize_type_str(s)).collect();
    let outs: Vec<String> = outputs.iter().map(|s| normalize_type_str(s)).collect();
    format!("({})->({})", ins.join(","), outs.join(","))
}

/// Converts a list of effect names (e.g. `["ARENA", "FILE_READ"]`) to a bitmask.
pub fn parse_effects(names: &[String]) -> std::result::Result<u32, String> {
    let mut mask = 0u32;
    for name in names {
        let bit =
            EffectMask::bit(name.trim()).ok_or_else(|| format!("unknown effect name: {name}"))?;
        mask |= bit;
    }
    Ok(mask)
}

/// Parses 64 hex chars into a 32-byte array.
pub fn parse_hash_str(s: &str) -> Option<Hash> {
    parse_hex(s.trim())
}
