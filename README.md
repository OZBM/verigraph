# VeriGraph (VG-IR)

[![ci](https://github.com/OZBM/verigraph/actions/workflows/ci.yml/badge.svg)](https://github.com/OZBM/verigraph/actions/workflows/ci.yml)
[![license: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)

VeriGraph is an AI-native programming system. Programs are typed, content-addressed dataflow graphs
with machine-checked contracts. SMT solvers check them while an AI writes them, verified graphs compile
to native ARM64, x86_64 and WASM binaries, VG-Mesh lets agents find code by contract, and OmniLift
imports legacy C, C++, Rust, Zig, Swift, GLSL and Python functions.

```text
NODE @add_sat [intent: "Add two bytes, saturating at 255"] {
  SIGNATURE: (a: u8, b: u8) -> (u8);
  EFFECTS: [];
  CONTRACT {
    POST: result >= a && result >= b && (a + b <= 255 ==> result == a + b);
  }
  BODY {
    %0 = ADD.sat(a, b);
    RETURN(%0);
  }
}
```

`vg verify` proves the contract for every input, or returns a counterexample that it replays in the
reference interpreter; `vg build` refuses to compile a node with an unproven obligation.

## Status

Version 0, pre-release. The toolchain is built and tested on macOS Apple Silicon; CI runs on macOS
ARM64 and Linux x86_64. Each gate of [ROADMAP.md](ROADMAP.md) has a script (`make gate-gN`) that
writes its evidence to [reports/](reports/):

| Gate | Script result | Missing |
| --- | --- | --- |
| G0 scaffold, G1 core IR | every criterion holds | owner approval |
| G2 types and interpreter, G3 verification | every criterion holds | owner approval |
| G4 codegen and runtime | fails one criterion | the OS sandbox refusal test on a Linux host |
| G5 agent loop | not run | the hosted-model benchmark (paid API credits) |
| G6 telemetry, G7 VG-Mesh lite, G8 OmniLift lite | every criterion holds | owner approval |

M1, the end-to-end audio limiter from a plain-language request, depends on G5. The `.vgir` format is
not frozen until G1 is approved, so it may still change.

## Quick start

Requirements: Rust (the version in `rust-toolchain.toml` is installed by rustup), z3 and clang.
Optional: Bitwuzla, cvc5, Homebrew LLVM, zig, the WASI sysroot and wasmtime for the other solvers and
targets. [docs/guide/install.md](docs/guide/install.md) lists the pinned versions.

```bash
brew install z3
```

```bash
cargo install --path crates/vg-cli --locked
```

```bash
vg verify examples/quickstart.vgs
```

```bash
vg run examples/quickstart.vgs @soft_knee '{"x": "3e38", "knee": "0.5"}'
```

```bash
vg build examples/quickstart.vgs @soft_knee -o target/vg
```

The counterexample-and-repair story of [docs/03-vg-sym.md](docs/03-vg-sym.md):

```bash
vg verify examples/soft_knee_draft.vgs
```

`vg mcp` serves `vg_check`, `vg_verify`, `vg_run` and `vg_build` to AI agents over MCP stdio.
[docs/quickstart.md](docs/quickstart.md) is the short tour; [docs/guide/](docs/guide/README.md) is the
user guide (tutorial, language, verification, CLI, MCP, diagnostics, troubleshooting).

## Repository map

| Path | What it holds |
| --- | --- |
| `crates/` | The Rust workspace: vg-core (IR, parser, canonicalizer, hashes), vg-store, vg-types, vg-interp, vg-smt, vg-codegen, vg-rt, vg-decode, vg-telemetry, vg-mesh, vg-omnilift, vg-cli (the `vg` binary) |
| `runtime/` | The C runtime of vg-rt |
| `docs/` | The specification, one chapter per subsystem; docs/13-16 hold the normative semantics, opcode reference, agent interface and conformance rules |
| `docs/adr/` | Decisions that changed the spec |
| `docs/research/` | The research protocol and 14 research tracks |
| `tests/corpus/` | Programs that must verify (`good/`), be rejected with a counterexample (`bad/`), or hash identically (`alpha/`) |
| `examples/` | Example programs, including the audio limiter and its buggy first draft |
| `benchmarks/` | The G4 kernels and the G5 synthesis tasks |
| `skills/verigraph-synthesis/` | The skill AI agents use to write VeriGraph programs |
| `editors/antigravity/` | Antigravity IDE integration: `.vgs` editor extension, agent plugin, installer |
| `site/` | A French pedagogical website built with VeriGraph |
| `reports/` | Gate reports and benchmark results |

## How it is built

VeriGraph is written by AI coding agents under a human owner who approves the gates. The agents follow
[AGENTS.md](AGENTS.md) (Claude Code reads it through CLAUDE.md, Gemini tools through GEMINI.md), take
tasks from [ROADMAP.md](ROADMAP.md), and log every change with its evidence in
[PROGRESS.md](PROGRESS.md). [PROMPTS.md](PROMPTS.md) holds the kickoff, workstream, gate review and
research prompts. [docs/11-errata.md](docs/11-errata.md) lists the 27 errors of the original spec
(kept in docs/source/ for history) that must never come back.

Before a change lands:

```bash
cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT),
at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in this work, as defined in the Apache-2.0 license, shall be dual licensed as above, without
any additional terms or conditions.
