# 0006 · Counterexample replay, contract gaps and unknown verdicts

Status: proposed on 2026-10-29 for the owner's approval; applied to docs 05, 07 and 15.

## Context

docs/05 said that a counterexample which does not reproduce in vg-interp is a verifier bug and stops the build. That is right only when the query encodes the node exactly. Modular verification abstracts on purpose: a callee is replaced by its contract, a `spec:` call by an uninterpreted function, and an assumed `forall` is instantiated only at some indices. A model that picks a callee result its contract allows, but its body never returns, does not reproduce, and that is no bug: the caller's proof genuinely does not follow from the contracts. A model with a slice of 2^62 elements cannot be replayed at all.

## Decision

On SAT, vg-smt first looks for a small model, re-solving with every slice length bounded by 4, then 16, then 256, and keeping the first that is SAT. Then it replays twice:

1. **Model-guided replay** runs the node in vg-interp with the model's inputs and θ and with the model's values for every abstracted term (callee results, `spec:` and `fold:` applications, formula division by zero). It also checks every assumption in full. If an assumption that was only partly instantiated fails, the verdict is `unknown` (VG-V0006). Otherwise the violation must reproduce; if it does not, the encoding is wrong, which is a verifier bug (VG-V0008) and stops the build.
2. **Concrete replay** runs the same inputs with the real callees. A reproduction is a `counterexample` (VG-V0001). Otherwise the verdict is `contract_gap` (VG-V0002): the obligation does not follow from the callees' contracts, though this input runs correctly. The report lists the values the model chose.

A model too large to materialize is `unknown` (VG-V0007). In release mode, if one solver returns UNSAT and another a model that reproduces, the build stops (VG-V0009).

## Consequences

Agents are told which of three repairs applies: fix the body (counterexample), strengthen a callee's POST or the node's PRE (contract gap), or split the obligation or add a guard (unknown). The "stop the build" rule now fires only on real verifier bugs, and G3's requirement that counterexamples reproduce means concrete reproduction. None of this weakens soundness: only UNSAT proves.
