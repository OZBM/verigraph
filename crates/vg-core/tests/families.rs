//! Alpha families must hash identically and distinct families must hash differently (docs/16).
//! Each alpha family also gets at least 10 seeded random variants per run, built by random sequences
//! of the equivalence moves of docs/13 (vg_core::generate); the seed is logged in target/gate-g1/alpha.txt.

mod common;

use std::fs;
use std::path::PathBuf;

use common::{repo, summary, vgs_files};
use vg_core::generate as moves;
use vg_core::ir::{Hash, hex};
use vg_core::{asm_local, parser, printer};

/// The node hashes of a module, sorted: equivalent modules hold the same nodes, whatever their
/// labels and order.
fn hashes(src: &str, what: &str) -> Vec<Hash> {
    let mut v: Vec<Hash> = asm_local(src)
        .unwrap_or_else(|e| panic!("{what}: {e}"))
        .iter()
        .map(|a| a.hashes.node)
        .collect();
    v.sort();
    v
}

fn families(kind: &str) -> Vec<(String, Vec<PathBuf>)> {
    let root = repo().join("tests/corpus").join(kind);
    let mut names: Vec<String> = fs::read_dir(&root)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
        .into_iter()
        .map(|n| {
            let files = vgs_files(&[&format!("tests/corpus/{kind}/{n}")]);
            (n, files)
        })
        .collect()
}

/// The seed of this run: VG_SEED if set, else a fixed default, so every run is reproducible.
fn seed() -> u64 {
    std::env::var("VG_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5eed_2026_1030)
}

const VARIANTS: usize = 10;

#[test]
fn alpha_families_hash_identically() {
    let fams = families("alpha");
    let seed = seed();
    let mut variants = 0;
    for (k, (name, files)) in fams.iter().enumerate() {
        assert!(
            files.len() >= 2,
            "alpha family {name} needs at least two files"
        );
        let base_src = fs::read_to_string(&files[0]).unwrap();
        let base = hashes(&base_src, &format!("{name}/{}", files[0].display()));
        for f in &files[1..] {
            let src = fs::read_to_string(f).unwrap();
            let h = hashes(&src, &format!("{name}/{}", f.display()));
            assert_eq!(
                h.iter().map(hex).collect::<Vec<_>>(),
                base.iter().map(hex).collect::<Vec<_>>(),
                "alpha family {name}: {} hashes differently from {}",
                f.display(),
                files[0].display()
            );
        }
        // Random variants of every file in the family.
        let module = parser::parse(&base_src).unwrap();
        for v in 0..VARIANTS {
            let s = seed ^ ((k as u64) << 32) ^ v as u64;
            let (variant, log) = moves::variant(&module, s);
            let text = moves::layout(&printer::print(&variant), s);
            let h = hashes(
                &text,
                &format!("{name} variant seed {s:#x} moves {log:?}\n{text}"),
            );
            assert_eq!(
                h, base,
                "{name}: variant with seed {s:#x} hashes differently; moves {log:?}\n{text}"
            );
            variants += 1;
        }
    }
    assert!(
        fams.len() >= 50,
        "G1 needs at least 50 alpha families, found {}",
        fams.len()
    );
    summary(
        "alpha.txt",
        &format!(
            "{} alpha families, {} hand-written files, {variants} random variants ({VARIANTS} per family), seed {seed:#x}: all identical\n",
            fams.len(),
            fams.iter().map(|f| f.1.len()).sum::<usize>()
        ),
    );
}

#[test]
fn distinct_families_hash_differently() {
    let fams = families("distinct");
    assert!(
        fams.len() >= 16,
        "only {} distinct families; the gate needs 16 (G1 review B8)",
        fams.len()
    );
    let mut pairs = 0;
    for (name, files) in &fams {
        assert!(
            files.len() >= 2,
            "distinct family {name} needs at least two files"
        );
        let hs: Vec<Vec<Hash>> = files
            .iter()
            .map(|f| {
                hashes(
                    &fs::read_to_string(f).unwrap(),
                    &format!("{name}/{}", f.display()),
                )
            })
            .collect();
        for i in 0..hs.len() {
            for j in i + 1..hs.len() {
                assert_ne!(
                    hs[i],
                    hs[j],
                    "distinct family {name}: {} and {} hash alike",
                    files[i].display(),
                    files[j].display()
                );
                pairs += 1;
            }
        }
    }
    summary(
        "distinct.txt",
        &format!(
            "{} distinct families, {pairs} pairs: all different\n",
            fams.len()
        ),
    );
}
