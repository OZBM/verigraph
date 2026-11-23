//! `vg mcp`: the docs/15 tools over MCP stdio (rmcp): vg_check, vg_verify, vg_run, vg_build,
//! vg_explain, vg_mesh_query and vg_mesh_publish, against the project in the working directory.
//! vg_lift arrives with P8.

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, JsonObject, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value as J, json};

use crate::tools;

#[derive(Clone)]
struct Vg;

fn tool(name: &'static str, description: &'static str, props: J, required: &[&str]) -> Tool {
    let mut p = json!({
        "source": { "type": "string", "description": "VG-Sym module text" },
        "path": { "type": "string", "description": "path to a .vgs file; used when source is absent" },
    });
    p.as_object_mut()
        .expect("object")
        .extend(props.as_object().cloned().unwrap_or_default());
    let J::Object(schema) = json!({ "type": "object", "properties": p, "required": required })
    else {
        unreachable!("json! of an object")
    };
    Tool::new(name, description, Arc::new(schema))
}

fn tools_list() -> Vec<Tool> {
    vec![
        tool(
            "vg_check",
            "Parse, validate, canonicalize, type- and effect-check a VG-Sym module.",
            json!({}),
            &[],
        ),
        tool(
            "vg_verify",
            "Prove every obligation of every node with the SMT solver; counterexamples are replayed in vg-interp.",
            json!({
                "timeout_ms": { "type": "integer", "description": "per obligation, default 10000" },
                "detail": { "type": "integer", "description": "0: no reports; 1 (default): reports for the first 3 failures; 2: all" },
            }),
            &[],
        ),
        tool(
            "vg_explain",
            "The failure report of one obligation (its failing slice, counterexample and diff) and a plain-language account.",
            json!({ "obligation": { "type": "string", "description": "an obligation ID, such as @soft_knee/POST.0" } }),
            &["obligation"],
        ),
        tool(
            "vg_mesh_query",
            "Find verified nodes in the project's Mesh registry by contract (docs/09 MeshContractQuery); source and path are not used.",
            json!({
                "input_types": { "type": "array", "items": { "type": "string" } },
                "output_types": { "type": "array", "items": { "type": "string" } },
                "max_effects": { "type": "array", "items": { "type": "string" } },
                "pre": { "type": "string" },
                "post": { "type": "string" },
                "max_cost": { "type": "string" },
                "intent": { "type": "string" },
                "spec": { "type": "string", "description": "a spec class name, for an exact match" },
                "limit": { "type": "integer" },
            }),
            &[],
        ),
        tool(
            "vg_mesh_publish",
            "Publish every node of a module to the project's Mesh registry through the six-check ingestion gate.",
            json!({}),
            &[],
        ),
        tool(
            "vg_run",
            "Run one node in vg-interp on docs/15 JSON inputs keyed by parameter name.",
            json!({
                "node": { "type": "string", "description": "node label, such as @soft_knee" },
                "inputs": { "type": "object", "description": "parameter name to docs/15 value" },
            }),
            &["node", "inputs"],
        ),
        tool(
            "vg_build",
            "Verify a root node and everything it calls, then compile it to native code.",
            json!({
                "root": { "type": "string", "description": "node label" },
                "targets": { "type": "array", "items": { "type": "string" },
                             "description": "aarch64-apple-darwin, x86_64-unknown-linux-gnu, wasm32-wasip1" },
                "out_dir": { "type": "string", "description": "default: target/vg" },
            }),
            &["root", "targets"],
        ),
    ]
}

/// The module text of a call: `source`, or the file at `path`.
fn source(args: &JsonObject) -> Result<String, String> {
    if let Some(s) = args.get("source").and_then(J::as_str) {
        return Ok(s.to_string());
    }
    let path = args
        .get("path")
        .and_then(J::as_str)
        .ok_or("give source or path")?;
    std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
}

pub fn dispatch(name: &str, args: &JsonObject) -> J {
    if name == "vg_mesh_query" {
        return crate::mesh::query(&J::Object(args.clone()), std::path::Path::new("."));
    }
    let src = match source(args) {
        Ok(s) => s,
        Err(e) => return json!({ "ok": false, "error": e }),
    };
    let lock = vg_store::Lock::read(std::path::Path::new("vg.lock")).unwrap_or_default();
    let mesh = |label: &str| {
        lock.resolved
            .get(label)
            .filter(|e| e.source != "local")
            .map(|e| (e.node, e.spec))
    };
    let store = if std::path::Path::new(".vg").is_dir() {
        vg_store::Store::open(std::path::Path::new(".")).ok()
    } else {
        None
    };
    let fetch = |h: &vg_core::ir::Hash| store.as_ref().and_then(|s| s.get(h).ok().flatten());
    let env = tools::Env {
        mesh: &mesh,
        fetch: &fetch,
    };
    let str_arg = |k: &str| args.get(k).and_then(J::as_str).unwrap_or_default();
    match name {
        "vg_check" => tools::check(&src, &env),
        "vg_verify" => {
            let t = args.get("timeout_ms").and_then(J::as_u64).unwrap_or(10_000);
            let detail = args.get("detail").and_then(J::as_u64).unwrap_or(1).min(2) as u8;
            tools::verify(&src, &env, t, detail)
        }
        "vg_explain" => tools::explain(&src, &env, str_arg("obligation"), 10_000),
        "vg_mesh_publish" => crate::mesh::publish(&src, &env, std::path::Path::new(".")),
        "vg_run" => {
            let inputs = args
                .get("inputs")
                .and_then(J::as_object)
                .cloned()
                .unwrap_or_default();
            tools::run(&src, &env, str_arg("node"), &inputs)
        }
        "vg_build" => {
            let targets: Vec<String> = args
                .get("targets")
                .and_then(J::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(|t| t.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let out = match str_arg("out_dir") {
                "" => "target/vg",
                d => d,
            };
            tools::build(
                &src,
                &env,
                str_arg("root"),
                &targets,
                std::path::Path::new(out),
                false,
            )
        }
        n => json!({ "ok": false, "error": format!("unknown tool {n}") }),
    }
}

impl ServerHandler for Vg {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tools_list(),
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        Ok(CallToolResult::structured(dispatch(&request.name, &args)).into())
    }
}

/// Serves the tools on stdin and stdout until the client disconnects.
pub fn serve() -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let service = Vg
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|e| e.to_string())?;
        service.waiting().await.map_err(|e| e.to_string())?;
        Ok(())
    })
}
