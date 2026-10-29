# R-05 · Proof certificates

**Question.** Can every obligation ship with a certificate that a small, independent checker verifies fast enough for Mesh ingestion?
**Why it is research.** An UNSAT core is not a proof; proof formats differ by solver and theory, and proofs can be large.
**Fallback that ships.** Pinned re-solving plus two-solver agreement (docs/05).  **Time-box:** 3 weeks.  **Starts after:** P3.

## Proposed solution

Route obligations to cvc5 with Alethe proof output and check them with Carcara. For pure bitvector obligations, bit-blast to SAT and check LRAT proofs with a verified checker. Anything unsupported falls back to pinned re-solving, and the proof record says which kind of evidence it carries.

## Go if

At least 90% of obligations certified, median check under 50 ms, p99 under 2 s, storage under 10× node size.

## Starting points

- [Carcara](https://link.springer.com/chapter/10.1007/978-3-031-30823-9_19) (TACAS 2023), the Alethe format, cvc5's proof documentation
- LRAT and the verified checker cake_lpr

## Prompt

```text
Run research track R-05 (independently checkable proof certificates) following docs/research/PROTOCOL.md.
Question: can every obligation ship with a certificate that a small, independent checker verifies fast enough for Mesh ingestion?
Build behind feature r-05: (1) cvc5 proof production in Alethe format for each obligation; (2) Carcara checking in vg-smt and in the Mesh ingestion gate; (3) for pure bitvector obligations, bit-blasting to SAT with LRAT proofs checked by a verified checker; (4) proof storage keyed by obligation hash, with compression.
Measure on the full test corpus: share of obligations certified, proof size relative to node size, median and p99 check time, solver slowdown with proofs on.
Read first: Carcara (TACAS 2023), cvc5 proof documentation, Alethe format specification, LRAT and cake_lpr.
Go if: at least 90% certified, median check under 50 ms, p99 under 2 s, storage under 10x node size. Otherwise keep re-solving plus two-solver agreement.
```
