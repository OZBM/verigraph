//! The query cache of docs/05 (P3.5): one proof record per obligation, keyed by the obligation
//! hash of docs/02 (BLAKE3, "VeriGraph 2026 obligation v2", over the printed query), so an
//! unchanged obligation is not solved twice. Only proofs are recorded; a refutation needs a model.
//! Records are trusted like the build directory: delete `.vg/proofs` to prove everything again.

use std::path::{Path, PathBuf};

use serde_json::{Value as J, json};
use vg_core::ir::{Hash, hex};

use crate::solver::SmtQuery;

/// The obligation hash of a query: its declarations and assertions, without the solver-specific
/// logic, option and check lines, so every solver's record of one obligation shares a key.
pub(crate) fn obligation_hash(q: &SmtQuery) -> Hash {
    vg_core::hash::derive(vg_core::hash::Context::Obligation, q.body().as_bytes())
}

/// A docs/05 proof record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub id: String,
    pub node: String,
    /// `z3`, or `z3+cvc5` when two solvers agreed.
    pub solver: String,
    /// Each named solver's `--version` line, in the same order, joined by "; ".
    pub version: String,
    /// `unsat`, or `sat` for a NONVACUOUS check.
    pub verdict: String,
    pub time_ms: u64,
    /// `re-solved` or `two-solvers`.
    pub evidence: String,
}

fn path(dir: &Path, key: &Hash) -> PathBuf {
    dir.join(format!("{}.json", hex(key)))
}

pub(crate) fn load(dir: &Path, key: &Hash) -> Option<Record> {
    let j: J = serde_json::from_slice(&std::fs::read(path(dir, key)).ok()?).ok()?;
    let s = |k: &str| j[k].as_str().map(str::to_string);
    (s("obligation")? == hex(key)).then_some(())?;
    Some(Record {
        id: s("id")?,
        node: s("node")?,
        solver: s("solver")?,
        version: s("version")?,
        verdict: s("verdict")?,
        time_ms: j["time_ms"].as_u64()?,
        evidence: s("evidence")?,
    })
}

/// Writes the record whole or not at all (a temporary file renamed into place), so a reader never
/// sees half of one. A cache that cannot be written only costs a re-solve, so errors are ignored.
pub(crate) fn store(dir: &Path, key: &Hash, r: &Record) {
    let body = json!({
        "obligation": hex(key), "id": r.id, "node": r.node, "solver": r.solver,
        "version": r.version, "verdict": r.verdict, "time_ms": r.time_ms, "evidence": r.evidence,
    });
    let final_path = path(dir, key);
    let tmp = dir.join(format!(".{}.{}.tmp", hex(key), std::process::id()));
    let _ = std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(&tmp, format!("{body:#}\n")))
        .and_then(|()| std::fs::rename(&tmp, &final_path));
    let _ = std::fs::remove_file(&tmp);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip_and_reject_a_wrong_key() {
        let dir = std::env::temp_dir().join(format!("vg-cache-test-{}", std::process::id()));
        let mut q = SmtQuery::default();
        q.decls.insert("x".into(), "(_ BitVec 8)".into());
        let key = obligation_hash(&q);
        let r = Record {
            id: "@f/POST.0".into(),
            node: "ab".into(),
            solver: "z3".into(),
            version: "Z3 version 5.4.1".into(),
            verdict: "unsat".into(),
            time_ms: 3,
            evidence: "re-solved".into(),
        };
        store(&dir, &key, &r);
        assert_eq!(load(&dir, &key), Some(r));
        // A record filed under another key is not taken for this one.
        std::fs::rename(path(&dir, &key), path(&dir, &[7; 32])).unwrap();
        assert_eq!(load(&dir, &[7; 32]), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
