# Research tracks

Thirteen claims in the source spec are research, not engineering, and R-14 covers a risk the source never named. Each is a time-boxed track with a proposed solution, a fallback the MVP already ships, and a prompt that has an AI do the research, build a prototype and report a go or no-go. No track ever blocks the MVP. Every track follows PROTOCOL.md.

| Track | Question | Fallback that ships | Time-box | Starts after |
| --- | --- | --- | --- | --- |
| [R-01](R-01-smt-guided-decoding.md) | Can SMT masking run inside decoding fast enough, without hurting quality? | Hosted generate-verify-repair loop | 4 weeks | Agent loop (P5) |
| [R-02](R-02-semantic-convergence.md) | How often can independent implementations be recognized as one function? | Spec classes by explicit proof | 3 weeks | VG-Mesh lite (P7) |
| [R-03](R-03-lifting-legacy-code.md) | Can real C and Rust loops be lifted into idiomatic combinators with checked equivalence? | Mechanical lift, bounded checks, fuzzing | 6 weeks | OmniLift lite (P8) |
| [R-04](R-04-contract-invariant-synthesis.md) | Can contracts and loop invariants be inferred reliably? | Hand-written INVARIANT clauses | 4 weeks | Verification (P3) |
| [R-05](R-05-proof-certificates.md) | Can every proof ship as a certificate a small checker verifies? | Pinned re-solving, two solvers agreeing | 3 weeks | Verification (P3) |
| [R-06](R-06-resource-latency-contracts.md) | How tight can cost bounds get, and how do they become honest latency claims? | COST for memory, BUDGET by benchmark | 3 weeks | Codegen (P4) |
| [R-07](R-07-learnable-parameters.md) | How can parameters be tuned online without breaking proofs or determinism? | Frozen parameter files | 4 weeks | Alpha learnable params (P9) |
| [R-08](R-08-counterfactual-telemetry.md) | Which failure reports make AI repair fastest? | Counterexample plus backward slice | 3 weeks | Telemetry (P6) |
| [R-09](R-09-token-efficiency.md) | Is VG-Sym really cheaper in tokens, and how well do models write it? | Current syntax, in-context skill | 4 weeks | Agent loop (P5) |
| [R-10](R-10-floating-point-verification.md) | Which numeric contracts can be proven fast, and how are transcendentals modeled? | Clamp and guard patterns, testing | 4 weeks | Verification (P3) |
| [R-11](R-11-self-hosting-feasibility.md) | What is the smallest language extension for writing the compiler in VeriGraph? | Rust stays the production compiler | 4 weeks | Alpha (M2) |
| [R-12](R-12-mesh-at-scale.md) | How does a public, federated Mesh stay trustworthy and spam-resistant? | One signed registry over HTTP | 4 weeks | Alpha (M2) |
| [R-13](R-13-dynamic-language-lifting.md) | Can pure Python and JS code be lifted with useful contracts? | Lift the C cores instead | 6 weeks | R-03 |
| [R-14](R-14-intent-validation.md) | Which way of confirming an AI-written contract catches the most wrong contracts per human answer? | Plain-language rendering and the contract card | 4 weeks | Agent loop (P5) |

[SURVEY-2026-10.md](SURVEY-2026-10.md) lists the recent work, checked against its primary source, that shaped docs/13 to docs/16 and these tracks.
