//! Content-addressed object store and index, like .git/objects: canonical .vgir objects under
//! `.vg/objects/`, a SQLite index beside them, and `vg.lock`, which pins every resolved label.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use vg_core::codec;
use vg_core::diag::Diagnostic;
use vg_core::ir::{Hash, Node, hex, parse_hex};

#[derive(Debug)]
pub enum Error {
    Io(PathBuf, std::io::Error),
    Db(rusqlite::Error),
    /// Bytes that are not a canonical node, or not the node their name promises.
    Invalid(Diagnostic),
    Lock(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(p, e) => write!(f, "{}: {e}", p.display()),
            Error::Db(e) => write!(f, "index: {e}"),
            Error::Invalid(d) => write!(f, "{} {}", d.code, d.message),
            Error::Lock(m) => write!(f, "vg.lock: {m}"),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        Error::Db(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

fn io<T>(p: &Path, r: std::io::Result<T>) -> Result<T> {
    r.map_err(|e| Error::Io(p.to_path_buf(), e))
}

/// The store of one project: `<root>/.vg/objects` and `<root>/.vg/index.sqlite`.
pub struct Store {
    objects: PathBuf,
    db: Connection,
}

impl Store {
    pub fn open(root: &Path) -> Result<Store> {
        let objects = root.join(".vg/objects");
        io(&objects, fs::create_dir_all(&objects))?;
        let db = Connection::open(root.join(".vg/index.sqlite"))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS nodes (
                 hash BLOB PRIMARY KEY,
                 spec INTEGER NOT NULL,
                 effect_mask INTEGER NOT NULL,
                 body_hash BLOB NOT NULL,
                 contract_hash BLOB NOT NULL
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS nodes_body ON nodes (body_hash);",
        )?;
        Ok(Store { objects, db })
    }

    fn path(&self, h: &Hash) -> PathBuf {
        let x = hex(h);
        self.objects.join(&x[..2]).join(format!("{}.vgir", &x[2..]))
    }

    /// Stores canonical .vgir bytes under their node hash (a SPEC's under its spec_hash) and
    /// indexes them. Anything that is not canonical is refused.
    pub fn put(&self, bytes: &[u8]) -> Result<Hash> {
        let node = codec::decode(bytes).map_err(Error::Invalid)?;
        let (_, hashes) = codec::encode(&node);
        let path = self.path(&hashes.node);
        if !path.exists() {
            let dir = path.parent().expect("objects/xx");
            io(dir, fs::create_dir_all(dir))?;
            // Write, then rename, so a crash never leaves a partial object under its final name.
            let tmp = path.with_extension("tmp");
            io(&tmp, fs::write(&tmp, bytes))?;
            io(&path, fs::rename(&tmp, &path))?;
        }
        self.db.execute(
            "INSERT OR IGNORE INTO nodes (hash, spec, effect_mask, body_hash, contract_hash) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![&hashes.node[..], node.spec, node.effects.0, &hashes.body[..], &hashes.contract[..]],
        )?;
        Ok(hashes.node)
    }

    /// The node stored under a hash, checked against it.
    pub fn get(&self, h: &Hash) -> Result<Option<Node>> {
        let path = self.path(h);
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Error::Io(path, e)),
        };
        let node = codec::decode(&bytes).map_err(Error::Invalid)?;
        if codec::encode(&node).1.node != *h {
            return Err(Error::Invalid(Diagnostic::error(
                "VG-S0204",
                Default::default(),
                format!("object {} holds another node", path.display()),
            )));
        }
        Ok(Some(node))
    }

    /// Stored nodes with this body_hash: one body under several contracts.
    pub fn with_body(&self, body: &Hash) -> Result<Vec<Hash>> {
        let mut st = self
            .db
            .prepare("SELECT hash FROM nodes WHERE body_hash = ?1 ORDER BY hash")?;
        let rows = st.query_map(params![&body[..]], |r| r.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for r in rows {
            let v = r?;
            out.push(
                v.try_into()
                    .map_err(|_| Error::Lock("index holds a hash that is not 32 bytes".into()))?,
            );
        }
        Ok(out)
    }

    pub fn contains(&self, h: &Hash) -> Result<bool> {
        Ok(self
            .db
            .query_row(
                "SELECT 1 FROM nodes WHERE hash = ?1",
                params![&h[..]],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
}

/// One pinned label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub node: Hash,
    pub spec: Option<Hash>,
    /// `local` or `mesh`.
    pub source: String,
}

/// `vg.lock`: every resolved label (docs/02, "Short hashes and vg.lock").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lock {
    pub resolved: BTreeMap<String, Entry>,
}

impl Lock {
    /// Reads a lock file, or an empty lock when there is none. Only the layout `vg` writes is
    /// accepted: a `[resolved]` table of inline tables, one per line.
    pub fn read(path: &Path) -> Result<Lock> {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Lock::default()),
            Err(e) => return Err(Error::Io(path.to_path_buf(), e)),
        };
        let mut lock = Lock::default();
        let mut in_table = false;
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            let bad = |m: &str| Error::Lock(format!("line {}: {m}", n + 1));
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[resolved]" {
                in_table = true;
                continue;
            }
            if !in_table {
                return Err(bad("expected [resolved]"));
            }
            let (label, rest) = line
                .strip_prefix('"')
                .and_then(|l| l.split_once('"'))
                .ok_or_else(|| bad("expected \"@label\" = { … }"))?;
            let label = label
                .strip_prefix('@')
                .ok_or_else(|| bad("a label starts with @"))?;
            let body = rest
                .trim()
                .strip_prefix('=')
                .map(str::trim)
                .and_then(|r| r.strip_prefix('{'))
                .and_then(|r| r.strip_suffix('}'))
                .ok_or_else(|| bad("expected = { … }"))?;
            let mut fields: BTreeMap<&str, &str> = BTreeMap::new();
            for kv in body.split(',') {
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| bad("expected key = \"value\""))?;
                let v = v
                    .trim()
                    .strip_prefix('"')
                    .and_then(|v| v.strip_suffix('"'))
                    .ok_or_else(|| bad("values are quoted"))?;
                if fields.insert(k.trim(), v).is_some() {
                    return Err(bad("a key appears twice"));
                }
            }
            let hash = |k: &str| -> Result<Option<Hash>> {
                fields
                    .get(k)
                    .map(|v| parse_hex(v).ok_or_else(|| bad(&format!("{k} is not 64 hex digits"))))
                    .transpose()
            };
            let node = hash("node")?.ok_or_else(|| bad("node is missing"))?;
            let spec = hash("spec")?;
            let source = fields
                .get("source")
                .ok_or_else(|| bad("source is missing"))?
                .to_string();
            if fields
                .keys()
                .any(|k| !["node", "spec", "source"].contains(k))
            {
                return Err(bad("unknown key"));
            }
            lock.resolved
                .insert(label.to_string(), Entry { node, spec, source });
        }
        Ok(lock)
    }

    /// The lock file's text: labels sorted, keys in a fixed order.
    pub fn render(&self) -> String {
        let mut out =
            String::from("# Written by vg; pins every resolved label (docs/02).\n[resolved]\n");
        for (label, e) in &self.resolved {
            out.push_str(&format!("\"@{label}\" = {{ node = \"{}\"", hex(&e.node)));
            if let Some(s) = &e.spec {
                out.push_str(&format!(", spec = \"{}\"", hex(s)));
            }
            out.push_str(&format!(", source = \"{}\" }}\n", e.source));
        }
        out
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        io(path, fs::write(path, self.render()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vg-store-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const SRC: &str = "module m;
NODE @inc { SIGNATURE: (x: u8) -> (u8); EFFECTS: []; CONTRACT { POST: result == x + 1; } BODY { %0 = ADD.wrap(x, 1:u8); RETURN(%0); } }
NODE @inc2 { SIGNATURE: (x: u8) -> (u8); EFFECTS: []; CONTRACT { } BODY { %0 = ADD.wrap(1:u8, x); RETURN(%0); } }";

    #[test]
    fn put_get_and_index() {
        let root = tmp("put");
        let s = Store::open(&root).unwrap();
        let a = vg_core::asm_local(SRC).unwrap();
        for x in &a {
            assert_eq!(s.put(&x.bytes).unwrap(), x.hashes.node);
            assert_eq!(s.put(&x.bytes).unwrap(), x.hashes.node);
            assert_eq!(s.get(&x.hashes.node).unwrap().as_ref(), Some(&x.node));
            assert!(s.contains(&x.hashes.node).unwrap());
        }
        // Same body (commutative ADD), different contracts.
        assert_eq!(a[0].hashes.body, a[1].hashes.body);
        let mut both = vec![a[0].hashes.node, a[1].hashes.node];
        both.sort();
        assert_eq!(s.with_body(&a[0].hashes.body).unwrap(), both);
        assert_eq!(s.get(&[7; 32]).unwrap(), None);
        // A tampered object is refused, never returned.
        let path = s.path(&a[0].hashes.node);
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        fs::write(&path, &bytes).unwrap();
        assert!(matches!(s.get(&a[0].hashes.node), Err(Error::Invalid(_))));
        assert!(matches!(s.put(&bytes), Err(Error::Invalid(_))));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lock_round_trip_and_strictness() {
        let root = tmp("lock");
        let path = root.join("vg.lock");
        assert_eq!(Lock::read(&path).unwrap(), Lock::default());
        let mut lock = Lock::default();
        lock.resolved.insert(
            "soft_knee".into(),
            Entry {
                node: [1; 32],
                spec: None,
                source: "local".into(),
            },
        );
        lock.resolved.insert(
            "sha256".into(),
            Entry {
                node: [2; 32],
                spec: Some([3; 32]),
                source: "mesh".into(),
            },
        );
        lock.write(&path).unwrap();
        assert_eq!(Lock::read(&path).unwrap(), lock);
        assert!(lock.render().find("@sha256").unwrap() < lock.render().find("@soft_knee").unwrap());
        fs::write(
            &path,
            "[resolved]\n\"@x\" = { node = \"zz\", source = \"local\" }\n",
        )
        .unwrap();
        assert!(Lock::read(&path).is_err());
        fs::write(&path, "\"@x\" = { node = \"00\" }\n").unwrap();
        assert!(Lock::read(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
