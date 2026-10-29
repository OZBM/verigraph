# R-07 · Learnable parameters under contracts

**Question.** How can parameters be tuned online without breaking proofs, determinism or stability?
**Why it is research.** Proofs must cover every tuned value; discrete combinators have no useful gradient; online learning can oscillate.
**Fallback that ships.** Frozen parameter files.  **Time-box:** 4 weeks.  **Starts after:** learnable parameters land in P9.

## Proposed solution

Prove every contract for all θ in the declared box; when a proof fails, split the box, prove the parts that hold and shrink the declared box to them. Smooth kernels use forward-mode autodiff; discrete ops use relaxed surrogates or gradient-free tuners such as SPSA and CMA-ES. Updates are projected into the box, stepped conservatively, and rolled back when the loss regresses.

## Go if

Loss improves at least 20% over default parameters in simulation, zero contract violations over 1e6 steps, tuner overhead under 5% CPU.

## Starting points

- JAX autodiff through control flow; Enzyme (LLVM-level autodiff)
- SPSA, CMA-ES; safe and constrained online optimization surveys

## Prompt

```text
Run research track R-07 (learnable parameters under contracts) following docs/research/PROTOCOL.md.
Question: how can parameters be tuned online without breaking proofs, determinism or stability?
Build behind feature r-07: (1) verification over the whole parameter box, with box splitting and shrinking when a proof fails; (2) a forward-mode autodiff transform of pure graphs; (3) gradient-free tuners (SPSA, CMA-ES, a bandit) for discrete ops; (4) a tuner node with projected updates, bounded steps and rollback on loss regression; (5) Distribution<T> and SAMPLE under NONDET with seeded RNG for tests.
Demos: the audio limiter and a bounded frame scheduler, in simulation.
Measure: loss improvement versus default parameters, contract violations over 1e6 steps, tuner CPU overhead, reproducibility with frozen parameters.
Read first: JAX autodiff through control flow, Enzyme, SPSA, CMA-ES, safe and constrained online optimization surveys.
Go if: loss improves at least 20%, zero violations, overhead under 5%. Otherwise ship frozen parameter files only.
```
