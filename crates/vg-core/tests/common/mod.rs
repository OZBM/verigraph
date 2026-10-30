//! Helpers shared by the integration tests; each test crate uses some of them.
#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every .vgs file under the given directories of the repository, sorted.
pub fn vgs_files(dirs: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = dirs.iter().map(|d| repo().join(d)).collect();
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "vgs") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

pub fn rel(p: &Path) -> String {
    p.strip_prefix(repo()).unwrap_or(p).display().to_string()
}

pub fn summary(name: &str, text: &str) {
    let dir = repo().join("target/gate-g1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), text).unwrap();
}
