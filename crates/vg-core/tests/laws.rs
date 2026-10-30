//! The laws of docs/16 ("Laws and their property tests") on every VG-Sym file of the repository:
//! parse after print is the identity; canonicalization is idempotent; .vgir decodes to the node it
//! encodes; asm(disasm(b)) = b; disasm(asm(s)) is a fixed point. Golden files pin the encoding
//! (ADR 0001). Each law writes its counts to target/gate-g1/, which scripts/gate-g1.sh reports.

mod common;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use common::{rel, repo, summary, vgs_files};

use vg_core::ir::Hash;
use vg_core::{asm_local, canon, codec, disasm, parser, printer};

/// Every file that must parse: the examples and the corpus (lexical cases are checked against Lark).
fn parsing_files() -> Vec<PathBuf> {
    vgs_files(&["examples", "tests/corpus", "tests/golden"])
}

#[test]
fn parse_after_print_is_the_identity() {
    let files = parsing_files();
    for f in &files {
        let src = fs::read_to_string(f).unwrap();
        let a = parser::parse(&src).unwrap_or_else(|e| panic!("{}: {e}", rel(f)));
        let text = printer::print(&a);
        let b = parser::parse(&text)
            .unwrap_or_else(|e| panic!("{}: printed text does not parse: {e}\n{text}", rel(f)));
        assert_eq!(a, b, "{}: parse(print(t)) != t", rel(f));
        assert_eq!(printer::print(&b), text, "{}", rel(f));
    }
    summary(
        "print.txt",
        &format!("{} files: parse after print is the identity\n", files.len()),
    );
}

/// The asm laws, on every file that assembles without a @MESH_QUERY resolution.
#[test]
fn asm_laws() {
    let (mut files, mut nodes) = (0, 0);
    let mut skipped = Vec::new();
    for f in parsing_files() {
        let src = fs::read_to_string(&f).unwrap();
        if src.contains("@MESH_QUERY") {
            skipped.push(rel(&f));
            continue;
        }
        let out = asm_local(&src).unwrap_or_else(|e| panic!("{}: {e}", rel(&f)));
        files += 1;
        let by_hash: HashMap<Hash, vg_core::ir::Node> = out
            .iter()
            .map(|a| (a.hashes.node, a.node.clone()))
            .collect();
        for a in &out {
            nodes += 1;
            let what = format!("{} @{}", rel(&f), a.label);
            // Canonicalization is idempotent.
            let again = canon::canonicalize(&a.node).unwrap();
            assert_eq!(
                again.node, a.node,
                "{what}: canonicalizing twice changes the node"
            );
            assert!(again.dropped.is_empty());
            // Decoding gives back the node, and encoding the decoded node gives back the bytes.
            let decoded = codec::decode(&a.bytes).unwrap_or_else(|e| panic!("{what}: {e}"));
            assert_eq!(decoded, a.node, "{what}");
            // asm(disasm(b)) = b, and disasm(asm(s)) is a fixed point.
            let m = disasm::disasm("d", &[a.hashes.node], &|h| by_hash.get(h).cloned())
                .unwrap_or_else(|e| panic!("{what}: {e}"));
            let text = printer::print(&m);
            let re = asm_local(&text)
                .unwrap_or_else(|e| panic!("{what}: disassembly does not assemble: {e}\n{text}"));
            let last = re.last().unwrap();
            assert_eq!(last.bytes, a.bytes, "{what}: asm(disasm(b)) != b\n{text}");
            let by_hash2: HashMap<Hash, vg_core::ir::Node> =
                re.iter().map(|a| (a.hashes.node, a.node.clone())).collect();
            let m2 =
                disasm::disasm("d", &[last.hashes.node], &|h| by_hash2.get(h).cloned()).unwrap();
            assert_eq!(
                printer::print(&m2),
                text,
                "{what}: disasm(asm(s)) is not a fixed point"
            );
        }
    }
    summary(
        "asm.txt",
        &format!(
            "{files} files, {nodes} nodes: canonicalization idempotent, decode(encode(n)) = n, asm(disasm(b)) = b, disasm(asm(s)) fixed\nnot assembled (they need a @MESH_QUERY resolution): {}\n",
            if skipped.is_empty() {
                "none".to_string()
            } else {
                skipped.join(", ")
            }
        ),
    );
}

/// tests/golden/<stem>.vgs assembles to exactly tests/golden/<stem>.<label>.vgir. Set
/// UPDATE_GOLDEN=1 to write them, which ADR 0001 forbids after G1 without a format_version bump.
#[test]
fn golden_files() {
    let dir = repo().join("tests/golden");
    let update = std::env::var("UPDATE_GOLDEN").is_ok_and(|v| v == "1");
    let mut count = 0;
    for f in vgs_files(&["tests/golden"]) {
        let stem = f.file_stem().unwrap().to_string_lossy().to_string();
        let src = fs::read_to_string(&f).unwrap();
        for a in asm_local(&src).unwrap_or_else(|e| panic!("{}: {e}", rel(&f))) {
            let path = dir.join(format!("{stem}.{}.vgir", a.label));
            if update {
                fs::write(&path, &a.bytes).unwrap();
            }
            let golden =
                fs::read(&path).unwrap_or_else(|_| panic!("missing golden file {}", rel(&path)));
            assert_eq!(
                golden,
                a.bytes,
                "{} changed: the encoding is pinned by ADR 0001",
                rel(&path)
            );
            count += 1;
        }
    }
    let orphans: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "vgir"))
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            let stem = name.split('.').next().unwrap();
            !dir.join(format!("{stem}.vgs")).exists()
        })
        .map(|p| rel(&p))
        .collect();
    assert!(
        orphans.is_empty(),
        "golden files without a source: {orphans:?}"
    );
    assert!(count > 0);
    summary(
        "golden.txt",
        &format!("{count} golden .vgir files reproduced byte for byte\n"),
    );
}
