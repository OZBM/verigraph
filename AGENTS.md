# AGENTS.md — Build VeriGraph (VG-IR)

You are one of several AI engineering agents (one per workstream, plus one per active
research track) building VeriGraph: an AI-native programming system in which programs are
typed, content-addressed dataflow graphs with machine-checked contracts, compiled to native
x86_64, ARM64 and WASM binaries.
The human owner approves gates. Agents do all the engineering.

## 1. Read before any work
1. ROADMAP.md — phases, gates, what comes next.
2. PROGRESS.md — done, in progress, blocked. Append only: never edit or delete a line.
   .gitattributes merges it with git's union driver, so entries from parallel branches
   combine without conflicts when branches are merged or rebased locally.
3. docs/00-vision.md, then the docs chapter for your task.
4. docs/11-errata.md — the 27 errors of the source spec. Never reintroduce one.
5. docs/research/ — only when your task is a research track.
The original spec in docs/source/ is history. Where it disagrees with docs/, docs/ wins.

## 2. Non-negotiables
- Soundness first. A verifier that accepts a wrong program is the worst possible bug.
  Never weaken a check to make a test pass.
- Never fake results: no invented hashes, benchmark numbers, proofs or test outcomes.
  If something was not run, write "not run".
- Hashes come only from the toolchain. In VG-Sym, write labels (@name).
- UNKNOWN or a solver timeout is never a proof.
- The .vgir format and canonicalization freeze when G1 passes. Changing them needs an
  ADR, the owner's approval and a format_version bump.
- Determinism: same input, same bytes. No timestamps, unseeded randomness or hash-map
  iteration order in any output.
- Smallest complete change: no speculative abstractions, options or code "for later".
  Every non-trivial function gets a test.
- Research code stays behind its feature flag on its own branch until its report says go.
- Never mark a gate passed. Write the gate report; the owner approves.

## 3. Repository layout
crates/   vg-core vg-store vg-types vg-interp vg-smt vg-codegen vg-rt
          vg-decode vg-telemetry vg-mesh vg-omnilift vg-cli
runtime/  C sources of vg-rt
tests/corpus/{good,bad,alpha}/   benchmarks/{kernels,synthesis}/   examples/
skills/verigraph-synthesis/   docs/   docs/guide/   docs/adr/   research/r-xx/   reports/
editors/antigravity/ (the Antigravity IDE extension, agent plugin and installer)
Shared data types live only in vg-core.

## 4. Technology choices
- Rust stable, edition 2024, toolchain pinned in rust-toolchain.toml; warnings denied in CI.
- Hashing: the blake3 crate in derive-key mode, with versioned context strings.
- Solvers: z3, cvc5 and bitwuzla as separate processes speaking SMT-LIB2 over pipes;
  no C++ bindings. Versions pinned in toolchain.lock.
- Codegen: textual LLVM IR, built with clang 19 or newer, for x86_64-unknown-linux-gnu,
  aarch64-apple-darwin and wasm32-wasip1 (wasi-sdk). MLIR only from the alpha, as text.
- Runtime: vg-rt in C, compiled per target; libc on native targets, WASI libc on WASM.
- MCP server: the official Rust SDK (rmcp), stdio transport.
- Storage: files under .vg/objects/ plus SQLite (rusqlite) indexes.
- Tests: proptest, cargo-fuzz, insta snapshots, criterion benchmarks.
- Python 3.11+ only for research experiments and OmniLift's Python front end (ADR 0012).
Add a dependency only with a one-line reason in the commit message, and only with a
permissive license (MIT, Apache-2.0, BSD).

## 5. Work loop, for every task
1. Take the next unblocked task for your workstream from ROADMAP.md. Write a 3-8 line
   plan in PROGRESS.md under "In progress", with your workstream ID.
2. Write the tests first: corpus files, property tests, known-bad cases.
3. Implement the smallest change that makes them pass.
4. Run cargo fmt --check, cargo clippy -- -D warnings, cargo test --workspace, and
   make gate-gN for your phase.
5. Commit as "<crate>: <what> (Gn)", one logical change per commit.
6. Append to PROGRESS.md: what is done, the evidence (test names, numbers), what is next.
7. After two failed attempts at the same failure, stop. Record what you tried in
   PROGRESS.md, then take another task or ask the owner.

## 6. Workstreams
W0 Integration lead: workspace, CI, vg-core interfaces, ROADMAP and PROGRESS upkeep,
   gate reports, merges.
W1 Core: vg-core, vg-store.        W2 Semantics: vg-types, vg-interp, vg-telemetry.
W3 Verification: vg-smt.           W4 Backend: vg-codegen, vg-rt.
W5 Agent loop: vg-cli (with vg mcp), vg-decode tiers 1-2, skills/verigraph-synthesis, editors/.
W6 Ecosystem: vg-mesh.             W7 Lifting: vg-omnilift.
W8 Research: one agent per active research track.
Edit only the crates your workstream owns. To change another crate's interface, write a
request in docs/interface-requests/ and let its owner make it. Main stays green.
When one agent runs every workstream, it takes them in the phase order of ROADMAP.md.

## 7. Phases and gates
P0 scaffold, P1 core IR, P2 types and interpreter, P3 verification, P4 codegen and
runtime, P5 agent loop, P6 telemetry, P7 VG-Mesh lite, P8 OmniLift lite, then M1.
Phases overlap as ROADMAP.md shows. Each gate has make gate-gN, which exits 0 only when
every criterion holds and writes reports/gate-gN.md with the evidence.
M1 is done when examples/audio_limiter goes from its plain-language request, through the
hosted loop, to verified binaries on three targets, and the counterexample-and-repair
story in docs/03-vg-sym.md reproduces.

## 8. Research tracks
Items labeled [R-xx] are research. Keep them out of the MVP path. Run one only when
assigned, with its prompt in docs/research/ and the protocol in docs/research/PROTOCOL.md.

## 9. Testing policy
- tests/corpus/good must verify. tests/corpus/bad must be rejected with a reproducing
  counterexample; each file names the obligation that fails.
- Every bug fix adds a corpus case.
- Differential tests: native and WASM builds against vg-interp on generated programs.
- Mutation tests: a contract-breaking mutant that verifies fails CI.
- Fuzzing: parser, codec and canonicalizer, 10 minutes per change, 1 hour nightly.
- Every example in docs/ and in the synthesis skill is checked in CI.

## 10. Ask the owner when
- a gate criterion looks wrong or unreachable;
- a frozen format or a non-negotiable would have to change;
- a dependency is non-permissive or a paid service is needed;
- two attempts at the same failure have failed;
- anything would publish, delete or spend money.
Ask in PROGRESS.md under "Questions for the owner", with options and a recommendation.

## 11. Logs and decisions
PROGRESS.md entries: date (YYYY-MM-DD), workstream, what, evidence.
Decisions that change a spec or an interface: docs/adr/NNNN-title.md, with context,
decision and consequences.
