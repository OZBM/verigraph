//! P5.7, the hosted loop of docs/07: a hosted model gets a synthesis task, the synthesis skill and
//! three tools (vg_check, vg_verify and submit, run through the same code as `vg mcp`), and writes
//! a node in at most a few repair rounds. A grader that trusts nothing the model says then judges
//! the submission as docs/16 (G5) asks: it verifies; it meets the task's reference contract, proven
//! by vg-smt through a wrapper that calls it (PRE_ref ⇒ PRE_cand, and PRE_ref ∧ POST_cand ⇒
//! POST_ref); and it passes the task's tests in vg-interp. A verified node that breaks its own
//! contract on a test input is an unsound accept.
//!
//! Providers: the Anthropic Messages API over HTTPS, through curl (Rust has no official SDK; the
//! key goes in a temporary header file, never in argv, a transcript or an error), and a replay
//! provider that plays scripted model turns, which every test uses: the real one spends money.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, Value as J, json};

use crate::tools::{self, Env};

/// Where model turns come from.
pub trait Provider {
    /// One Messages API response to `body`, for task `task`.
    fn send(&mut self, task: &str, body: &J) -> Result<J, String>;
}

/// The Anthropic Messages API.
pub struct Anthropic {
    key: String,
}

impl Anthropic {
    /// Reads the key from ANTHROPIC_API_KEY.
    pub fn from_env() -> Result<Self, String> {
        match std::env::var("ANTHROPIC_API_KEY") {
            Ok(key) if !key.is_empty() => Ok(Self { key }),
            _ => Err("set ANTHROPIC_API_KEY to call the Anthropic API".into()),
        }
    }
}

impl Provider for Anthropic {
    fn send(&mut self, _task: &str, body: &J) -> Result<J, String> {
        let dir = std::env::temp_dir().join(format!("vg-loop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let (headers, request) = (dir.join("headers"), dir.join("request.json"));
        let written = write_private(&headers, &format!("x-api-key: {}\n", self.key))
            .and_then(|()| std::fs::write(&request, body.to_string()).map_err(|e| e.to_string()));
        let out = written.and_then(|()| {
            Command::new("curl")
                .args([
                    "-sS",
                    "--max-time",
                    "900",
                    "https://api.anthropic.com/v1/messages",
                ])
                .args(["-H", "content-type: application/json"])
                .args(["-H", "anthropic-version: 2026-10-21"])
                .args(["-H", "anthropic-beta: server-side-fallback-2026-07-01"])
                .arg("-H")
                .arg(format!("@{}", headers.display()))
                .arg("--data-binary")
                .arg(format!("@{}", request.display()))
                .output()
                .map_err(|e| format!("curl: {e}"))
        });
        let _ = std::fs::remove_dir_all(&dir);
        let out = out?;
        let resp: J = serde_json::from_slice(&out.stdout).map_err(|_| {
            format!(
                "the API answered no JSON (curl exit {:?})",
                out.status.code()
            )
        })?;
        if resp["type"] == "error" {
            return Err(format!("API error: {}", resp["error"]["message"]));
        }
        Ok(resp)
    }
}

#[cfg(unix)]
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| e.to_string())
}

/// Scripted turns: `{ "<task id>": [<response>, …] }`, played in order.
pub struct Replay {
    turns: BTreeMap<String, Vec<J>>,
}

impl Replay {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let turns = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self { turns })
    }
}

impl Provider for Replay {
    fn send(&mut self, task: &str, _body: &J) -> Result<J, String> {
        let left = self.turns.get_mut(task).filter(|t| !t.is_empty());
        left.map(|t| t.remove(0))
            .ok_or_else(|| format!("the replay script has no more turns for {task}"))
    }
}

/// One task: what the model is asked, and what the grader checks it against.
pub struct Task {
    pub id: String,
    pub request: String,
    /// The node's label, without `@`.
    pub node: String,
    /// The reference contract's `@MESH_QUERY` fields; None for a bare prompt (M1.1).
    pub contract: Option<BTreeMap<String, String>>,
    pub tests: Vec<J>,
}

/// The fields of the one `@MESH_QUERY` block of a contract.vgs, by name.
pub fn query_fields(src: &str) -> BTreeMap<String, String> {
    let body = src.split("@MESH_QUERY").nth(1).unwrap_or_default();
    let body = body.split_once('{').map_or("", |(_, b)| b);
    let mut out = BTreeMap::new();
    for line in body.lines() {
        let line = line.trim();
        if let Some((k, v)) = line.split_once(':')
            && ["signature", "effects", "pre", "post"].contains(&k.trim())
        {
            out.insert(
                k.trim().to_string(),
                v.trim().trim_end_matches(';').to_string(),
            );
        }
    }
    out
}

/// Splits on commas outside `<>`, `()` and `[]`.
fn top_level(list: &str) -> Vec<String> {
    let (mut out, mut depth, mut cur) = (vec![], 0i32, String::new());
    for ch in list.chars() {
        match ch {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(ch);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// The grader's wrapper: the reference contract over a call of the candidate, so verifying it
/// proves PRE_ref ⇒ PRE_cand (CALL.pre) and PRE_ref ∧ POST_cand ⇒ POST_ref (POST).
pub fn wrapper(node: &str, q: &BTreeMap<String, String>) -> Option<String> {
    let (params, results) = q.get("signature")?.split_once("->")?;
    let params = top_level(params.trim().strip_prefix('(')?.strip_suffix(')')?);
    let results = top_level(results.trim().strip_prefix('(')?.strip_suffix(')')?);
    let args: Vec<String> = (0..params.len()).map(|i| format!("arg{i}")).collect();
    let sig: Vec<String> = args
        .iter()
        .zip(&params)
        .map(|(a, t)| format!("{a}: {t}"))
        .collect();
    let outs: Vec<String> = (0..results.len()).map(|i| format!("%{i}")).collect();
    let pre = q
        .get("pre")
        .map(|p| format!("PRE: {p}; "))
        .unwrap_or_default();
    Some(format!(
        "\nNODE @vg_reference [intent: \"Grader: the reference contract, met through a call\"] {{\n  \
         SIGNATURE: ({}) -> ({});\n  EFFECTS: {};\n  CONTRACT {{ {pre}POST: {}; }}\n  BODY {{\n    \
         {} = CALL_HASH(@{node}, {});\n    RETURN({});\n  }}\n}}\n",
        sig.join(", "),
        results.join(", "),
        q.get("effects").map_or("[]", String::as_str),
        q.get("post")?,
        outs.join(", "),
        args.join(", "),
        outs.join(", "),
    ))
}

/// The tasks of a benchmarks/synthesis directory (one per subdirectory with a request.md), or
/// the one prompt of a Markdown file.
pub fn load_tasks(path: &Path) -> Result<Vec<Task>, String> {
    let read =
        |p: PathBuf| std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()));
    if path.extension().is_some_and(|e| e == "md") {
        let id = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        return Ok(vec![Task {
            id,
            request: read(path.to_path_buf())?,
            node: String::new(),
            contract: None,
            tests: vec![],
        }]);
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("request.md").is_file())
        .collect();
    dirs.sort();
    dirs.into_iter()
        .map(|d| {
            let tests: J =
                serde_json::from_str(&read(d.join("tests.json"))?).map_err(|e| e.to_string())?;
            Ok(Task {
                id: d
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                request: read(d.join("request.md"))?,
                node: tests["node"]
                    .as_str()
                    .unwrap_or_default()
                    .trim_start_matches('@')
                    .to_string(),
                contract: Some(query_fields(&read(d.join("contract.vgs"))?)),
                tests: tests["cases"].as_array().cloned().unwrap_or_default(),
            })
        })
        .collect()
}

fn tool(name: &str, description: &str) -> J {
    json!({
        "name": name, "description": description,
        "input_schema": {
            "type": "object",
            "properties": { "source": { "type": "string", "description": "a whole VG-Sym module" } },
            "required": ["source"], "additionalProperties": false,
        },
        "strict": true,
    })
}

fn tools_list() -> J {
    json!([
        tool(
            "vg_check",
            "Parse, validate, canonicalize, type- and effect-check a VG-Sym module. Free; does not count as a repair round."
        ),
        tool(
            "vg_verify",
            "Prove every obligation of every node with the SMT solvers; counterexamples are replayed in vg-interp. Each call is one repair round."
        ),
        tool(
            "submit",
            "Submit the final module. The conversation ends; the grader verifies it, checks it against the task's reference contract and runs the task's tests."
        ),
    ])
}

/// The result of one task, as reports/g5/results.json records it.
pub struct Graded {
    pub id: String,
    pub success: bool,
    pub rounds: usize,
    pub verified: bool,
    pub reference_contract: bool,
    pub tests_passed: bool,
    pub unsound_accept: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub error: Option<String>,
}

/// Runs one task through the loop and grades what the model submits.
pub fn run_task(
    task: &Task,
    provider: &mut dyn Provider,
    model: &str,
    max_rounds: usize,
    skill: &str,
    env: &Env,
) -> (Graded, J) {
    let smt = tools::solver_options(10_000);
    let target = if task.node.is_empty() {
        String::new()
    } else {
        let q = task.contract.clone().unwrap_or_default();
        format!(
            "\n\nThe node must be `NODE @{}` with signature `{}` and effects `{}`. State in its \
             contract what the request asks for.",
            task.node,
            q.get("signature").map_or("", String::as_str),
            q.get("effects").map_or("[]", String::as_str)
        )
    };
    let system = format!(
        "{skill}\n\nYou write VeriGraph programs in VG-Sym. Use vg_check freely and vg_verify to \
         prove your module; you have at most {max_rounds} vg_verify calls. When the module \
         verifies, call submit with it. A contract weaker than the request (such as POST: true) \
         fails the grader."
    );
    let mut messages =
        vec![json!({ "role": "user", "content": format!("{}{target}", task.request) })];
    let (mut rounds, mut input_tokens, mut output_tokens) = (0usize, 0u64, 0u64);
    let mut submitted: Option<String> = None;
    let mut error = None;
    let mut responses = vec![];
    for _ in 0..(3 * max_rounds + 3) {
        let body = json!({
            "model": model, "max_tokens": 16000, "system": system,
            "tools": tools_list(), "messages": messages, "fallbacks": "default",
            // Each turn resends the whole conversation: cache its prefix.
            "cache_control": { "type": "ephemeral" },
        });
        let resp = match provider.send(&task.id, &body) {
            Ok(r) => r,
            Err(e) => {
                error = Some(e);
                break;
            }
        };
        input_tokens += resp["usage"]["input_tokens"].as_u64().unwrap_or(0);
        output_tokens += resp["usage"]["output_tokens"].as_u64().unwrap_or(0);
        responses.push(resp.clone());
        if resp["stop_reason"] == "refusal" {
            error = Some("the model declined the request".into());
            break;
        }
        let content = resp["content"].as_array().cloned().unwrap_or_default();
        // The whole content goes back, thinking blocks included, unchanged.
        messages.push(json!({ "role": "assistant", "content": content }));
        let uses: Vec<&J> = content.iter().filter(|b| b["type"] == "tool_use").collect();
        if uses.is_empty() {
            break;
        }
        let mut results = vec![];
        for u in uses {
            let source = u["input"]["source"].as_str().unwrap_or_default();
            let (text, is_error) = match u["name"].as_str().unwrap_or_default() {
                "vg_check" => (tools::check(source, env).to_string(), false),
                "vg_verify" if rounds >= max_rounds => (
                    format!("no repair rounds left ({max_rounds} used): call submit"),
                    true,
                ),
                "vg_verify" => {
                    rounds += 1;
                    (tools::verify(source, env, &smt, 1).to_string(), false)
                }
                "submit" => {
                    submitted = Some(source.to_string());
                    ("submitted".into(), false)
                }
                other => (format!("unknown tool {other}"), true),
            };
            results.push(json!({
                "type": "tool_result", "tool_use_id": u["id"], "content": text, "is_error": is_error,
            }));
        }
        messages.push(json!({ "role": "user", "content": results }));
        if submitted.is_some() {
            break;
        }
    }
    let mut g = grade(task, submitted.as_deref(), env, &smt);
    g.rounds = rounds;
    g.input_tokens = input_tokens;
    g.output_tokens = output_tokens;
    g.success &= rounds <= max_rounds;
    g.error = error.or(g.error);
    let transcript =
        json!({ "task": task.id, "model": model, "messages": messages, "responses": responses });
    (g, transcript)
}

/// The grader: nothing the model claimed counts, only what vg-smt and vg-interp show.
pub fn grade(task: &Task, source: Option<&str>, env: &Env, smt: &vg_smt::Options) -> Graded {
    let mut g = Graded {
        id: task.id.clone(),
        success: false,
        rounds: 0,
        verified: false,
        reference_contract: false,
        tests_passed: false,
        unsound_accept: false,
        input_tokens: 0,
        output_tokens: 0,
        error: None,
    };
    let Some(src) = source else {
        g.error = Some("nothing was submitted".into());
        return g;
    };
    let v = tools::verify(src, env, smt, 0);
    let node_ok = |v: &J, label: &str| {
        v["nodes"].as_array().is_some_and(|ns| {
            ns.iter()
                .any(|n| n["label"] == format!("@{label}") && n["verified"] == true)
        })
    };
    g.verified = v["ok"] == true && (task.node.is_empty() || node_ok(&v, &task.node));
    if task.node.is_empty() {
        g.success = g.verified;
        return g;
    }
    if let Some(w) = task.contract.as_ref().and_then(|q| wrapper(&task.node, q)) {
        g.reference_contract = node_ok(
            &tools::verify(&format!("{src}{w}"), env, smt, 0),
            "vg_reference",
        );
    }
    let runs: Vec<(J, J)> = task
        .tests
        .iter()
        .map(|c| {
            let inputs: Map<String, J> = c["inputs"].as_object().cloned().unwrap_or_default();
            (
                tools::run(src, env, &format!("@{}", task.node), &inputs),
                c["outputs"].clone(),
            )
        })
        .collect();
    g.tests_passed = !runs.is_empty()
        && runs
            .iter()
            .all(|(r, want)| r["ok"] == true && r["outputs"] == *want);
    g.unsound_accept = unsound(g.verified, &runs);
    g.success = g.verified && g.reference_contract && g.tests_passed;
    g
}

/// A verified node that, on an input meeting its own PRE, traps or breaks its own POST.
fn unsound(verified: bool, runs: &[(J, J)]) -> bool {
    verified
        && runs
            .iter()
            .any(|(r, _)| r["pre"] == true && (r["ok"] != true || r["post"] == false))
}

/// Runs every task, writes `<out>/results.json` and one transcript per task; returns the results.
pub fn run_all(
    tasks: &[Task],
    provider: &mut dyn Provider,
    model: &str,
    max_rounds: usize,
    skill: &str,
    out: &Path,
    env: &Env,
) -> Result<J, String> {
    let tdir = out.join("transcripts");
    std::fs::create_dir_all(&tdir).map_err(|e| format!("{}: {e}", tdir.display()))?;
    let mut rows = vec![];
    for t in tasks {
        let (g, transcript) = run_task(t, provider, model, max_rounds, skill, env);
        let path = tdir.join(format!("{}.json", t.id));
        std::fs::write(&path, format!("{transcript:#}\n"))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        rows.push(json!({
            "id": g.id, "success": g.success, "rounds": g.rounds, "verified": g.verified,
            "reference_contract": g.reference_contract, "tests_passed": g.tests_passed,
            "unsound_accept": g.unsound_accept, "input_tokens": g.input_tokens,
            "output_tokens": g.output_tokens, "error": g.error,
        }));
    }
    let count = |k: &str| rows.iter().filter(|r| r[k] == true).count();
    let results = json!({
        "model": model, "tasks": rows,
        "summary": { "success": count("success"), "unsound": count("unsound_accept") },
    });
    let path = out.join("results.json");
    std::fs::write(&path, format!("{results:#}\n"))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wrapper_states_the_reference_contract_over_a_call() {
        let q = query_fields(
            "@MESH_QUERY @f {\n  signature: (Array<u32, 8>, u32) -> (u32);\n  effects: [];\n  \
             pre: arg1 < 8;\n  post: result == arg0[arg1];\n  intent: \"x\";\n}",
        );
        let w = wrapper("f", &q).unwrap();
        assert!(
            w.contains("SIGNATURE: (arg0: Array<u32, 8>, arg1: u32) -> (u32);"),
            "{w}"
        );
        assert!(
            w.contains("CONTRACT { PRE: arg1 < 8; POST: result == arg0[arg1]; }"),
            "{w}"
        );
        assert!(w.contains("%0 = CALL_HASH(@f, arg0, arg1);"), "{w}");
    }

    #[test]
    fn an_unsound_accept_is_a_verified_node_breaking_its_own_contract() {
        let ok = (json!({"ok": true, "pre": true, "post": true}), json!([1]));
        let broke = (json!({"ok": true, "pre": true, "post": false}), json!([1]));
        let outside = (json!({"ok": false, "pre": false}), json!([1]));
        assert!(!unsound(true, &[ok.clone(), outside.clone()]));
        assert!(unsound(true, &[ok, broke.clone()]));
        assert!(!unsound(false, &[broke]));
    }
}
