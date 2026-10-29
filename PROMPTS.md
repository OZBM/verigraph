# Prompts

Paste these into any coding agent opened at the repo root. Replace the `<placeholders>`.

## Kickoff — the first session, or the W0 lead

```text
You are the lead engineer for VeriGraph in this repository. Read AGENTS.md completely, then ROADMAP.md, PROGRESS.md and docs/00-vision.md. Start phase P0: a Cargo workspace with the 12 crates as skeletons, CI for macOS ARM64 and Linux x86_64, rust-toolchain.toml, toolchain.lock with the clang and solver versions installed here, a Makefile with one gate target per gate, and the tests/corpus folders. Then write reports/gate-g0.md and stop for my review. Work only in this repository, and tell me exactly what you ran and what you did not.
```

## Workstream — one per parallel agent

| ID | Name | Owns |
| --- | --- | --- |
| W0 | Integration lead | Workspace, CI, vg-core interfaces, ROADMAP, PROGRESS, gate reports, merges |
| W1 | Core | vg-core, vg-store |
| W2 | Semantics | vg-types, vg-interp, vg-telemetry |
| W3 | Verification | vg-smt |
| W4 | Backend | vg-codegen, vg-rt |
| W5 | Agent loop | vg-cli and vg mcp, vg-decode tiers 1–2, skills/verigraph-synthesis |
| W6 | Ecosystem | vg-mesh |
| W7 | Lifting | vg-omnilift |
| W8 | Research | One agent per active research track |

```text
You are workstream <W-ID> (<name>) on VeriGraph. Read AGENTS.md completely, then ROADMAP.md, PROGRESS.md and the docs chapters for your crates. Take the next unblocked task for <W-ID> from ROADMAP.md, log your plan in PROGRESS.md, and follow the work loop in AGENTS.md section 5. Edit only the crates your workstream owns. When your phase's gate script passes, write its report and stop for review.
```

## Continue — after a reset or in a new session

```text
Resume work on VeriGraph. Read AGENTS.md, the last 40 lines of PROGRESS.md and the last 20 commits. Re-run the test suite to confirm the current state before changing anything. Then continue the in-progress task for your workstream, or take the next one.
```

## Gate review — always in a fresh session

```text
You are an independent reviewer for VeriGraph gate <GN>. Do not trust reports/gate-<gN>.md. Re-run make gate-<gN> yourself, read that gate's criteria in ROADMAP.md, and try to break the claim: add at least five adversarial tests (for G3, programs that must fail verification; for G1, equivalent graphs that might hash differently). Report each criterion as met or not met with evidence, plus the bugs found and the tests added, in reports/gate-<gN>-review.md. Do not fix code.
```

## Research track

```text
Run research track <R-xx> in this repository. Read AGENTS.md, docs/research/PROTOCOL.md and docs/research/<R-xx file>, then follow the prompt in that file on branch research/<r-xx>, behind feature flag <r-xx>. Deliver docs/research/<R-xx>-report.md with a clear go or no-go.
```

## Build an app in VeriGraph — after M1

```text
Use the verigraph-synthesis skill (skills/verigraph-synthesis/SKILL.md) and the vg MCP tools to build: <plain-language request>. Start with a contract card for my confirmation, query VG-Mesh before writing new nodes, and keep repairing until vg_verify proves every obligation, or give me the exact list of what remains unproven and why.
```
