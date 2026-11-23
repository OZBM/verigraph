# Quick start (v0)

What works today, on macOS ARM64 with z3 installed (`brew install z3`). Every command prints the JSON
result of the MCP tool of the same name (docs/15) and exits 1 when `ok` is false. The
[user guide](guide/README.md) covers all of this in depth.

```bash
cargo build --release -p vg-cli
```

```bash
alias vg=target/release/vg
```

## Check, run, verify, build

```bash
vg check examples/quickstart.vgs
```

```bash
vg run examples/quickstart.vgs @soft_knee '{"x": "3e38", "knee": "0.5"}'
```

```bash
vg verify examples/quickstart.vgs
```

```bash
vg build examples/quickstart.vgs @soft_knee -o target/vg
```

- `check` parses, canonicalizes, hashes, then type- and effect-checks every node.
- `run` executes a node in vg-interp. Inputs are docs/15 JSON values keyed by parameter name: floats
  are strings (`"0.5"`, `"-0.0"`, `"inf"`, `"nan"`), u64 and i64 decimal strings, arrays JSON arrays.
  The result says whether PRE held on the inputs and POST on the outputs.
- `verify` proves every obligation with z3. A refuted obligation carries a counterexample as named
  inputs, replayed in vg-interp: `reproduced: "concrete"` means the body really breaks the contract,
  and the verdict becomes `contract_gap` when only a callee's contract is too weak.
- `build` verifies the root and everything it calls, then compiles it with clang, with proof facts
  and without runtime checks; it refuses a node with an unproven obligation. The binary reads its
  inputs from stdin as the docs/13 bytes of each parameter and writes its results the same way.
  `--target x86_64-unknown-linux-gnu` and `--target wasm32-wasip1` produce object files until a Linux
  sysroot and wasi-sdk are installed. `--unverified` builds a node that is not fully proven, with a
  runtime check on every partial op (exit code 4 when one fails).

The counterexample story of docs/03:

```bash
vg verify examples/soft_knee_draft.vgs
```

## Use it from an AI agent

`vg mcp` serves `vg_check`, `vg_verify`, `vg_run` and `vg_build` over MCP stdio. For Antigravity IDE,
run `editors/antigravity/install.sh` (docs/guide/antigravity.md). For Claude Code, add this to
`.mcp.json` at the root of a project that has VeriGraph sources:

```json
{ "mcpServers": { "verigraph": { "command": "/path/to/verigraph/target/release/vg", "args": ["mcp"] } } }
```

Then give the agent skills/verigraph-synthesis/SKILL.md. The other four tools of docs/15 (Mesh query
and publish, lift, explain) arrive with P6 to P8.

## Not in v0 yet

- `@MESH_QUERY` resolution beyond vg.lock pins, so examples/audio_limiter.vgs's `@limit_frame` cannot
  call its SHA-256 yet; its `@soft_knee` verifies and builds (examples/quickstart.vgs has a copy).
- COST obligations (reported as unknown), cvc5 and Bitwuzla, small-model search, the proof cache,
  plain-language `contract_text`, failing slices (`vg explain`).
- Linked binaries for Linux x86_64 and WASM, sandbox manifests, checked-entry symbols for libraries.
- Obligation IDs name ops by canonical index (`%3`), not by the port written in the source.
