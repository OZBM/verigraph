//! `vg mcp` speaks MCP over stdio: the handshake, the tool list, and one call of each cheap tool.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

const MODULE: &str = "module m;\n\nNODE @inc {\n  SIGNATURE: (x: u8) -> (u8);\n  EFFECTS: [];\n  CONTRACT {\n    PRE: x < 255;\n    POST: result == x + 1;\n  }\n  BODY {\n    %0 = ADD.checked(x, 1:u8);\n    RETURN(%0);\n  }\n}\n";

/// A fresh working directory for this test process: vg keeps its proof cache in `./.vg`, which must
/// neither land in the source tree nor serve records from an earlier run.
fn scratch_cwd() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("vg-test-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    })
}

#[test]
fn mcp_lists_and_calls_tools() {
    let mut vg = Command::new(env!("CARGO_BIN_EXE_vg"))
        .current_dir(scratch_cwd())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("vg mcp starts");
    let mut stdin = vg.stdin.take().unwrap();
    let mut lines = BufReader::new(vg.stdout.take().unwrap()).lines();
    let mut call = |msg: Value| -> Option<Value> {
        writeln!(stdin, "{msg}").unwrap();
        msg.get("id")?;
        let line = lines.next()?.ok()?;
        Some(serde_json::from_str(&line).expect("JSON-RPC reply"))
    };
    let init = call(
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "vg-test", "version": "0"}}}),
    )
    .expect("initialize reply");
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "{init}"
    );
    call(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let list = call(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "vg_check",
            "vg_verify",
            "vg_explain",
            "vg_mesh_query",
            "vg_mesh_publish",
            "vg_run",
            "vg_build",
            "vg_lift"
        ]
    );
    let check = call(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "vg_check", "arguments": {"source": MODULE}}}))
    .unwrap();
    assert_eq!(check["result"]["structuredContent"]["ok"], true, "{check}");
    assert_eq!(check["result"]["structuredContent"]["module"], "m");
    assert_eq!(
        check["result"]["structuredContent"]["nodes"][0]["contract_text"],
        "Requires: x < 255. Ensures: the result = x + 1."
    );
    let partial = call(json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": {"name": "vg_verify", "arguments": {"source": MODULE, "obligations": ["@inc/NONVACUOUS"]}}}))
    .unwrap();
    assert_eq!(
        partial["result"]["structuredContent"]["results"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "{partial}"
    );
    assert_eq!(
        partial["result"]["structuredContent"]["nodes"][0]["verified"], false,
        "partial prefix selection must not mark the node verified: {partial}"
    );
    let run = call(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "vg_run", "arguments": {"source": MODULE, "node": "@inc", "inputs": {"x": 41}}}}))
    .unwrap();
    assert_eq!(
        run["result"]["structuredContent"]["outputs"],
        json!([42]),
        "{run}"
    );
    drop(stdin);
    assert!(vg.wait().unwrap().success());
}
