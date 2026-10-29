# 0010 · Well-defined contracts

Status: proposed on 2026-10-29 for the owner's approval; applied to docs 05, 13 and 15.

## Context

A formula can read a value the logic leaves unspecified: `x / y` with y = 0, `xs[i]` out of range, `ok(r)` of an Err, a `spec:` call outside its PRE, `fold:` beyond the end of its collection. The solver may choose anything there, so no proof is unsound, but a contract such as `POST: result == x / y` with no guard fails with a counterexample that blames the body for what is a hole in the contract, and the concrete pre-screen cannot even evaluate it. Dafny avoids this by checking that its specifications are well-formed; Gemini's spec audit asked for the same.

## Decision

- Each formula term that can read an unspecified value adds a **well-definedness obligation**: the divisor is not zero, the index is in range, the Result has the right tag, the `spec:` arguments satisfy its PRE, the `fold:` count is at most the length.
- It is proven where the term occurs: under the left operand for the right operand of `&&`, under the negated left operand for the right operand of `||`, under the antecedent for the consequent of `==>`, under the range bounds inside `forall`, and under the clause's own hypotheses (PRE for POST and COST; PRE, `0 ≤ k ≤ len` and the earlier conjuncts for an INVARIANT; only the earlier conjuncts for PRE itself).
- Its ID extends the clause's: `@n/POST.0.wd.j`, with j numbering the terms of that conjunct in pre-order.

## Consequences

A contract never relies on an unspecified value, so the pre-screen can evaluate every clause on every input that satisfies PRE, and a failure points at the clause, with a message such as "POST.0 divides by y, which may be 0: guard it with `y != 0 ==>`". Agents write the guard once instead of chasing a counterexample through the body.
