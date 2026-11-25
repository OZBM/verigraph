# 12 · Risks, budget and open questions

The gravest risk is a verifier that accepts wrong programs; the largest cost is agent tokens, and the gate scripts exist to stop them being spent on rework.

| Risk | Likelihood | Impact | Mitigation |
| --- | --- | --- | --- |
| A verifier soundness bug | Medium | Severe | Known-bad and mutation suites, two solvers, interpreter replay of every counterexample, certificates from R-05 |
| Solver timeouts on real programs | High | Medium | Modular contracts, fresh-element rules, a query cache, explicit guards as the fallback |
| Models write poor VG-Sym, having never seen it | High | High | A synthesis skill with tested examples, the repair loop, R-09's corpus and fine-tune |
| LLVM or clang version drift | Medium | Low | Textual IR, a pinned clang, a CI matrix |
| A research track fails | Medium, per track | Low for the MVP | Every track has a fallback that already ships |
| Agent costs overrun | Medium | Medium | Gate scripts catch rework early; parallel agents only after G0; a token ceiling per gate |
| Scope creep from the vision | High | Medium | The [MVP], [ALPHA] and [R-xx] labels, and the smallest-complete-change rule |
| Format churn after G1 invalidates hashes | Low | High | Format freeze, with an ADR and a version bump for any change |
| A contract that does not say what the human meant: verified, and wrong | High | Severe | The toolchain renders every contract in plain words for confirmation (docs/15); contract strength scores (R-04); intent validation by solver-chosen examples (R-14) |
| Floats differ between targets | Medium | High | One NaN written canonically (ADR 0002), the default floating-point environment checked at every entry, differential tests on three targets |
| A wrong proof fact silently miscompiles | Low | Severe | Builds with and without proof facts compared against vg-interp (docs/16); checked entries at the trust boundary (ADR 0009) |
| Spurious counterexamples send the agent the wrong way | Medium | Medium | Model-guided and concrete replay, with contract_gap and unknown verdicts (ADR 0006) |

## Budget

- **Agent tokens** dominate the cost. Log them per phase in PROGRESS.md and set a ceiling per gate.
- **Local models** can carry the bulk experiments of R-01 and R-09 at no API cost. A 7–14B code model runs on an Apple Silicon Mac with 32 GB of memory or more, or on one 24 GB GPU.
- **CI** needs macOS ARM64 and Linux x86_64 runners. Alive2 needs a one-time LLVM build of a few hours.

## Open questions for the owner

- [x] License: Apache-2.0, MIT, or both? MIT OR Apache-2.0, dual (owner, 2026-11-25).
- [ ] Start date and agent count: ROADMAP.md assumes 26 October 2026 and up to six build agents in parallel.
- [ ] Which hosted model drives the G5 benchmark?
- [x] Public or private repository during the MVP? Private until M1, then public (owner, 2026-10-29); made public before M1 once the license was settled (owner, 2026-11-25).
- [ ] Is "VeriGraph" free to use as a project name?
- [ ] Approve ADRs 0002 to 0009, written on 2026-10-29 and already applied to the spec draft, before G1 freezes the format.
- [ ] x86-64-v3 (AVX2, FMA) as the Linux x86_64 baseline, so FMA is one instruction?
- [ ] Add docs/16's distinctness families to G1, and define G5 success as verifying against each task's reference contract?
