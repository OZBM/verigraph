# R-08 · Counterfactual telemetry

**Question.** Which failure reports make AI repair fastest?
**Why it is research.** Minimal slices and constraint diffs are well studied for humans, not for LLM repair loops.
**Fallback that ships.** Counterexample plus backward slice (docs/07).  **Time-box:** 3 weeks.  **Starts after:** P6.

## Proposed solution

Treat the failure report as a product to optimize. Compare five variants: the raw counterexample, plus a backward slice, plus a delta-debugged minimal slice, plus an unsat-core constraint diff, plus a minimal-correction-set "smallest change that fixes it". Keep whichever cuts repair rounds most for real models.

## Go if

The best variant cuts mean repair rounds by at least 25% versus the raw counterexample; slices have 5 ops or fewer on 80% of bugs; reports take under 200 ms.

## Starting points

- Delta debugging (Zeller); dynamic slicing
- MUS and MCS extraction; LLM program repair with verifier feedback
- [Counterexamples as Feedback for Agent Self-Correction](https://arxiv.org/abs/2609.02892) (2026): counterexample feedback solved 90% of tasks within four turns, error-only feedback 23%
- [CausalRepair](https://arxiv.org/abs/2608.10613) (2026): minimal causal context by dual slicing; [ExVerus](https://arxiv.org/abs/2603.25810) (2026): counterexamples generalized into invariants; [MaxSAT-based fault localization for repair](https://arxiv.org/abs/2502.07786) (2025)

## Prompt

```text
Run research track R-08 (counterfactual telemetry) following docs/research/PROTOCOL.md.
Question: which failure reports make AI repair fastest?
Build behind feature r-08: report variants (1) raw counterexample; (2) plus backward slice; (3) plus delta-debugged minimal slice; (4) plus unsat-core constraint diff; (5) plus minimal correction set; and runtime capture through the guard map with size caps and redaction.
Benchmark: 60 seeded bugs across arithmetic, bounds, floats, effects and folds; two models (one hosted, one open-weight).
Measure: repair success and mean rounds per variant, slice size, report generation time.
Read first: delta debugging (Zeller), dynamic slicing, MUS and MCS extraction, LLM program repair with verifier feedback.
Go if: the best variant cuts mean repair rounds by at least 25% versus the raw counterexample, slices have 5 ops or fewer on 80% of bugs, reports take under 200 ms. Otherwise ship counterexample plus backward slice.
```
