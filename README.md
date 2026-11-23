# VeriGraph build kit

Everything an AI coding agent needs to build VeriGraph (VG-IR) from an empty repository: the corrected specification, the roadmap and its gates, the rules every agent follows, the research tracks, and ready-to-paste prompts.

VeriGraph is an AI-native programming system. Programs are typed, content-addressed dataflow graphs with machine-checked contracts; SMT solvers check them while the AI writes them; verified graphs compile to native x86_64, ARM64 and WASM binaries; VG-Mesh lets agents find code by contract, and OmniLift imports legacy libraries.

## Start

1. Unzip; the `verigraph/` folder is the repo root. Inside it run `git init && git add . && git commit -m "VeriGraph kit"`.
2. Install: Rust stable; clang/LLVM 19 or newer; the `z3`, `cvc5` and `bitwuzla` command-line solvers; Python 3.11+; wasi-sdk. Later phases add Alive2 (P8) and a local model runtime such as llama.cpp, Ollama or vLLM (R-01, R-09).
3. Open your coding agent at the repo root. Claude Code reads CLAUDE.md and Gemini tools read GEMINI.md; both import AGENTS.md. Any other agent gets AGENTS.md from the Kickoff prompt.
4. Paste the **Kickoff** prompt from PROMPTS.md. For parallel agents, give each one the **Workstream** prompt.
5. At every gate, read `reports/gate-gN.md`, run the **Gate review** prompt in a fresh session, then approve or send the findings back.

Your part: about 15–30 minutes a day reading PROGRESS.md, reviewing gate reports and answering the agents' questions. Agents never mark a gate passed themselves.

## Use the toolchain

The v0 toolchain checks, runs, verifies and builds VG-Sym programs, and serves them to AI agents over
MCP: see docs/quickstart.md, and docs/guide/ for the complete user guide. `editors/antigravity/install.sh`
integrates it into Antigravity IDE: an editor extension for `.vgs` files, and an agent plugin with the
MCP server and the synthesis skill.

## Map

| Path | What it holds |
| --- | --- |
| AGENTS.md | The rules every agent follows |
| PROMPTS.md | Kickoff, workstream, continue, gate review, research track, build an app |
| ROADMAP.md | Milestones, dates, gates, tasks by phase, research waves |
| PROGRESS.md | The agents' log; starts empty |
| docs/00-vision.md … docs/10-bootstrap.md | The corrected specification, one chapter per subsystem |
| docs/11-errata.md | The 27 errors of the source spec and their fixes |
| docs/12-risks-and-open-questions.md | Risks, budget, and the decisions waiting on the owner |
| docs/13-semantics.md | What a node means: values, one NaN, evaluation, memory, formulas, the guarantee, and the mathematics behind them |
| docs/14-opcode-reference.md | Every MVP op: number, typing, meaning, obligation, SMT encoding, LLVM lowering |
| docs/15-agent-interface.md | What agents read: error codes, obligation IDs, JSON values, MCP tool schemas, failure reports, plain-language contracts |
| docs/16-conformance-and-gates.md | Test file formats, the laws property tests check, benchmarks, and what each gate script runs |
| docs/guide/ | The user guide: installation, tutorial, language, verification, command line, MCP, Antigravity IDE, diagnostics, troubleshooting |
| docs/adr/ | Decisions that changed the spec; 0002–0010 await the owner's approval |
| docs/research/ | The research protocol, 14 research tracks with their prompts, and the October 2026 literature survey |
| docs/source/original-spec.md | The source document, kept for history; docs/ wins where they disagree |
| skills/verigraph-synthesis/SKILL.md | The skill AIs use to write VeriGraph programs once the toolchain exists |
| examples/ | The audio-limiter example, its plain-language request, and the buggy first draft the verifier must reject |
| tools/ | An executable reference grammar for VG-Sym (Lark) and a script that parses every example |
| editors/antigravity/ | The Antigravity IDE integration: the editor extension, the agent plugin and their installer |

## Labels

**[MVP]** ships in v0.1 · **[ALPHA]** ships in v0.2 · **[R-xx]** research track, never on the MVP path.
