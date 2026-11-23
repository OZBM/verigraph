use std::path::{Path, PathBuf};
use std::time::Instant;

use vg_core::lexer::{lex, token_texts};
use vg_core::types::Type;
use vg_decode::{Prefix, TokenClass};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn vgs_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("vgs"))
        .collect();
    files.sort();
    files
}

#[test]
fn good_corpus_and_examples_soundness() {
    let root = repo_root();
    let mut paths = vgs_files_in(&root.join("tests/corpus/good"));
    paths.extend(vgs_files_in(&root.join("examples")));
    paths.extend(vgs_files_in(&root.join("tests/golden")));
    let mut synth_dirs: Vec<PathBuf> = std::fs::read_dir(root.join("benchmarks/synthesis"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    synth_dirs.sort();
    for d in synth_dirs {
        paths.push(d.join("reference.vgs"));
        paths.push(d.join("contract.vgs"));
    }

    assert!(paths.len() >= 70);
    for path in paths {
        let src = std::fs::read_to_string(&path).unwrap();
        let toks = token_texts(&src).unwrap_or_else(|e| panic!("lex {}: {e}", path.display()));
        let mut p = Prefix::new();
        for (i, tok) in toks.iter().enumerate() {
            let allowed = p.allowed();
            assert!(
                allowed.accepts(tok),
                "{}: token #{i} `{tok}` rejected by allowed(): {allowed:?}",
                path.display()
            );
            if let Err(d) = p.push(tok) {
                panic!("{}: push failed at token #{i} `{tok}`: {d}", path.display());
            }
        }
    }
}

#[test]
fn error_corpus_rejects_no_later_than_vg_check() {
    let root = repo_root();
    let files = vgs_files_in(&root.join("tests/corpus/errors"));
    assert_eq!(files.len(), 87, "expected all 87 error corpus files");

    for path in files {
        let src = std::fs::read_to_string(&path).unwrap();
        let lex_toks = lex(&src).unwrap();
        let texts = token_texts(&src).unwrap();

        let mut p = Prefix::new();
        let mut err_idx = None;
        for (i, tok) in texts.iter().enumerate() {
            let allowed = p.allowed();
            match p.push(tok) {
                Ok(()) => {
                    assert!(
                        allowed.accepts(tok),
                        "{}: push succeeded for `{tok}` not in allowed()",
                        path.display()
                    );
                }
                Err(_) => {
                    let is_num_lit = matches!(
                        lex_toks[i].tok,
                        vg_core::lexer::Tok::Int(_) | vg_core::lexer::Tok::Decimal(_)
                    );
                    if !is_num_lit {
                        assert!(
                            !allowed.accepts(tok),
                            "{}: push failed on `{tok}` yet allowed().accepts() was true!",
                            path.display()
                        );
                    }
                    err_idx = Some(i);
                    break;
                }
            }
        }
        let prefix_idx = err_idx.unwrap_or_else(|| {
            panic!("{}: Prefix accepted invalid program", path.display());
        });

        let check_diags = vg_types::check_source(&src).expect_err(&format!(
            "{}: check_source unexpectedly succeeded",
            path.display()
        ));
        let first_span = check_diags
            .iter()
            .filter(|d| d.span.line > 0)
            .map(|d| d.span)
            .min_by_key(|s| s.offset);
        if let Some(sp) = first_span {
            // Find the token index where vg check's error span begins, and the end of that span.
            let span_start_tok = lex_toks
                .iter()
                .position(|t| t.span.offset >= sp.offset)
                .unwrap_or(texts.len().saturating_sub(1));
            let span_end = sp.offset + sp.len;
            let mut span_end_tok = lex_toks
                .iter()
                .rposition(|t| t.span.offset < span_end)
                .unwrap_or(span_start_tok);
            // vg_core::lower sets BODY statement spans to just the leading `%n` token and
            // parameter declaration spans to just the parameter name token before `:`.
            // Extend 1-token `%n` statement spans to their `;` and 1-token `param:` spans
            // to the end of their `: Type` annotation (never beyond that construct).
            if span_end_tok == span_start_tok && texts[span_start_tok].starts_with('%') {
                if let Some(rel_semi) = texts[span_start_tok..].iter().position(|t| t == ";") {
                    span_end_tok = span_start_tok + rel_semi;
                }
            } else if span_end_tok == span_start_tok
                && texts.get(span_start_tok + 1).map(String::as_str) == Some(":")
            {
                let mut depth = 0i32;
                for (j, t) in texts.iter().enumerate().skip(span_start_tok + 2) {
                    match t.as_str() {
                        "(" | "<" | "{" | "[" => depth += 1,
                        ")" | ">" | "}" | "]" => {
                            if depth == 0 {
                                span_end_tok = j;
                                break;
                            }
                            depth -= 1;
                        }
                        "," | ";" | "=" if depth == 0 => {
                            span_end_tok = j;
                            break;
                        }
                        _ => {}
                    }
                }
            }
            // Prefix must report no later than the token where vg check's error span begins,
            // or within the enclosing statement/node span if vg_core::lower attributed the error to
            // a multi-token construct or leading `%n` statement span enclosing the causal token.
            assert!(
                prefix_idx <= span_end_tok,
                "{}: Prefix errored at token #{prefix_idx} (`{}`), after vg check's span {:?} (tokens {span_start_tok}..={span_end_tok})",
                path.display(),
                texts[prefix_idx],
                sp
            );
        }
    }
}

#[test]
fn allowed_precision_unit_tests() {
    // 1. Opcode mode & port type filtering
    let mut p = Prefix::new();
    for tok in [
        "module",
        "t",
        ";",
        "NODE",
        "@f",
        "{",
        "SIGNATURE",
        ":",
        "(",
        "x",
        ":",
        "u32",
        ",",
        "y",
        ":",
        "f32",
        ")",
        "->",
        "(",
        "u32",
        ")",
        ";",
        "EFFECTS",
        ":",
        "[",
        "]",
        ";",
        "CONTRACT",
        "{",
        "}",
        "BODY",
        "{",
        "%0",
        "=",
    ] {
        p.push(tok).unwrap();
    }
    let a = p.allowed();
    // Pure node cannot emit FILE_READ/FILE_WRITE/CLOCK_READ
    assert!(a.opcodes.iter().any(|(op, _)| op == "ADD"));
    assert!(!a.opcodes.iter().any(|(op, _)| op == "FILE_READ"));
    assert!(!a.opcodes.iter().any(|(op, _)| op == "CLOCK_READ"));

    p.push("ADD").unwrap();
    assert!(!p.allowed().accepts("(")); // ADD requires mode
    assert!(p.allowed().accepts("."));
    p.push(".").unwrap();
    assert!(p.allowed().accepts("checked"));
    assert!(p.allowed().accepts("wrap"));
    assert!(p.allowed().accepts("sat"));
    assert!(!p.allowed().accepts("s"));
    p.push("checked").unwrap();
    p.push("(").unwrap();

    // First operand: u32 param `x` fits, f32 param `y` does NOT
    let a_arg0 = p.allowed();
    assert!(a_arg0.ports.contains(&("x".into(), Type::UInt(32))));
    assert!(!a_arg0.ports.iter().any(|(n, _)| n == "y"));
    p.push("x").unwrap();
    p.push(",").unwrap();

    // Second operand: must also be u32
    let a_arg1 = p.allowed();
    assert!(a_arg1.ports.contains(&("x".into(), Type::UInt(32))));
    assert!(!a_arg1.ports.iter().any(|(n, _)| n == "y"));
    p.push("1").unwrap();
    p.push(":").unwrap();
    let a_scalar = p.allowed();
    assert!(a_scalar.token_classes.contains(&TokenClass::Keyword("u32")));
    assert!(!a_scalar.token_classes.contains(&TokenClass::Keyword("i32")));
    assert!(!a_scalar.token_classes.contains(&TokenClass::Keyword("f32")));
    p.push("u32").unwrap();
    p.push(")").unwrap();
    p.push(";").unwrap();
    p.push("RETURN").unwrap();
    p.push("(").unwrap();
    let a_ret = p.allowed();
    assert!(a_ret.ports.contains(&("%0".into(), Type::UInt(32))));
    assert!(a_ret.ports.contains(&("x".into(), Type::UInt(32))));
    assert!(!a_ret.ports.iter().any(|(n, _)| n == "y"));
}

#[test]
fn measure_allowed_latency_p99() {
    let root = repo_root();
    let files = vgs_files_in(&root.join("tests/corpus/good"));
    let all_tokens: Vec<Vec<String>> = files
        .iter()
        .map(|p| token_texts(&std::fs::read_to_string(p).unwrap()).unwrap())
        .collect();

    // Warm-up pass
    for toks in &all_tokens {
        let mut p = Prefix::new();
        for tok in toks {
            let _ = p.allowed();
            p.push(tok).unwrap();
        }
    }

    // Measured pass over the entire good corpus
    let mut nanos = Vec::new();
    for toks in &all_tokens {
        let mut p = Prefix::new();
        for tok in toks {
            let t0 = Instant::now();
            let _a = std::hint::black_box(p.allowed());
            nanos.push(t0.elapsed().as_nanos() as u64);
            p.push(tok).unwrap();
        }
        let t0 = Instant::now();
        let _a = std::hint::black_box(p.allowed());
        nanos.push(t0.elapsed().as_nanos() as u64);
    }
    nanos.sort_unstable();
    let p99_idx = ((nanos.len() as f64) * 0.99).ceil() as usize - 1;
    let p99_ns = nanos[p99_idx.min(nanos.len() - 1)];
    let p99_us = (p99_ns as f64) / 1000.0;
    println!("P5.4 allowed_p99_us={p99_us:.2} (samples={})", nanos.len());
    assert!(
        p99_us < 1000.0,
        "allowed() p99 latency {p99_us:.2} us exceeded 1000 us budget"
    );
}
