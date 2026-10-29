# 11 · Errata of the source spec

The source spec (docs/source/original-spec.md) has 27 technical errors. Each has a fix that keeps the vision, and the chapters of this spec already apply them. Agents must never reintroduce one.

| # | Problem in the source spec | Fix (where) |
| --- | --- | --- |
| E-01 | The header is 120 bytes, so not 16-byte aligned; no section offsets; ops point into a constant pool that does not exist | 128-byte header, a section directory, CONSTS, TYPES and RESULTS sections (02) |
| E-02 | Ports are numbered "by topological order", which is not unique, so equivalent graphs can hash differently | Merkle hashing and a post-order walk from the outputs, with merging and dead-op removal (02) |
| E-03 | Side effects have no order in a pure DAG, and merging identical ops would merge two writes | Effect tokens thread every effectful op (02, 04) |
| E-04 | A recursive function would have to contain its own hash | SCC hashing with REC_CALL indices, as in Unison (02) |
| E-05 | The grammar makes the AI write hashes it cannot compute; the "built-in" hashes are invented (`#e3b0c442` is the start of SHA-256 of the empty string); 8-digit prefixes collide early | Labels in text, hashes computed by `vg asm` and pinned in `vg.lock`, 12-digit display (02, 03) |
| E-06 | A 32-value FP16 intent embedding inside each node is too small and changes with the embedding model | Intent text in the unhashed DEBUG section; embeddings in the Mesh index, per model (02, 09) |
| E-07 | The EBNF leaves TypeSig, SmtFormula, ArgList and more undefined; the examples assign to signature ports and use inline expressions the grammar forbids | A complete grammar with single-assignment ports and RETURN (03) |
| E-08 | No comparison, logic, select, index, slice or cast ops; "overflow-checked or wrapping" is left open | New opcode families and a mode byte: checked, wrap, sat (02) |
| E-09 | PURE is 0x0000 yet listed beside other bits; DETERMINISTIC has no bit; one node is both DETERMINISTIC and STOCHASTIC_DIFF; SIMD is treated as an effect | 32-bit effect set: PURE is empty, determinism means no NONDET, SIMD is a target feature (04) |
| E-10 | ARENA_ALLOC(65536) mixes a capability bit with a byte budget | The ARENA bit plus a COST clause (04) |
| E-11 | Contracts must be quantifier-free, yet the examples use `forall` | Bounded `forall`, discharged by fresh-element proof rules (05) |
| E-12 | FOLD_BOUNDED cannot be verified without a loop invariant, and the grammar has none | A mandatory named INVARIANT (03, 05) |
| E-13 | Tier 3 has a 5 ms timeout, but the gate demands 2.5 ms p99 for all three tiers per token | SMT only at edge bindings, lazy and asynchronous; UNKNOWN never masks (07) |
| E-14 | A Z3 UNSAT core is called a proof certificate, replayable in under 10 ms | Pinned re-solving and dual-solver agreement now; Alethe certificates later (05, R-05) |
| E-15 | Wall-clock latency is stated as an SMT-provable postcondition | COST proves abstract bounds; BUDGET is measured per target (05, R-06) |
| E-16 | tanh appears in a float proof, but the solver's float theory has no transcendentals | Uninterpreted functions with axioms from a verified math library, plus a final clamp (05, R-10) |
| E-17 | Learnable parameters change at runtime while the proof covers one value; a hard threshold has no useful gradient | Proofs over the whole parameter box; gradient-free tuners for discrete ops (06, R-07) |
| E-18 | "0% runtime overhead" ignores guards that could not be proven | Residual guards counted per build; overhead gates of 20%, then 5% (06) |
| E-19 | "Mathematical immunity" to unauthorized network access holds only inside the language | OS sandbox manifests derived from the root's handles (04) |
| E-20 | SHA-256 lifted from C++ and from Python "converges to the same hash" | Spec classes: implementations proven to refine one SPEC node (09, R-02) |
| E-21 | Proof-carrying code makes supply-chain malware "mathematically impossible" | Effect confinement plus spec strength, attestations and a transparency log, with residual risks stated (09, R-12) |
| E-22 | Discovery in under 5 ms, including Z3 implication checks | Exact and indexed paths meet 5 ms; SMT checks are cached and asynchronous (09) |
| E-23 | Alive2 is cited as proving full equivalence of lifted libraries | Alive2 is bounded: round-trip checks and fuzzing now, invariants for full proofs (08, R-03) |
| E-24 | Self-hosting compares stage 1 with stage 2, and Rust "retires" while Z3 and LLVM stay | Compare stage 2 with stage 3; solvers and LLVM stay as host services (10) |
| E-25 | Gates that cannot be measured: "100% blocked", "repair in 1 iteration", "perf parity" | Gates with test suites, rates and bounds (ROADMAP.md) |
| E-26 | Per-token logit masking is assumed for any LLM, but hosted APIs do not expose it | Hosted mode through tools; masking with local open-weight models (07) |
| E-27 | A 70–85% token saving is asserted without measurement, and hex hashes are expensive tokens | Labels instead of hashes, and a token benchmark (R-09) |

Corrections to this specification itself, found after it was written, are recorded as ADRs (docs/adr/0002 to 0009), not here: this table lists only the source spec's errors.
