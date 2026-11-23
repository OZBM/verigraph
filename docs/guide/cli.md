# Command-line reference

```text
vg asm FILE.vgs [--root DIR] [--keep-debug]   assemble into DIR/.vg/objects and pin labels in DIR/vg.lock
vg disasm (HASH | FILE.vgir) [--root DIR]     print VG-Sym for a node and everything it calls
vg hash FILE...                               print node, body and contract hashes (.vgs or .vgir)
vg check FILE.vgs                             parse, canonicalize, type- and effect-check (JSON)
vg verify FILE.vgs [--mode M] [--timeout MS]  prove every obligation with the SMT solvers (JSON)
vg run FILE.vgs @NODE JSON                    run a node in vg-interp; JSON maps parameter names to values
vg build FILE.vgs @NODE [--target TRIPLE]... [-o DIR] [--unverified]
                                              verify, then compile (default target: aarch64-apple-darwin)
vg mcp                                        serve vg_check, vg_verify, vg_run, vg_build over MCP stdio
```

`check`, `verify`, `run` and `build` print one JSON object, the result of the MCP tool of the same
name ([MCP server](mcp.md)), and share these exit codes:

| Exit code | Meaning |
| --- | --- |
| 0 | `"ok": true` |
| 1 | `"ok": false`: the JSON says why |
| 2 | Wrong arguments; the usage is printed on stderr |

They resolve `@MESH_QUERY` labels through the `vg.lock` of `--root` (default: the current folder).

## vg check

```bash
vg check FILE.vgs
```

Parses the module, validates and canonicalizes every node, computes its hashes, and checks types and
effects. Nothing is proven.

| Field | Content |
| --- | --- |
| `ok` | `true` when no diagnostic is an error |
| `nodes` | for each node that checks: `label`, `kind` (`node` or `spec`), `node_hash`, `effects`, `cost_ops` |
| `diagnostics` | `code`, `severity` (`error` or `warning`), `node`, `op`, `line`, `col`, `message` ([Diagnostics](diagnostics.md)) |

`line` and `col` are 1-based. A type or effect error found on the canonical node has `line` 0 and
names its node instead.

## vg verify

```bash
vg verify FILE.vgs [--mode edit|ci|release] [--timeout MS]
```

Proves every obligation of every node. z3 takes most queries and Bitwuzla, when installed, the
queries with floats. `--mode` sets the time limit per query: `edit` 2000 ms, `ci` 60000 ms, and
`release` 60000 ms with a second solver that must give the same answer, so each proof there has
the evidence `two-solvers`. Without `--mode` the limit is 10000 ms; `--timeout` overrides it.
Proofs are recorded in `<root>/.vg/proofs` (`--root`, default `.`) and reused while they hold.
[Verification](verification.md) explains the obligations and verdicts.

| Field | Content |
| --- | --- |
| `ok` | `true` when every obligation of every node is proven |
| `nodes` | `label`, `verified` |
| `results` | one per obligation, sorted by ID: `id`, `hash` (the obligation hash of docs/02, which names its proof record), `time_ms`, `verdict`, `code` (when not proven), `solver` (such as `z3`, or `z3+cvc5` when two agreed; `none` for the pre-screen), `evidence` (`prescreen`, `re-solved`, `two-solvers` or `cached`), `message`, and `report` for a refutation |
| `report` | `node`, `obligation`, `inputs` (by parameter name), `reproduced` (`concrete` or `no`) |
| `diagnostics` | present instead of `results` when the module does not load |

## vg run

```bash
vg run FILE.vgs @NODE '{"name": value, …}'
```

Runs one node in vg-interp, the reference interpreter. Inputs are keyed by parameter name, in the
[JSON value](#json-values) forms; an `Eff` parameter is supplied for you. Version 0 runs with no
effect host, so effect ops find no files or clocks: use `vg run` on pure nodes.

| Result | Fields |
| --- | --- |
| The node returned | `ok: true`, `outputs` (a JSON array, `Eff` left out), `pre` and `post` (whether each held) |
| The node trapped | `ok: false`, `pre`, `trap` with the failing `op` (canonical index), `opcode` and `message` |
| Bad input | `ok: false`, `error` |

`pre` and `post` are `null` when the clause could not be evaluated.

## vg build

```bash
vg build FILE.vgs @NODE [--target TRIPLE]... [-o DIR] [--unverified]
```

Verifies `@NODE` and everything it calls (10-second timeout per obligation), then compiles it with
clang. Targets: `aarch64-apple-darwin` (the default), `x86_64-unknown-linux-gnu` and `wasm32-wasip1`;
repeat `--target` for several. Each artifact is written to `DIR/<label>-<target>` (default DIR:
`target/vg`).

- A fully proven node builds with proof facts and no runtime checks.
- A node that is not fully proven is refused, and the result carries the `verify` report. With
  `--unverified` it builds with a runtime check on every partial op.

| Field | Content |
| --- | --- |
| `ok`, `verified` | the build succeeded; every obligation was proven |
| `artifacts` | `target`, `path`, `bytes`, `blake3` (of the file), `linked` (`false` for an object file), `residual_guards` |
| `error`, `verify` | why a build was refused, and the verification result |

In version 0, only `aarch64-apple-darwin` produces a linked executable. The other two targets produce
object files (`"linked": false`) until a Linux sysroot and wasi-sdk are set up.

## vg asm, vg disasm, vg hash

```bash
vg asm FILE.vgs [--root DIR] [--keep-debug]
```

Assembles every node into `DIR/.vg/objects` (content-addressed `.vgir` files with a SQLite index) and
pins each label in `DIR/vg.lock`. It prints one `@label hash` line per node. `--keep-debug` keeps the
source map and comments in each object's DEBUG section.

```bash
vg disasm HASH [--root DIR]
```

Prints a stored node, and everything it calls, as VG-Sym. Names are not part of a node's identity, so
they come back generated: `@n0dceb8d6b0ab` for the node, `a0`, `a1` for its parameters.

```bash
vg hash FILE...
```

Prints the node, body and contract hashes of every node in `.vgs` or `.vgir` files. Two nodes with the
same node hash are the same program.

## JSON values

Inputs, outputs and counterexamples share one encoding (docs/15), so a counterexample pastes straight
into `vg run`:

| Type | JSON |
| --- | --- |
| Bool | `true`, `false` |
| u8 … u32, i8 … i32 | a number |
| u64, i64 | a decimal string, such as `"18446744073709551615"` |
| floats | a string: the shortest decimal that rounds back (`"0.1"`, `"3e38"`, `"-0.0"`), or `"inf"`, `"-inf"`, `"nan"` |
| Array, Tensor, Slice, Tuple | a JSON array; tensors nest, row-major |
| Result | `{"ok": v}` or `{"err": e}` |

## Built executables

An executable built by `vg build` reads its inputs from stdin and writes its results to stdout, in the
byte form of docs/13:

- Each parameter, in order, except `Eff` and handles: Bool as one byte (0 or 1), integers
  little-endian at their width, floats as their IEEE bits little-endian, arrays and tensors as their
  elements, row-major. docs/13 and docs/06 give the layout of the other types.
- The results follow the same rules, in order.

| Exit code | Meaning |
| --- | --- |
| 0 | Success; the results are on stdout |
| 1 | The inputs break PRE (checked on entry) |
| 2 | The input bytes do not decode: too few bytes, or a non-canonical NaN |
| 3 | The arena is exhausted |
| 4 | A runtime check failed (only in `--unverified` builds) |

```bash
printf '\007\000\000\000\002\000\000\000' | target/vg/safe_div-aarch64-apple-darwin | xxd
```

```text
00000000: 0300 0000                                ....
```
