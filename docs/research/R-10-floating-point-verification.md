# R-10 · Floating-point verification

**Question.** Which numeric contracts can be discharged quickly and soundly, and how should transcendental functions be modeled?
**Why it is research.** SMT float reasoning is slow, and solver float theories have no transcendentals.
**Fallback that ships.** Clamp and guard patterns plus testing.  **Time-box:** 4 weeks.  **Starts after:** P3.

## Proposed solution

Tier the numeric checks: interval analysis first, because it is cheap and sound; then Bitwuzla on small float kernels; error-bound tools for accuracy claims. Transcendentals become uninterpreted functions whose range, monotonicity and error axioms come from a correctly rounded math library. Exhaustive f16 testing guards against false proofs. A fourth, compositional tier is worth testing first for accuracy claims: type systems that bound rounding error by construction, with a graded monad over a sensitivity analysis (NumFuzz) or graded coeffects for backward error (Bean). They check in linear time, need no solver, and give agents an error budget per op, which suits VeriGraph's local typing.

## Go if

At least 80% of numeric obligations discharged within 10 s, and zero false proofs.

## Starting points

- SMT-LIB floating-point theory; Bitwuzla
- FPTaylor, Gappa, Daisy; CORE-MATH (correctly rounded libm)
- [Numerical Fuzz: A Type System for Rounding Error Analysis](https://arxiv.org/abs/2405.04612) (Kellison and Hsu, 2024) and [Bean: A Language for Backward Error Analysis](https://arxiv.org/abs/2501.14550) (Kellison et al., 2025)

## Prompt

```text
Run research track R-10 (floating-point and numeric verification) following docs/research/PROTOCOL.md.
Question: which numeric contracts can be discharged quickly and soundly, and how should transcendental functions be modeled?
Build behind feature r-10: (1) interval and abstract-interpretation passes for float ranges; (2) routing of small float kernels to Bitwuzla; (3) accuracy postconditions checked with an error-bound tool (FPTaylor, Gappa or Daisy); (4) axioms for tanh, exp, log and sin from a correctly rounded library's contracts; (5) NaN and infinity lints.
Validation: exhaustive testing over all f16 inputs for unary kernels, and 1e7 random f32 inputs otherwise; any disagreement with a proof is a soundness bug.
Measure: share of numeric obligations discharged within 10 s, time per obligation, false proofs found.
Read first: SMT-LIB floating-point theory, Bitwuzla, FPTaylor, Gappa, Daisy, CORE-MATH.
Go if: at least 80% discharged within 10 s and zero false proofs. Otherwise keep clamp and guard patterns plus testing.
```
