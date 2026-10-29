# R-02 · Semantic convergence

**Question.** How often can independent implementations of the same function be recognized as equal, and at what cost?
**Why it is research.** Program equivalence is undecidable; structural hashes of independent implementations differ.
**Fallback that ships.** Spec classes by explicit proof (docs/09).  **Time-box:** 3 weeks.  **Starts after:** P7.

## Proposed solution

Three levels of sameness. The structural hash stays the identity. A second, normal-form hash comes from equality saturation over a fixed, terminating, bit-exact rewrite set with deterministic extraction, so it reproduces on every machine. Spec-class membership, proven by refinement queries at fixed sizes or for every size through a fold invariant (ADR 0008), is the level agents rely on. A cheap fourth signal finds candidate pairs: a behavioral fingerprint, the hash of a node's outputs on a fixed, seeded input vector per signature, as AlphaCode clustered programs by their outputs; equal fingerprints propose a refinement query, and different fingerprints refute equivalence at once.

## Go if

At least 50% of pairs converge by normal-form hash or are proven equal within 60 s each, and normal forms are bit-identical across runs and machines.

## Starting points

- [egg](https://arxiv.org/abs/2004.03082) (POPL 2021) and egglog ([Better Together: Unifying Datalog and Equality Saturation](https://arxiv.org/abs/2304.04332), PLDI 2023)
- [Hashing Modulo Alpha-Equivalence](https://arxiv.org/abs/2105.02856) (Maziarz et al., PLDI 2021), for formulas and kernels with binders
- [SAW](https://saw.galois.com) equivalence proofs
- [Unison's hashing of definitions](https://unison-lang.org/docs/language-reference/hashes)

## Prompt

```text
Run research track R-02 (semantic convergence) following docs/research/PROTOCOL.md.
Question: how often can independent implementations of the same function be recognized as equal, and at what cost?
Build behind feature r-02: (1) a corpus of at least 40 pairs of independent implementations (CRC-32, Adler-32, SHA-256 compression, popcount, saturating add, clamp, and more), lifted or hand-written; (2) an egg-based normalizer with a bit-exact rewrite set (no float reassociation), node and time limits, and deterministic extraction, producing a normal-form hash; (3) spec-class membership checks by refinement queries at fixed input sizes.
Measure: share of pairs that converge by normal-form hash, by proof, or not at all; time per pair; determinism of normal forms across machines and runs.
Read first: egg (POPL 2021), SAW equivalence proofs, Unison's hashing of definitions.
Go if: at least 50% of pairs converge or are proven equal within 60 s each, and normal forms are bit-identical across runs. Otherwise keep spec classes by explicit proof.
```
