//! vg-core's lexer and parser agree with tools/vgsym.lark (docs/16, G1): on every VG-Sym file of
//! the repository, the same verdict and the same token texts. tests/lark-parity.tsv records Lark's
//! answers; tools/lark_parity.py writes it, and scripts/gate-g1.sh checks that it is current.

mod common;

use std::fs;

use common::{rel, repo, summary, vgs_files};
use vg_core::{lexer, parser};

fn fnv(texts: &[String]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in texts.join("\u{1f}").bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[test]
fn parser_agrees_with_lark() {
    let fixture =
        fs::read_to_string(repo().join("tests/lark-parity.tsv")).expect("tests/lark-parity.tsv");
    let files = vgs_files(&["examples", "tests/corpus", "tests/golden", "tests/lexical"]);
    let mut listed: Vec<String> = Vec::new();
    let (mut accepted, mut rejected) = (0, 0);
    for line in fixture.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        let [path, ok, n, h] = cols[..] else {
            panic!("bad fixture line {line:?}")
        };
        listed.push(path.to_string());
        let src = fs::read_to_string(repo().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        let mine = parser::parse(&src);
        assert_eq!(
            mine.is_ok(),
            ok == "1",
            "{path}: Lark says {ok}, vg-core says {mine:?}"
        );
        let (count, sum) = match lexer::token_texts(&src) {
            Ok(t) => (t.len().to_string(), fnv(&t)),
            Err(_) => ("-".to_string(), "-".to_string()),
        };
        assert_eq!(
            (count.as_str(), sum.as_str()),
            (n, h),
            "{path}: tokens differ from Lark's"
        );
        if ok == "1" {
            accepted += 1
        } else {
            rejected += 1
        }
    }
    let on_disk: Vec<String> = files.iter().map(|f| rel(f)).collect();
    assert_eq!(
        listed, on_disk,
        "tests/lark-parity.tsv does not list exactly the VG-Sym files: run tools/lark_parity.py"
    );
    summary(
        "parity.txt",
        &format!(
            "{} files ({accepted} accepted, {rejected} rejected by Lark): same verdict and token texts in vg-core\n",
            listed.len()
        ),
    );
}
