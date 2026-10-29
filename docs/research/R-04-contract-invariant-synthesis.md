# R-04 · Contract and invariant synthesis

**Question.** Can PRE, POST and FOLD_BOUNDED invariants be inferred reliably for lifted and AI-written code?
**Why it is research.** Invariant inference is undecidable in general; LLM guesses need a sound filter.
**Fallback that ships.** Hand-written INVARIANT clauses.  **Time-box:** 4 weeks.  **Starts after:** P3.

## Proposed solution

Gather candidates from interpreter traces, Daikon-style, and from an LLM. Prune with Houdini until the set is inductive, then check with SMT and refine on counterexamples. Score each contract's strength two ways, so weak specs become visible: how many mutants of the body it rejects, and how many mutated outputs it rejects for reference inputs (symbolic spec testing, as in Lahiri's FMCAD 2024 metric and nl2postcond), which needs no verifier run. For folds, try the functional invariant `acc == fold:f(init, xs, k, …)` first (ADR 0008): it is always inductive, so the search only needs the extra facts the POST requires.

## Go if

At least 70% of 100 folds get an inductive invariant proving the target POST within 2 minutes, and inferred contracts have a mean mutation score of at least 0.8.

## Starting points

- Daikon (dynamic invariant detection); Houdini (annotation pruning)
- [AutoVerus](https://arxiv.org/abs/2409.13082) (LLM proof synthesis for Rust)
- LLM-based loop invariant generation since 2024; Dafny and Verus proof-synthesis work, such as [AlphaVerus](https://arxiv.org/abs/2412.06176)
- [Evaluating LLM-driven User-Intent Formalization for Verification-Aware Languages](https://arxiv.org/abs/2406.09757) (Lahiri, FMCAD 2024) and [nl2postcond](https://arxiv.org/abs/2310.01831) (Endres et al.): measuring a specification's strength with inputs and mutated outputs

## Prompt

```text
Run research track R-04 (contract and loop-invariant synthesis) following docs/research/PROTOCOL.md.
Question: can PRE, POST and FOLD_BOUNDED invariants be inferred reliably for lifted and AI-written code?
Build behind feature r-04: (1) dynamic candidate invariants from vg-interp traces; (2) LLM-proposed candidates; (3) Houdini-style pruning to an inductive set, then SMT checking; (4) a counterexample-guided refinement loop; (5) a mutation-based contract-strength score.
Corpus: 100 folds from lifted code and from benchmarks/synthesis.
Measure: share of folds with an inductive invariant proving the target POST within 2 minutes; mean mutation score of inferred contracts; LLM calls per success.
Read first: Daikon, Houdini, AutoVerus, LLM-based loop invariant generation papers since 2024, Dafny and Verus proof-synthesis work.
Go if: at least 70% of folds proven, mean mutation score at least 0.8. Otherwise INVARIANT stays hand-written, with this tool as an assistant.
```
