# 05 · Contracts and verification

Every obligation becomes one quantifier-free SMT query. UNSAT means proven; SAT returns a counterexample that vg-interp must reproduce; UNKNOWN never counts as a proof.

## Clauses

| Clause | What it says | How it is checked |
| --- | --- | --- |
| PRE | What callers must guarantee | At every call site, against the caller's facts |
| POST | What the node guarantees | Once, against the body |
| INVARIANT @name(k, acc) | What holds after k steps of a FOLD_BOUNDED | A base case and one inductive step |
| DECREASES | A natural-number measure for REC_CALL | Strictly smaller at each recursive call |
| COST | Static bounds: heap, arena and stack bytes, op counts | Cost semantics summed over the graph |
| BUDGET | Measured targets such as `latency_p99_us` | Benchmarks per target; never claimed as a proof |

## Logic

Formulas have the types of docs/03: mathematical integers, IEEE floats, arrays and uninterpreted functions. vg-smt encodes each integer term as a bitvector wide enough that no operation in its formula can overflow, a width that follows from the operand types and literal sizes (`len` fits in 64 bits), so the encoding is exact and no query uses the SMT-LIB `Int` or `Real` sorts. Every query is quantifier-free bitvectors, floats, arrays and uninterpreted functions, which z3, cvc5 and bitwuzla all accept, so any two of them can check a release proof. A `spec:` call names an executable reference spec; the solver sees it as an uninterpreted function whose PRE and POST are added as facts at each application and whose body is unfolded one level there, and vg-interp can run it. A `fold:` term is an uninterpreted function with its two defining equations instantiated at each application (ADR 0008), and a kernel applied inside a formula is inlined as the exact term of its body, where an op outside its domain yields an unspecified value (an uninterpreted function of its operands), never the wrapped one. Transcendentals such as tanh are axiomatized the same way, from a verified math library's contracts (R-10). LEARNABLE parameters are universally quantified over their declared box, so every proof holds for every tuned value.

**Quantifiers.** A `forall` in a goal becomes fresh constants: the query asks for one index where it fails. A `forall` in an assumption is instantiated at every index term of the same collection that occurs in the query, in a fixed order: this is the fresh-element rule the combinator rules below rely on. In general it may be partial, which is sound, because dropping instances only weakens what is assumed; a model that violates an assumption instantiated this way is reported as `unknown` (VG-V0006), never as a counterexample (ADR 0006).

**Integer widths.** Every integer term is a signed bitvector wide enough that it never reaches the most negative value of its width, so negation and division cannot overflow. An uN or iN value takes N+1 bits, `len` takes 65 like a u64, a literal k takes bits(|k|) + 1, and a range variable the wider of its bounds. Then a ± b takes max(w(a), w(b)) + 1, a × b takes w(a) + w(b), −a, abs(a) and a / b keep w(a), a % b takes min(w(a), w(b)), and min and max take the wider operand. Each operator sign-extends its operands to the widest of their widths and its result width, computes there, and keeps its result's width, which always holds the value; a comparison extends both sides to the wider one. A width that is too narrow is a soundness bug, because a wrapped sum can make a false POST look proven. These widths are enough by induction: every term's magnitude stays below 2^(w−1) for its width w, since a sum of two such terms stays below 2^max(wa, wb), a product below 2^(wa+wb−2), and negation, abs, division and remainder never grow a magnitude; so no term ever reaches the most negative value of its width. A division or remainder by zero in a formula has an unspecified value, as SMT-LIB's integers do: vg-smt encodes it as an uninterpreted function of the dividend, and the well-definedness obligations of ADR 0010 keep any contract from reading it.

## Combinator proof rules

| Op | The caller must prove | The result guarantees |
| --- | --- | --- |
| SIMD_MAP(f, xs, b…) | For a fresh element e of xs: the known facts about e imply PRE_f(e, b) | POST_f(xs[i], b, ys[i]) for every index i |
| FOLD_BOUNDED(f, init, xs), invariant I | I(0, init); for fresh k < n, acc and r: I(k, acc) implies PRE_f(acc, xs[k]), and I(k, acc) with POST_f(acc, xs[k], r) implies I(k+1, r) | I(n, result) |
| GENERATE(f, n) | PRE_f(i) for a fresh i < n | POST_f(i, ys[i]) for every i < n |
| SCAN_PREFIX(⊕, xs) | ⊕ is associative, proven once for fresh a, b, c | ys[k] is the ⊕-fold of xs[0..=k] |
| PARTITION_BY(p, xs) | p is total on the elements | Kept elements satisfy p, the rest fail it, and none are lost |
| MATCH_DISJOINT(v, arms) | The arm guards are exhaustive and pairwise disjoint | POST of the arm taken |
| ASSERT_GUARD(c, @f, a…, else=@g) | PRE_f(a…) under c; PRE_g(a…) under ¬c | result = c ? f(a…) : g(a…), and only that branch runs; POST_f holds under c, POST_g under ¬c; the guard is erased when c is proven |
| CALL_HASH(f, args) | PRE_f(args) | POST_f(args, result); the callee's body never enters the proof |
| INDEX(xs, i) | i < len(xs) | The element |
| UPDATE(xs, i, v) | i < len(xs) | xs with element i replaced by v |
| SLICE(xs, lo, hi) | lo ≤ hi ≤ len(xs) | A view of the hi − lo elements from lo |
| ADD.checked, MUL.checked, … | No overflow | The exact result |
| DIV.checked, MOD.checked | Divisor ≠ 0; for signed types, not MIN / −1 | The exact result |

**The fresh-element rule** keeps queries quantifier-free: a `forall` over an array becomes one query about an arbitrary element, so contracts may quantify while the solver never has to. Facts about whole arrays enter the context as per-element facts on that fresh element. For a `Tensor<T, [d1, …, dn]>`, an index is a tuple (i1, …, in) with 0 ≤ ik < dk, the fresh element sits at one fresh index tuple, and SIMD_MAP returns a tensor of the same shape.

**ASSERT_GUARD is the one lazy combinator.** The graph is strict, so every other op's obligations must hold on every path. An op that is safe only under a condition, such as an INDEX into a slice that may be empty, goes in a kernel called through ASSERT_GUARD, where its PRE is checked under that condition (pattern in skills/verigraph-synthesis/SKILL.md). Plain ASSERT_GUARD is a safety guard: its else branch is a failure path, so while c is unproven the guard counts as residual cost and its else branch reports at runtime (docs/06, docs/07). `ASSERT_GUARD.branch` has the same proof rule but marks a branch the program means to take, such as `@first_or_zero`'s empty slice: it never counts as a residual guard, and its else branch reports nothing.

## Pipeline

1. vg-smt walks the canonical graph once and builds a symbolic value for every port.
2. **Concrete pre-screen.** Before any solver runs, vg-interp executes the node on a fixed, seeded set of inputs: boundary values (0, ±1, each integer type's MIN and MAX, −0.0, subnormals, the largest finite floats, the literals PRE compares against, the corners of every LEARNABLE box, slice lengths 0, 1, 2, 3 and 16), first each parameter's values with the others at a base value, then 256 random combinations seeded with BLAKE3 in derive-key mode (`VeriGraph 2026 prescreen v1`) over the node_hash, so every machine tries the same inputs. Each input that satisfies PRE is checked against POST, and every FOLD_BOUNDED invariant is checked at each step k. A failure is a counterexample at once, reported like a solver's and with the step k for an invariant, so a draft like examples/soft_knee_draft.vgs fails without waiting on a solver. A pass proves nothing.
3. **Non-vacuity.** A contract that no input can satisfy makes every obligation trivially UNSAT, so each node's PRE, input refinements and LEARNABLE box must be shown satisfiable: by an input from step 2, or else by a solver model that replays in vg-interp. A contract shown unsatisfiable is rejected as vacuous, and one whose check stays unknown fails too.
4. Each obligation becomes `assumptions ∧ ¬goal`, printed as SMT-LIB2 and hashed. The hash keys a cache, so an unchanged obligation is never solved twice. The printed text is canonical, so equal obligations get equal hashes on every machine: symbols are named from canonical positions (`a3` for input 3, `t0` for θ 0, `p12_0` for result 0 of op 12, `k0` for a fresh index); declarations are sorted; only assumptions that share symbols with the goal, closed under sharing, are kept, so an edit elsewhere in the node does not invalidate the cache; and the `set-logic`, option and seed lines are fixed in P3.3 and pinned in toolchain.lock with the solver versions.
5. Each solver runs as one long-lived process over pipes. An independent obligation starts from `(reset-assertions)`, and `push` and `pop` serve only checks that share a context. Z3 by default, Bitwuzla for float queries, cvc5 when a proof certificate is wanted. Per-query timeout: 2 s in the edit loop, 60 s in CI, both configurable.
6. On SAT, vg-smt looks for a small model first, bounding every slice length by 4, then 16, then 256. The model is decoded into VG values and replayed twice (ADR 0006). **Model-guided replay** uses the model's values for every abstracted term (callee results, `spec:` and `fold:` applications) and checks every assumption in full; if the violation does not reproduce, the encoding is wrong: that is a verifier bug (VG-V0008), and it stops the build. **Concrete replay** runs the real callees: a reproduction is a `counterexample`; otherwise the verdict is `contract_gap`, meaning the obligation does not follow from the callees' contracts, and the report shows the values the model chose for them.
7. On UNKNOWN, a timeout, a model that violates a partly instantiated assumption, or a model too large to replay, the obligation stays unproven, with that reason. The node fails unless the author moves the operation behind an explicit ASSERT_GUARD on that condition, which is then counted as residual runtime cost.

| Verdict | Means | Typical repair |
| --- | --- | --- |
| proven | UNSAT, under the pinned solvers | — |
| counterexample | refuted on real inputs, reproduced concretely | fix the body, or narrow PRE if the input is truly invalid |
| contract_gap | refuted only through values a callee's contract allows | strengthen the callee's POST, or inline it |
| vacuous | no input satisfies PRE, the refinements and the LEARNABLE box | weaken PRE |
| unknown | timeout, solver unknown, partial instantiation or unreplayable model | split the obligation, add an INVARIANT conjunct, or guard the op |
| error | a verifier bug or solver disagreement | none: the build stops, and the case joins the known-bad suite |

## Proof records

Each result is stored as:

```json
{ "obligation": "<hash>", "id": "@soft_knee/POST.0", "node": "<node_hash>", "solver": "z3",
  "version": "<pinned>", "verdict": "unsat", "time_ms": 41,
  "query": "<hash of the SMT-LIB2 text>", "evidence": "re-solved" }
```

A cached record is reused only when its solver name and version match the pinned ones; otherwise the query is solved again.

## Cost model

COST clauses are proven in this model; BUDGET is measured (docs/16). Each resource is computed exactly, as a symbolic expression over the node's inputs and intermediate lengths, and each COST conjunct becomes an obligation `computed ≤ bound` (`@n/COST.i`).

| Resource | Computed as |
| --- | --- |
| `ops` | 1 per executed op; SIMD_MAP, FOLD_BOUNDED and GENERATE add n times their kernel's cost, ASSERT_GUARD the larger of its branches, CALL_HASH the callee's COST `ops` bound (a callee without one makes the caller's bound unprovable) |
| `arena_bytes` | the sum of the node's allocations (docs/13), each len × size(T) rounded up to 16, plus its callees' `arena_bytes` bounds; a callee without ARENA in its effects contributes 0, which is how `@limit_frame` proves `arena_bytes == 0` through its pure `@sha256` binding |
| `heap_bytes` | 0: the MVP has no heap |
| `stack_bytes` | not proven in the MVP: vg-codegen reports each node's frame size per target from the compiler, and a COST bound on it is checked against that report at build time (R-06) |

Kernels are fixed-size, so their costs are constants, and an MVP node's cost is a sum of products of lengths and constants, with max at branches. Mesh ranks candidates by these expressions (docs/09).

In the MVP, trust rests on pinned solver versions and reproducible re-solving; release builds require two solvers to agree (`evidence: "two-solvers"`). Independently checkable certificates (`evidence: "certificate"`), such as cvc5's Alethe proofs checked by Carcara, are R-05. A signature on a record attests who checked it, never that the claim is true.

## Testing the verifier

A verifier that accepts a wrong program is the worst bug this project can have, so three suites run on every change:

- **Known-bad programs**, each rejected with a reproducing counterexample; each file names the obligation that must fail.
- **Mutation testing:** mutate verified nodes (flip comparisons, change constants, swap operands, drop guards); any mutant that breaks its contract and still verifies fails CI.
- **Differential testing:** native binaries against vg-interp on generated inputs.
