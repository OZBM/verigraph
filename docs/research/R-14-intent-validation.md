# R-14 · Intent validation

**Question.** Which way of confirming an AI-written contract catches the most wrong contracts per answer a human gives?
**Why it is research.** No algorithm can decide whether a formal contract matches informal intent (Lahiri, FMCAD 2024). Verification moves the risk from the code to the contract, and in VeriGraph an AI writes both; a weak or wrong POST verifies just as well as a right one.
**Fallback that ships.** The toolchain's plain-language rendering of every contract (docs/15) and the contract card the human confirms (docs/07).  **Time-box:** 4 weeks.  **Starts after:** P5.

## Proposed solution

Layer three checks on the contract card and measure each one's catch rate. First, deterministic rendering in plain words, already in the MVP. Second, distinguishing examples: the solver proposes inputs with outputs the contract allows, at boundary values, and near-miss outputs it forbids; the human answers yes or no to each, as TiCoder does with tests. Where two independently drafted contracts exist (two models, or a model and the human's earlier answers), the solver finds an input on which they disagree and asks about that one. Third, symbolic spec testing: a contract is sound if it accepts reference input-output pairs and complete to the degree it rejects mutated outputs, the metric of Lahiri's FMCAD 2024 paper and of nl2postcond. Mutated outputs are cheaper than R-04's mutated bodies, because they need no verification run.

## Go if

The best format catches at least 90% of seeded wrong contracts with at most 5 yes-or-no answers per task, and flags at most 5% of right contracts as wrong.

## Starting points

- [Evaluating LLM-driven User-Intent Formalization for Verification-Aware Languages](https://arxiv.org/abs/2406.09757) (Lahiri, FMCAD 2024)
- [Interactive Code Generation via Test-Driven User-Intent Formalization](https://arxiv.org/abs/2208.05950) (Lahiri et al., TiCoder)
- [Can Large Language Models Transform Natural Language Intent into Formal Method Postconditions?](https://arxiv.org/abs/2310.01831) (Endres et al., nl2postcond)
- [Clover: Closed-Loop Verifiable Code Generation](https://arxiv.org/abs/2310.17807) (Sun et al.): consistency checks among code, docstring and annotations

## Prompt

```text
Run research track R-14 (intent validation) following docs/research/PROTOCOL.md.
Question: which way of confirming an AI-written contract catches the most wrong contracts per answer a human gives?
Build behind feature r-14: (1) the contract renderer of docs/15, with golden tests; (2) an example generator that asks the solver for inputs and outputs the contract allows at boundary values, and for near-miss outputs it forbids; (3) a disagreement finder that, given two contracts for one task, asks the solver for an input on which they differ; (4) symbolic spec testing: soundness against reference input-output pairs and completeness against mutated outputs; (5) a simulated user that answers from the task's reference contract.
Benchmark: benchmarks/synthesis, 30 tasks, each with its reference contract and 3 seeded plausible but wrong contracts (an off-by-one bound, a missing relational fact, a weakened POST).
Measure for each format (rendering alone; rendering plus examples; disagreement queries; spec testing): wrong contracts caught, right contracts flagged, questions per task, solver time.
Read first: Lahiri (FMCAD 2024), TiCoder, nl2postcond, Clover.
Go if: the best format catches at least 90% of wrong contracts with at most 5 yes-or-no answers per task and flags at most 5% of right ones. Otherwise ship rendering alone, and keep the contract card as the human's checkpoint.
```
