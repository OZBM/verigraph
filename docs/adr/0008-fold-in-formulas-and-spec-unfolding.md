# 0008 · Folds in formulas, and unfolding SPEC nodes

Status: proposed on 2026-10-29 for the owner's approval; applied to docs 03, 05, 13, 14, tools/vgsym.lark and tools/check_examples.py.

## Context

Formulas could not say what a fold computes. An INVARIANT could bound an accumulator but not state that it equals "the CRC so far", and a `spec:` call was only an uninterpreted function constrained by its POST, which cannot describe its own recursion. So no quantifier-free query could prove that a fold-based implementation computes a spec: not `@sha256`'s `result == spec:sha256(arg0)`, on which M1's digest rests, and not the crc32 and adler32 spec classes of G8.

## Decision

- **`fold:f(init, xs, k, b…)`** is a formula term: the left fold of kernel @f over the first k elements of xs. vg-smt treats it as an uninterpreted function and adds, for each ground application, the instance of its defining equations at that k: `k = 0 ⇒ fold = init`, and `0 < k ≤ len(xs) ⇒ fold:f(init, xs, k, b…) = f(fold:f(init, xs, k − 1, b…), xs[k − 1], b…)`. Instances are never chained further, so queries stay quantifier-free and finite.
- **Kernel applications inside formulas are inlined**: kernels are non-recursive and fixed-size, so f(…) becomes the exact SMT term of its body.
- **A `spec:` application unfolds one level**: it equals the SPEC node's body applied to the arguments, where each FOLD_BOUNDED in that body becomes a `fold:` term and each SIMD_MAP is instantiated at the indices the query reads. The SPEC's PRE and POST are added as facts too.
- Grammar: `Call ::= (("spec" | "fold") ":")? Ident "(" … ")"`; `fold` becomes a reserved word.

## Consequences

The natural invariant of any fold, `acc == fold:f(init, xs, k, b…)`, can be written and checked by the FOLD rule. Equivalence between a table-driven and a bitwise CRC reduces to the FOLD.step query `tbl_step(a, x) == bit_step(a, x)`, a quantifier-free bitvector query. Relational invariants, such as zlib's deferred modulo in adler32, use the same vocabulary. This is the induction principle of lists made available to agents: lifts and specs share one logic, and the queries remain the quantifier-free fragment all three solvers accept.
