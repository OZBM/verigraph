# 00 · Vision

VeriGraph (VG-IR) is an AI-native programming system. Programs are typed dataflow graphs, not text files. Every function is identified by a BLAKE3 hash of its canonical structure and contract. SMT solvers check contracts and capabilities while the AI writes the code. Verified graphs compile to native x86_64, ARM64 and WASM binaries. VG-Mesh lets agents find code by what it provably does, and OmniLift imports legacy libraries.

The MVP is about six weeks of agent-only work; the full vision is a 12-month program. This specification keeps the source document's vision, fixes its 27 technical errors (docs/11-errata.md) and turns its research-grade claims into 13 labeled research tracks (docs/research/). A fourteenth track covers a risk the source never named: a contract that does not say what the human meant (R-14).

## The pillars, kept intact

| Part | The idea | In the MVP (v0.1) | Research track |
| --- | --- | --- | --- |
| Content-addressed graphs | Code is a canonical DAG; identity is a BLAKE3 hash | Binary format, VG-Sym text projection, canonical hashing, content store | R-02 |
| Dense combinators | Point-free dataflow with high-arity ops | SIMD_MAP, FOLD_BOUNDED, GENERATE, ASSERT_GUARD, CALL_HASH, plus scalar, array and effect ops | R-09 |
| Contracts and SMT-guided generation | Pre, post, termination and effects, checked while the AI generates | Verifier, generate-verify-repair loop for any AI, grammar-constrained decoding | R-01, R-04, R-05, R-10 |
| Differentiable execution | Learnable parameters tuned at runtime inside proven bounds | Parameters are explicit inputs, verified over their whole range | R-07 |
| Causal telemetry | Minimal failing subgraph and counterexample instead of stack traces | Counterexample plus failing slice, as JSON | R-08 |
| Native compilation | Proof erasure, then direct machine code | LLVM IR to x86_64, ARM64 and wasm32 | R-06 |
| OmniLift | Lift legacy libraries with equivalence proofs | Simple C, C++, Rust, Zig, Swift and GLSL functions, bounded equivalence checking | R-03, R-13 |
| VG-Mesh | Find code by contract, not by package name | Local registry, query by spec, re-verifying ingestion | R-12 |
| Self-hosting | The compiler compiles itself | After the alpha, around month 6 | R-11 |

## The three changes that matter most

- **Labels, not hand-written hashes.** AI agents write local names; the toolchain computes every hash. Models cannot compute BLAKE3, and the source spec's "built-in" hashes were invented.
- **Convergence by specification.** Two implementations of SHA-256 never share a structural hash. They join the same *spec class* once each is proven to meet the same reference spec.
- **Two generation modes.** Hosted models (Claude, Gemini, GPT) use a generate-verify-repair loop through tools, because their APIs expose no per-token logit masking. In-loop SMT masking runs on local open-weight models.

## What makes it AI-native

Every design decision is judged by what it does for an agent that writes, checks and repairs code with no human in the loop. Eight invariants follow from that; a change that breaks one needs an ADR saying why.

1. **Every judgment is data.** Errors, verdicts, counterexamples and costs come back as typed JSON with stable codes, located in the agent's own names (docs/15).
2. **Local reasoning.** Types follow from operands, one op at a time, and a node is verified against its callees' contracts, never their bodies, so an agent writes, checks and fixes one node at a time.
3. **Prefix-checkable text.** Ports and labels are defined before use, so every prefix can be checked and a constrained decoder can mask by type (ADR 0005).
4. **Identity by content.** One canonical graph has one hash on every machine, so each obligation is proven once and the proof is reused by every agent, locally and through Mesh.
5. **Same input, same bytes.** Hashes, proofs, diagnostics, binaries and NaN bits are deterministic (ADR 0002), so agents can cache, diff and replay anything.
6. **Nothing implicit.** No implicit conversion, no default arithmetic mode, no undefined behavior in verified code, no inline expression. Explicitness costs tokens, which R-09 measures, and buys meaning a model cannot misread.
7. **Contracts are the shared language.** Humans confirm contracts rendered by the toolchain in plain words (docs/15), agents search Mesh by contract, and the verifier ties contracts to code; R-14 measures whether contracts mean what people meant.
8. **Honest guarantees.** UNKNOWN is never a proof, measured is never proven, and every guarantee names what it trusts (docs/13).

The normative chapters behind these invariants are docs/13 (semantics), docs/14 (every op), docs/15 (the agent interface) and docs/16 (tests and gates).

## Non-goals

- Rewriting Z3, cvc5 or LLVM. They remain host services, even after self-hosting.
- Replacing human languages for humans. VG-Sym is for review, not for hand-writing large systems.
- Proving freedom from side channels, or anything a contract does not state.

## Glossary

| Term | Meaning |
| --- | --- |
| Node | The content-addressed unit: one function, stored as .vgir |
| Op | A combinator instance inside a node's body |
| Operand | What an op reads: an input, a constant, or another op's result |
| Port | A single-assignment value name in VG-Sym (`%3`) |
| Label | A local name for a node or query (`@soft_knee`); the toolchain maps it to a hash |
| VG-IR | The canonical binary graph format |
| VG-Sym | The text projection agents write and humans review |
| Obligation | One fact the verifier must prove, such as a PRE at a call site |
| Spec class | All implementations proven to refine one SPEC node |
| Effect | What a node may do (read files, read the clock); the empty set is PURE |
| Handle | A capability value (FileIn, Socket) created only by the runtime |
| Eff token | The value that orders side effects in the graph |
| SCC | A group of mutually recursive nodes, hashed together |
| Gate | A machine-checked phase exit (`make gate-gN`) approved by the owner |
| TCB | Trusted computing base: the code that must be right for proofs to mean anything |
| Kernel | A pure, fixed-size node that SIMD_MAP, FOLD_BOUNDED or GENERATE applies element by element |
| Contract gap | A refutation that needs callee results their contracts allow but their bodies never produce: strengthen a contract, not the body |
| Effect trace | The results of a run's effect ops in Eff order; replaying it makes effectful runs deterministic |
| Checked entry | The generated function that validates inputs and sets the floating-point environment before an exported node runs |
