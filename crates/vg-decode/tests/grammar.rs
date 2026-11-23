use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use vg_decode::{Format, export, grammar::recognize};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn collect_vgs(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_vgs(&p, out);
        } else if p.extension().and_then(|s| s.to_str()) == Some("vgs") {
            out.push(p);
        }
    }
}

fn corpus_and_example_files() -> Vec<PathBuf> {
    let root = repo_root();
    let mut files = Vec::new();
    for rel in [
        "tests/corpus/good",
        "tests/corpus/bad",
        "tests/corpus/alpha",
        "tests/corpus/errors",
        "examples",
    ] {
        collect_vgs(&root.join(rel), &mut files);
    }
    files
}

fn check_gbnf_syntax(src: &str) {
    let mut defs: BTreeMap<String, String> = BTreeMap::new();
    for (idx, raw_line) in src.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (lhs, rhs) = line
            .split_once("::=")
            .unwrap_or_else(|| panic!("GBNF line {} missing `::=`: {line}", idx + 1));
        let lhs = lhs.trim();
        assert!(
            !lhs.is_empty()
                && lhs.bytes().enumerate().all(|(i, b)| if i == 0 {
                    b.is_ascii_lowercase()
                } else {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'
                }),
            "invalid GBNF rule name `{lhs}` on line {}",
            idx + 1
        );
        let rhs = rhs.trim();
        assert!(!rhs.is_empty(), "empty RHS for GBNF rule `{lhs}`");
        assert!(
            defs.insert(lhs.to_string(), rhs.to_string()).is_none(),
            "duplicate GBNF rule `{lhs}`"
        );
    }
    assert!(
        defs.contains_key("root"),
        "GBNF grammar missing `root` rule"
    );

    // Verify balanced parens/brackets/quotes and collect every referenced rule name.
    let mut refs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (lhs, rhs) in &defs {
        let bytes = rhs.as_bytes();
        let mut i = 0;
        let mut paren_depth = 0i32;
        let mut used = BTreeSet::new();
        while i < bytes.len() {
            match bytes[i] {
                b' ' | b'\t' => i += 1,
                b'"' => {
                    i += 1;
                    let mut closed = false;
                    while i < bytes.len() {
                        if bytes[i] == b'\\' {
                            i += 2;
                        } else if bytes[i] == b'"' {
                            i += 1;
                            closed = true;
                            break;
                        } else {
                            i += 1;
                        }
                    }
                    assert!(closed, "unterminated string literal in GBNF rule `{lhs}`");
                }
                b'[' => {
                    i += 1;
                    let mut closed = false;
                    while i < bytes.len() {
                        if bytes[i] == b'\\' {
                            i += 2;
                        } else if bytes[i] == b']' {
                            i += 1;
                            closed = true;
                            break;
                        } else {
                            i += 1;
                        }
                    }
                    assert!(closed, "unterminated char class in GBNF rule `{lhs}`");
                }
                b'(' => {
                    paren_depth += 1;
                    i += 1;
                }
                b')' => {
                    paren_depth -= 1;
                    assert!(paren_depth >= 0, "unmatched `)` in GBNF rule `{lhs}`");
                    i += 1;
                }
                b'|' | b'*' | b'+' | b'?' => i += 1,
                b if b.is_ascii_lowercase() => {
                    let start = i;
                    while i < bytes.len()
                        && (bytes[i].is_ascii_lowercase()
                            || bytes[i].is_ascii_digit()
                            || bytes[i] == b'-')
                    {
                        i += 1;
                    }
                    let name = &rhs[start..i];
                    assert!(
                        defs.contains_key(name),
                        "GBNF rule `{lhs}` references undefined rule `{name}`"
                    );
                    used.insert(name.to_string());
                }
                other => panic!(
                    "unexpected byte {:?} in GBNF rule `{lhs}`: {rhs}",
                    other as char
                ),
            }
        }
        assert_eq!(paren_depth, 0, "unclosed `(` in GBNF rule `{lhs}`");
        refs.insert(lhs.clone(), used);
    }

    // Reachability check from `root`.
    let mut visited = BTreeSet::new();
    let mut stack = vec!["root".to_string()];
    while let Some(cur) = stack.pop() {
        if visited.insert(cur.clone()) {
            for nxt in &refs[&cur] {
                stack.push(nxt.clone());
            }
        }
    }
    let defined: BTreeSet<String> = defs.keys().cloned().collect();
    assert_eq!(
        visited,
        defined,
        "unreachable GBNF rules: {:?}",
        defined.difference(&visited).collect::<Vec<_>>()
    );
}

#[test]
fn export_is_deterministic() {
    for fmt in [Format::Gbnf, Format::Lark, Format::Llguidance] {
        assert_eq!(export(fmt), export(fmt));
    }
}

#[test]
fn gbnf_export_is_syntactically_well_formed() {
    let gbnf = export(Format::Gbnf);
    check_gbnf_syntax(&gbnf);
}

#[test]
fn llguidance_export_is_well_formed() {
    let llg = export(Format::Llguidance);
    assert!(llg.starts_with("%llguidance {}"));
    assert!(llg.contains("\nstart:"));
    assert!(!llg.contains("(?!)"), "llguidance does not allow lookahead");
    assert!(
        !llg.contains("\n?"),
        "llguidance export omits Lark `?` inline prefix"
    );

    let mut rules = BTreeSet::new();
    for line in llg.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('%') {
            continue;
        }
        let (name, _) = line
            .split_once(':')
            .unwrap_or_else(|| panic!("invalid llguidance line: {line}"));
        assert!(
            rules.insert(name.trim().to_string()),
            "duplicate rule `{name}` in llguidance export"
        );
    }
    for req in [
        "start", "node_def", "formula", "LABEL", "IDENT", "OPNAME", "PORT", "INT", "DECIMAL",
        "STRING", "WS", "COMMENT",
    ] {
        assert!(rules.contains(req), "llguidance missing rule `{req}`");
    }
}

#[test]
fn in_crate_recognizer_accepts_all_corpus_and_examples() {
    let files = corpus_and_example_files();
    assert!(
        files.len() >= 100,
        "expected >= 100 corpus/example files, found {}",
        files.len()
    );
    for path in files {
        let src = std::fs::read_to_string(&path).unwrap();
        if let Err(d) = recognize(&src) {
            panic!("recognizer rejected {}: {d}", path.display());
        }
    }

    // Lexical parity cases from tools/check_examples.py
    let wrap = |params: &str, post: &str, body: &str| {
        format!(
            "module lex;\nNODE @n {{\n  SIGNATURE: ({params}) -> (i32);\n  EFFECTS: [];\n  CONTRACT {{ POST: {post}; }}\n  BODY {{ {body} }}\n}}\n"
        )
    };
    let cases = [
        (wrap("x: i32", "result == x-1", "RETURN(x);"), true),
        (
            wrap(
                "x: i8",
                "result == x",
                "%0 = ADD.wrap(x, -128:i8); RETURN(%0);",
            ),
            true,
        ),
        (
            wrap(
                "x: i32",
                "result == x",
                "%0 = IDENTITY([1, -2, 3]:Array<i32, 3>); %1 = INDEX(%0, 0:u64); RETURN(%1);",
            ),
            true,
        ),
        (wrap("x: i32", "result == x % 2", "RETURN(x);"), true),
        (wrap("x: i32", "result == x%2", "RETURN(x);"), false),
        (wrap("x: i32", "(result.0).1 == x", "RETURN(x);"), true),
        (wrap("x: i32", "result.0.1 == x", "RETURN(x);"), false),
        (
            wrap("x: i32", "result == x", "%0 = ADD(result, x); RETURN(%0);"),
            false,
        ),
        (wrap("post: i32", "result == post", "RETURN(post);"), false),
        (
            wrap("x: i32", "forall(i in0..2) { x > 0 }", "RETURN(x);"),
            false,
        ),
        (
            wrap(
                "x: i32",
                "result == x",
                "%0: i32 = ADD.wrap(x, 1:i32); RETURN(%0);",
            ),
            true,
        ),
        (
            wrap("x: i32", "result == fold:step(0:i32, x, 1)", "RETURN(x);"),
            true,
        ),
        (wrap("fold: i32", "result == fold", "RETURN(fold);"), false),
    ];
    for (src, expect_ok) in cases {
        assert_eq!(recognize(&src).is_ok(), expect_ok, "case failed:\n{src}");
    }
}

#[test]
fn lark_export_accepts_corpus_via_python() {
    let check = Command::new("python3").args(["-c", "import lark"]).output();
    match check {
        Ok(out) if out.status.success() => {}
        _ => {
            eprintln!("skipping lark_export_accepts_corpus_via_python: python3 lark not installed");
            return;
        }
    }

    let lark_grammar = export(Format::Lark);
    let files = corpus_and_example_files();
    let py = r#"
import sys
from lark import Lark
g_text = sys.stdin.read()
g = Lark(g_text, parser="earley", lexer="basic")
for path in sys.argv[1:]:
    src = open(path, encoding="utf-8", newline="").read()
    g.parse(src)
print(f"lark parsed {len(sys.argv) - 1} files")
"#;
    let mut cmd = Command::new("python3");
    cmd.arg("-c").arg(py);
    for p in &files {
        cmd.arg(p);
    }
    cmd.stdin(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("spawn python3");
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(lark_grammar.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "python3 lark verification failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
