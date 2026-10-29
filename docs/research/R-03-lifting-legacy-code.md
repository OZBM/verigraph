# R-03 · Lifting legacy code

**Question.** Can real C and Rust functions with loops be lifted into idiomatic VG-IR combinators with machine-checked equivalence?
**Why it is research.** Loop equivalence needs invariants; Alive2 is bounded; pointer-heavy code resists functional lifting.
**Fallback that ships.** Mechanical lift with bounded checks and fuzzing (docs/08).  **Time-box:** 6 weeks.  **Starts after:** P8.

## Proposed solution

Keep the mechanical lift as the reference, then recognize map, fold, scan and partition idioms in loop-closed SSA. Where patterns fail, an LLM proposes a combinator form and the verifier checks it, as in LLMLift. Unbounded equivalence comes from relational invariants over synchronized loops, supplied by R-04. Pointers become (buffer, offset) pairs with bounds, and aliasing assumptions become PRE.

## Go if

At least 60% of the corpus lifted, 80% of those with an idiomatic form, and 100% passing bounded checks and fuzzing.

## Starting points

- [Alive2](https://www-old.cs.utah.edu/~regehr/alive2-pldi21.pdf) (PLDI 2021)
- [Verified Code Transpilation with LLMs](https://neurips.cc/virtual/2024/poster/93370) (NeurIPS 2024)
- Tenspiler, C2TACO, [SAW](https://saw.galois.com)

## Prompt

```text
Run research track R-03 (lifting legacy code) following docs/research/PROTOCOL.md.
Question: can real C and Rust functions with loops be lifted into idiomatic VG-IR combinators with machine-checked equivalence?
Build behind feature r-03: (1) idiom recognition for map, fold, scan and partition over LCSSA-form LLVM IR; (2) an LLM-propose, verifier-check path for loops the patterns miss; (3) unbounded equivalence via relational invariants over synchronized loops (product programs), using R-04's invariant synthesis when it exists; (4) UB curing that reports each UB input found.
Corpus: zlib crc32.c and adler32.c, one C SHA-256, the portable backend of Rust's sha2 crate, plus 20 loops from other permissively licensed C libraries.
Measure: share lifted, share with an idiomatic form, bounded Alive2 results at unroll 8 and above, 1e6 differential fuzz cases per function, share with unbounded proofs, time per function.
Read first: Alive2 (PLDI 2021), Verified Code Transpilation with LLMs (NeurIPS 2024), Tenspiler, C2TACO, SAW.
Go if: at least 60% lifted, 80% of those idiomatic, 100% pass bounded checks and fuzzing. Otherwise keep the mechanical lift with bounded checks.
```
