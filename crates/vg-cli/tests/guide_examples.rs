//! Every ```vgsym example of the user guide (docs/guide) is checked, as AGENTS.md section 9 asks. The
//! first line of an example says what it must do: `// expect: verify`, `// expect: reject <ID>` (a
//! counterexample reproduced in vg-interp), `// expect: contract_gap <ID>`, or `// expect: error
//! <CODE>` (vg check reports that code). An example with no expect line must pass vg check.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value as J;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn vg(args: &[&str]) -> J {
    let out = Command::new(env!("CARGO_BIN_EXE_vg"))
        .args(args)
        .output()
        .expect("vg runs");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "vg {args:?} printed no JSON ({e}): {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// The ```vgsym blocks of a Markdown file, each with the line of its opening fence.
fn blocks(md: &str) -> Vec<(usize, String)> {
    let mut out = vec![];
    let mut open: Option<(usize, String)> = None;
    for (i, line) in md.lines().enumerate() {
        match open.as_mut() {
            None if line.trim() == "```vgsym" => open = Some((i + 1, String::new())),
            None => {}
            Some(_) if line.trim() == "```" => out.extend(open.take()),
            Some((_, src)) => {
                src.push_str(line);
                src.push('\n');
            }
        }
    }
    assert!(open.is_none(), "unclosed ```vgsym block");
    out
}

fn not_proven(r: &J) -> String {
    let ids: Vec<String> = r["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o["verdict"] != "proven")
        .map(|o| format!("{} {}", o["id"], o["verdict"]))
        .collect();
    format!(
        "not proven: {}; diagnostics: {}",
        ids.join(", "),
        r["diagnostics"]
    )
}

fn check(path: &str, src: &str) -> Result<(), String> {
    let expect = src
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("// expect: "))
        .unwrap_or("check");
    let words: Vec<&str> = expect.split_whitespace().collect();
    match words[..] {
        ["check"] => {
            let r = vg(&["check", path]);
            (r["ok"] == true)
                .then_some(())
                .ok_or(format!("vg check: {}", r["diagnostics"]))
        }
        ["verify"] => {
            let r = vg(&["verify", path]);
            (r["ok"] == true)
                .then_some(())
                .ok_or_else(|| not_proven(&r))
        }
        [kind @ ("reject" | "contract_gap"), id] => {
            let r = vg(&["verify", path]);
            let o = r["results"]
                .as_array()
                .and_then(|rs| rs.iter().find(|o| o["id"] == id))
                .ok_or_else(|| format!("no obligation {id}; {}", not_proven(&r)))?;
            let want = if kind == "reject" {
                "counterexample"
            } else {
                kind
            };
            if o["verdict"] != want {
                return Err(format!("{id} is {}, not {want}", o["verdict"]));
            }
            if kind == "reject" && o["report"]["reproduced"] != "concrete" {
                return Err(format!("{id}: the counterexample does not reproduce"));
            }
            Ok(())
        }
        ["error", code] => {
            let r = vg(&["check", path]);
            let found = r["diagnostics"]
                .as_array()
                .is_some_and(|ds| ds.iter().any(|d| d["code"] == code));
            (r["ok"] == false && found).then_some(()).ok_or(format!(
                "vg check does not report {code}: {}",
                r["diagnostics"]
            ))
        }
        _ => Err(format!("unknown expectation {expect:?}")),
    }
}

#[test]
fn every_guide_example_holds() {
    let mut mds: Vec<PathBuf> = fs::read_dir(repo().join("docs/guide"))
        .expect("docs/guide")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    mds.sort();
    let tmp = std::env::temp_dir().join(format!("vg-guide-{}", std::process::id()));
    fs::create_dir_all(&tmp).unwrap();
    let (mut n, mut failures) = (0, vec![]);
    for md in &mds {
        for (line, src) in blocks(&fs::read_to_string(md).unwrap()) {
            n += 1;
            let f = tmp.join(format!("example{n}.vgs"));
            fs::write(&f, &src).unwrap();
            if let Err(e) = check(f.to_str().unwrap(), &src) {
                failures.push(format!("{}:{line}: {e}", md.display()));
            }
        }
    }
    let _ = fs::remove_dir_all(&tmp);
    eprintln!("{n} guide examples, {} failures", failures.len());
    assert!(n >= 8, "only {n} examples in docs/guide");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
