# R-13 · Dynamic-language lifting

**Question.** Can pure Python and JS numeric and string code be lifted into VG-IR with useful contracts?
**Why it is research.** Dynamic types, implicit conversions and library semantics (NumPy) have no static IR to start from.
**Fallback that ships.** Lift the libraries' C cores through OmniLift's C path.  **Time-box:** 6 weeks.  **Starts after:** R-03.

## Proposed solution

Restrict lifting to a typed, pure subset of Python and JavaScript. Infer types from test-suite traces and concolic exploration, give NumPy primitives their own spec library, and reuse OmniLift's two-step lift. Equivalence rests on differential testing plus bounded symbolic checks.

## Go if

At least 30 functions lifted with 100% differential agreement over 1e5 inputs each, and at least half with a non-trivial POST.

## Starting points

- CrossHair (Python symbolic execution); concolic testing
- Python type inference from traces; LLM-assisted transpilation with verification

## Prompt

```text
Run research track R-13 (Python and JavaScript lifting) following docs/research/PROTOCOL.md.
Question: can pure Python and JS numeric and string code be lifted into VG-IR with useful contracts?
Build behind feature r-13: (1) Tree-sitter front ends for a pure, typed subset; (2) type inference from test-suite traces plus concolic exploration (CrossHair for Python); (3) a spec library for the NumPy primitives used; (4) the two-step lift (mechanical, then idiomatic) and contract inference from R-04.
Corpus: 30 or more functions from two permissively licensed pure-Python libraries and one JS library.
Measure: share lifted, differential agreement over 1e5 inputs per function, share with a non-trivial POST.
Read first: CrossHair, concolic testing, Python type inference from traces, LLM-assisted transpilation with verification.
Go if: at least 30 functions lifted with 100% differential agreement and at least half with a non-trivial POST. Otherwise lift the libraries' C cores through the C path.
```
