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

/// A fresh working directory for this test process: vg keeps its proof cache in `./.vg`, which must
/// neither land in the source tree nor serve records from an earlier run.
fn scratch_cwd() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("vg-test-guide_examples-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    })
}

fn vg(args: &[&str]) -> J {
    let out = Command::new(env!("CARGO_BIN_EXE_vg"))
        .current_dir(scratch_cwd())
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

/// Every ```text block holding a `NODE` in `skills/verigraph-synthesis/SKILL.md` must pass both
/// `vg check` (with non-empty `contract_text`) and `vg verify` (P5.5, AGENTS.md section 9).
#[test]
fn every_synthesis_skill_example_verifies() {
    let skill_path = repo().join("skills/verigraph-synthesis/SKILL.md");
    let md = fs::read_to_string(&skill_path).expect("SKILL.md exists");
    let mut text_blocks = Vec::new();
    let mut open: Option<(usize, String)> = None;
    for (i, line) in md.lines().enumerate() {
        match open.as_mut() {
            None if line.trim() == "```text" => open = Some((i + 1, String::new())),
            None => {}
            Some(_) if line.trim() == "```" => {
                let (ln, blk) = open.take().unwrap();
                if blk.contains("NODE @") {
                    text_blocks.push((ln, blk));
                }
            }
            Some((_, src)) => {
                src.push_str(line);
                src.push('\n');
            }
        }
    }
    assert!(open.is_none(), "unclosed ```text block in SKILL.md");
    assert!(
        text_blocks.len() >= 4,
        "expected at least 4 VG-Sym blocks in SKILL.md, found {}",
        text_blocks.len()
    );

    let tmp = std::env::temp_dir().join(format!("vg-skill-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let mut failures = Vec::new();
    for (idx, (line, raw)) in text_blocks.iter().enumerate() {
        let src = if raw.trim_start().starts_with("module ") {
            raw.clone()
        } else {
            format!("module skill_{idx};\n\n{raw}")
        };
        let f = tmp.join(format!("skill_{idx}.vgs"));
        fs::write(&f, &src).unwrap();
        let path = f.to_str().unwrap();

        let chk = vg(&["check", path]);
        if chk["ok"] != true {
            failures.push(format!(
                "SKILL.md:{line}: vg check failed: {}",
                chk["diagnostics"]
            ));
            continue;
        }
        let nodes = chk["nodes"].as_array().expect("nodes array");
        for node in nodes {
            let ct = node["contract_text"].as_str().unwrap_or("");
            let obs = node["obligations"].as_u64().unwrap_or(0);
            if ct.is_empty() || obs == 0 {
                failures.push(format!(
                    "SKILL.md:{line}: node {} missing contract_text or obligations: {node}",
                    node["label"]
                ));
            }
        }
        let ver = vg(&["verify", path]);
        if ver["ok"] != true {
            failures.push(format!("SKILL.md:{line}: {}", not_proven(&ver)));
        } else {
            let proven_count = ver["results"].as_array().map_or(0, Vec::len) as u64;
            let expected_count: u64 = nodes.iter().filter_map(|n| n["obligations"].as_u64()).sum();
            if proven_count != expected_count {
                failures.push(format!(
                    "SKILL.md:{line}: vg check counted {expected_count} obligations, vg verify ran {proven_count}"
                ));
            }
        }
    }
    let _ = fs::remove_dir_all(&tmp);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn vg_check_renders_soft_knee_contract_text_and_caps_diagnostics() {
    let tmp = std::env::temp_dir().join(format!("vg-check-ct-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    // Extract @soft_knee from examples/audio_limiter.vgs into a standalone module.
    let limiter = fs::read_to_string(repo().join("examples/audio_limiter.vgs")).unwrap();
    let start = limiter.find("NODE @soft_knee").unwrap();
    let end = limiter.find("NODE @limit_frame").unwrap();
    let soft_knee_mod = format!("module soft_knee_only;\n\n{}", &limiter[start..end]);
    let f = tmp.join("soft_knee.vgs");
    fs::write(&f, &soft_knee_mod).unwrap();

    let r = vg(&["check", f.to_str().unwrap()]);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(r["module"], "soft_knee_only");
    let n0 = &r["nodes"][0];
    assert_eq!(n0["label"], "@soft_knee");
    assert_eq!(n0["signature"], "(x: f32, knee: f32) -> (f32)");
    assert_eq!(
        n0["contract_text"],
        "Requires: x is finite, and knee ≥ 0.5, and knee ≤ 0.99 \
         (the f32 value 0.9900000095367431640625). \
         Ensures: the result ≥ −1.0, and the result ≤ 1.0, and \
         (x ≥ 0.0) is true exactly when (the result ≥ 0.0) is true."
    );

    // 25 unused ops (VG-W0001) + 1 real type error -> ok MUST stay false even when capped at 20.
    let mut many = String::from(
        "module cap_test;\nNODE @w {\n  SIGNATURE: (x: u32) -> (u32);\n  EFFECTS: [];\n  CONTRACT { }\n  BODY {\n",
    );
    for i in 0..25 {
        many.push_str(&format!("    %{i} = ADD.wrap(x, 1:u32);\n"));
    }
    many.push_str("    RETURN(x);\n  }\n}\n");
    many.push_str(
        "NODE @bad {\n  SIGNATURE: (x: u32) -> (u32);\n  EFFECTS: [];\n  CONTRACT { }\n  BODY {\n    %0 = FADD(x, x);\n    RETURN(%0);\n  }\n}\n",
    );
    let f_cap = tmp.join("cap.vgs");
    fs::write(&f_cap, &many).unwrap();
    let rc = vg(&["check", f_cap.to_str().unwrap()]);
    assert_eq!(rc["ok"], false, "error past cap must still fail ok: {rc}");
    assert_eq!(rc["diagnostics"].as_array().unwrap().len(), 20);
    assert!(
        rc["truncated_diagnostics"].as_u64().unwrap_or(0) >= 6,
        "{rc}"
    );

    let _ = fs::remove_dir_all(&tmp);
}
