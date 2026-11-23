# MCP server

`vg mcp` serves the toolchain to AI agents over the Model Context Protocol, on stdio. An agent writes
VG-Sym, calls these tools, and acts on their JSON: the same JSON the [command line](cli.md) prints.

## Tools

Every tool takes the module either as `source` (its text) or as `path` (a `.vgs` file); `source` wins
when both are given.

| Tool | Other arguments | Result |
| --- | --- | --- |
| `vg_check` | none | as `vg check`: `ok`, `nodes`, `diagnostics` |
| `vg_verify` | `timeout_ms` (per obligation, default 10000) | as `vg verify`: `ok`, `nodes`, `results` |
| `vg_run` | `node` (a label such as `@soft_knee`), `inputs` (an object keyed by parameter name) | as `vg run` |
| `vg_build` | `root` (a label), `targets` (an array of target triples), `out_dir` (default `target/vg`) | as `vg build` |

`vg_build` never builds a node that is not fully proven: the `--unverified` escape is only on the
command line, for a person to choose. Give `out_dir` as an absolute path, since a relative one is read
from the folder the client started the server in.

A call, as a client sends it:

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "tools/call",
  "params": {
    "name": "vg_run",
    "arguments": { "path": "examples/quickstart.vgs", "node": "@soft_knee", "inputs": { "x": "3e38", "knee": "0.5" } }
  }
}
```

The result carries the JSON object twice, as `structuredContent` and as the text of its one content
item; here it is `{"ok": true, "outputs": ["1.0"], "post": true, "pre": true}`. `isError` stays
`false` when `ok` is `false`: a refuted obligation is an answer, not a failure of the call.

## Connect a client

The server is the command `vg` with the argument `mcp`. Use an absolute path to `vg`: a client started
from a desktop app does not always see your shell's `PATH`.

**Antigravity** (IDE, app and CLI): `editors/antigravity/install.sh` installs a plugin that declares
the server; see [Antigravity IDE](antigravity.md). By hand, the entry goes in
`~/.gemini/config/mcp_config.json`:

```json
{ "mcpServers": { "verigraph": { "command": "/Users/you/.cargo/bin/vg", "args": ["mcp"] } } }
```

**Claude Code**: a `.mcp.json` at the root of a project that holds VeriGraph sources:

```json
{ "mcpServers": { "verigraph": { "command": "/Users/you/.cargo/bin/vg", "args": ["mcp"] } } }
```

**Any other client**: start `vg mcp` as a stdio server. It speaks MCP protocol version 2025-06-18.

## The synthesis skill

`skills/verigraph-synthesis/SKILL.md` teaches an agent the workflow (contract first, check, verify,
repair, build), the rules of the language, and patterns that prove. Give it to the agent with the
server; the Antigravity plugin does both. The tools of docs/15 that are not in version 0
(`vg_mesh_query`, `vg_mesh_publish`, `vg_lift`, `vg_explain`) arrive with phases P6 to P8.
