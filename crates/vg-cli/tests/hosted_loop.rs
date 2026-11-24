//! P5.7: the hosted loop and its grader, driven by scripted model turns (`vg loop --replay`), so no
//! test calls a paid API. Three copies of the midpoint task: one solved in the second repair round
//! after a counterexample, one whose POST is weakened to `true`, one that verifies its own weak
//! contract but computes the wrong value. The grader must tell them apart.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value as J, json};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

const OVERFLOWS: &str = "module m;
NODE @midpoint_u32 {
  SIGNATURE: (a: u32, b: u32) -> (u32);
  EFFECTS: [];
  CONTRACT { POST: result == (a + b) / 2; }
  BODY {
    %0 = ADD.checked(a, b);
    %1 = SHR.checked(%0, 1:u32);
    RETURN(%1);
  }
}";

const WEAK: &str = "module m;
NODE @midpoint_u32 {
  SIGNATURE: (a: u32, b: u32) -> (u32);
  EFFECTS: [];
  CONTRACT { POST: true; }
  BODY {
    %0 = AND(a, b);
    %1 = XOR(a, b);
    %2 = SHR.checked(%1, 1:u32);
    %3 = ADD.checked(%0, %2);
    RETURN(%3);
  }
}";

const WRONG: &str = "module m;
NODE @midpoint_u32 {
  SIGNATURE: (a: u32, b: u32) -> (u32);
  EFFECTS: [];
  CONTRACT { POST: result == a; }
  BODY {
    RETURN(a);
  }
}";

fn turn(id: &str, tool: &str, source: &str, tokens: (u64, u64)) -> J {
    json!({
        "content": [
            { "type": "text", "text": "Checking." },
            { "type": "tool_use", "id": id, "name": tool, "input": { "source": source } },
        ],
        "stop_reason": "tool_use",
        "usage": { "input_tokens": tokens.0, "output_tokens": tokens.1 },
    })
}

#[test]
fn the_grader_tells_a_solution_from_a_weak_contract_and_a_wrong_body() {
    let dir = std::env::temp_dir().join(format!("vg-hosted-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let src_task = repo().join("benchmarks/synthesis/03_midpoint_u32");
    for t in ["a_solved", "b_weak", "c_wrong"] {
        let d = dir.join("tasks").join(t);
        std::fs::create_dir_all(&d).unwrap();
        for f in ["request.md", "contract.vgs", "tests.json"] {
            std::fs::copy(src_task.join(f), d.join(f)).unwrap();
        }
    }
    let reference = std::fs::read_to_string(src_task.join("reference.vgs")).unwrap();
    let script = json!({
        "a_solved": [
            turn("t1", "vg_verify", OVERFLOWS, (1000, 200)),
            turn("t2", "vg_verify", &reference, (1500, 300)),
            turn("t3", "submit", &reference, (2000, 50)),
        ],
        "b_weak": [turn("w1", "submit", WEAK, (900, 100))],
        "c_wrong": [turn("r1", "submit", WRONG, (900, 100))],
    });
    std::fs::write(dir.join("script.json"), script.to_string()).unwrap();
    let out = dir.join("out");
    let run = Command::new(env!("CARGO_BIN_EXE_vg"))
        .current_dir(repo())
        .arg("loop")
        .arg(dir.join("tasks"))
        .arg("--replay")
        .arg(dir.join("script.json"))
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let results: J =
        serde_json::from_str(&std::fs::read_to_string(out.join("results.json")).unwrap()).unwrap();
    let task = |id: &str| {
        results["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()
            .clone()
    };

    let a = task("a_solved");
    assert_eq!(a["success"], true, "{a}");
    assert_eq!(
        (a["rounds"].as_u64(), a["verified"].as_bool()),
        (Some(2), Some(true))
    );
    assert_eq!(
        (
            a["reference_contract"].as_bool(),
            a["tests_passed"].as_bool()
        ),
        (Some(true), Some(true))
    );
    assert_eq!(
        (a["input_tokens"].as_u64(), a["output_tokens"].as_u64()),
        (Some(4500), Some(550))
    );
    // The first round's verify result, sent back to the model, carries the overflow.
    let t: J = serde_json::from_str(
        &std::fs::read_to_string(out.join("transcripts/a_solved.json")).unwrap(),
    )
    .unwrap();
    let first_result = t["messages"][2]["content"][0]["content"].as_str().unwrap();
    assert!(first_result.contains("ADD.no_overflow") && first_result.contains("counterexample"));

    let b = task("b_weak");
    assert_eq!(
        (b["verified"].as_bool(), b["tests_passed"].as_bool()),
        (Some(true), Some(true)),
        "{b}"
    );
    assert_eq!(
        (b["reference_contract"].as_bool(), b["success"].as_bool()),
        (Some(false), Some(false))
    );

    let c = task("c_wrong");
    assert_eq!(
        (c["verified"].as_bool(), c["tests_passed"].as_bool()),
        (Some(true), Some(false)),
        "{c}"
    );
    assert_eq!(
        (c["success"].as_bool(), c["unsound_accept"].as_bool()),
        (Some(false), Some(false))
    );

    assert_eq!(results["summary"], json!({ "success": 1, "unsound": 0 }));
    assert_eq!(results["model"], "claude-opus-5-5");
    let _ = std::fs::remove_dir_all(&dir);
}
