# 13 · Core semantics

This chapter fixes what a node means: its values, how it evaluates, what its formulas denote, and which rewrites preserve its meaning. vg-interp implements it literally, and the canonicalizer, vg-types, vg-smt and vg-codegen are correct exactly when they agree with it. Each op's own rules are in docs/14-opcode-reference.md; the decisions behind this chapter are ADRs 0002 to 0009.

## Values

| Type | Values | Bytes (AS_BYTES, ABI, CONSTS) |
| --- | --- | --- |
| Bool | false, true | one byte, 0x00 or 0x01 |
| uN | integers 0 … 2^N − 1 | N/8 bytes, little-endian |
| iN | integers −2^(N−1) … 2^(N−1) − 1 | two's complement, little-endian |
| f16, bf16, f32, f64 | every finite value of the format, including +0 and −0; +∞ and −∞; **one NaN** | IEEE 754 interchange encoding, NaN written as the canonical NaN below |
| Array<T, N> | sequences of exactly N values of T | the N elements, consecutive |
| Tensor<T, [d1, …, dn]> | arrays indexed by (i1, …, in) with 0 ≤ ik < dk | row-major: element (i1, …, in) sits at offset ((i1·d2 + i2)·d3 + … + in)·size(T) |
| Slice<T> | sequences of 0 … 2^64 − 1 values of T | the elements, consecutive |
| Tuple<T1, …, Tn>, Result<T, E> | n-tuples; Ok(t) or Err(e) | none in the MVP (no AS_BYTES) |
| Eff | one value, the token | none |
| FileIn, FileOut, Clock | opaque capabilities, created only by vg-rt | none |

**One NaN (ADR 0002).** A float value is a number, an infinity or NaN, and NaN has neither sign nor payload, exactly as in SMT-LIB's floating-point theory. Wherever a float becomes bytes, NaN is written as the canonical quiet NaN with a zero sign bit; wherever bytes become a float, every NaN pattern reads as NaN.

| Type | f16 | bf16 | f32 | f64 |
| --- | --- | --- | --- | --- |
| Canonical NaN | 0x7E00 | 0x7FC0 | 0x7FC00000 | 0x7FF8000000000000 |

This is the NaN that WebAssembly's deterministic profile produces ("only positive canonical NaN outputs are produced"). LLVM, by contrast, gives NaN results "a non-deterministic sign" and a payload chosen from several options, and x86_64 and AArch64 produce different default NaNs, so vg-codegen canonicalizes NaN wherever a float reaches memory or leaves an exported node, unless the verifier proved the value is not NaN (docs/06). The result: one node gives the same bytes in vg-interp and on every target. Zero keeps its sign: −0.0 and +0.0 are different values with different bytes, equal under IEEE comparison.

**Fixed-size types** are Bool, the scalar types, and Array, Tensor, Tuple and Result built only from fixed-size types. Kernels and accumulators use only fixed-size types (docs/04), so they never allocate.

## Evaluation

- **A node is a function** from its inputs — its parameters, then its LEARNABLE parameters θ in declaration order — to its results. Evaluating it evaluates every op of its canonical graph once, in any order consistent with the data dependencies. The order cannot matter: pure ops have no effects, and effect ops are totally ordered by the Eff chain.
- **Strict.** Every op is evaluated, whether or not a SELECT later discards its value. **ASSERT_GUARD is the only lazy op:** it evaluates its condition, then calls exactly one branch.
- **Partial ops.** Some ops are defined on only part of their domain: checked arithmetic, DIV and MOD, checked shifts, INDEX, UPDATE, SLICE, CONCAT, a CAST that can lose information, GET_OK, GET_ERR, FCLAMP, FCOPYSIGN, and CALL_HASH outside the callee's PRE. The domain is the op's obligation (docs/14). A verified node never applies an op outside its domain, for any input that satisfies PRE and any θ in the LEARNABLE box. When an unverified node does, vg-interp stops with a *trap* that names the op and its inputs; compiled code has no defined behavior there, which is why vg-codegen accepts only verified nodes.
- **Kernels.** SIMD_MAP and GENERATE apply their kernel to each element independently; the order is unspecified because kernels are pure. FOLD_BOUNDED applies its kernel to elements 0, 1, …, n − 1 in order.
- **Effects.** An effect op's results come from the environment. The meaning of an effectful node is a function of its inputs and its *effect trace*: the results of its effect ops, in Eff-chain order. vg-interp runs effect ops against a scripted trace (docs/16), so every run, replay and counterexample is deterministic; vg-rt can record a trace, opt-in and size-capped. Successive CLOCK_READ results along one Eff chain never decrease.
- **Nothing else is observable.** Results never depend on addresses, arena placement, timing, vectorization or thread scheduling.

## Memory (MVP, ADR 0003)

Values are immutable, and where they live is vg-codegen's choice, with one rule visible in the language: an op whose result is a Slice of non-constant length needs memory at run time, and it takes that memory from the call's arena.

- Each call of an exported or root node receives one bump arena from its caller; vg-rt provides the root's. SIMD_MAP and GENERATE over runtime lengths, CONCAT of slices, an UPDATE of a slice that is not uniquely owned, and FILE_READ allocate there. Views allocate nothing: SLICE, AS_BYTES, and inputs.
- A node that contains such an op, or calls a node that does, has ARENA in its effects. ARENA needs no Eff token, because the order of allocations is invisible.
- An UPDATE of a slice updates in place, allocating nothing, only when its collection operand is the result of an allocating op and that result has exactly one use; a view (SLICE, AS_BYTES, an input) is never updated in place, because other values may share its memory. Any other UPDATE of a slice copies it. Arrays and tensors are fixed-size values and never allocate.
- **Views never outlive their memory.** A slice that reaches a node's results must view an input or arena memory, never a fixed-size value computed in the node, which lives only as long as the call (VG-T0120). The check walks back from RETURN through SLICE, AS_BYTES, SELECT, TUPLE, PROJ, OK, ERR, GET_OK and GET_ERR, and through calls, whose slice results may view their slice arguments. To return such bytes, return the fixed-size value itself.
- vg-types computes a node's exact arena use as a symbolic expression of input lengths and intermediate slice lengths; COST states a bound and vg-smt proves it (docs/05). vg-rt sizes the root's arena from that expression before the call; an exported node gets a generated size function (docs/06).
- The arena is released when the outermost exported call returns. Slices returned by an exported node stay valid until its caller resets the arena.
- `Arena<N>` values and ARENA_ALLOC — explicit, scoped regions — are [ALPHA]; R-11 needs them for long-lived data.

## Formulas

Contract formulas (docs/03) are a small logic over the same values. vg-smt encodes them exactly (docs/05); vg-interp evaluates them for the concrete pre-screen and for replay.

| Term | Meaning |
| --- | --- |
| A parameter name | that input, by position: renaming parameters never changes a hash |
| `result`, `result.j` | the node's result, or its j-th result when it has several (POST only) |
| A port `%n` | the value of that op result; allowed only in INVARIANT clauses |
| Integer terms | mathematical integers: no wrapping, ever |
| Float terms | IEEE values of their type; `+ - * /` round to nearest-even |
| `==`, `!=` on floats | IEEE comparison: `NaN == NaN` is false, `-0.0 == 0.0` is true; `<`, `<=`, `>`, `>=` are false whenever an operand is NaN |
| `==` on collections, tuples and Results | equal shapes and lengths, and `==` element by element |
| `same(a, b)` | identity at any type: both NaN, or equal with the same sign; for collections, equal lengths and `same` element by element. For byte-representable types, `same(a, b)` holds exactly when `as_bytes(a) == as_bytes(b)` |
| `xs[i]`, `t[i1, …, in]` | the element, for an index in range; otherwise an unspecified value of the element type |
| `a / 0`, `a % 0` on integers | an unspecified integer (docs/05) |
| `forall(i in a..b) { φ }` | φ holds for every integer i with a ≤ i < b; true when a ≥ b |
| `is_ok(r)`, `ok(r)`, `err(r)` | the tag of a Result; its Ok value, or its Err value (unspecified for the other tag) |
| `fold:f(init, xs, k, b…)` | the left fold of kernel @f over the first k elements of xs with broadcasts b: init when k = 0, and f(fold:f(init, xs, k − 1, b…), xs[k − 1], b…) when 0 < k ≤ len(xs); unspecified above len(xs) (ADR 0008) |
| `spec:s(args)` | the result of executable SPEC node @s on args; unspecified outside its PRE |

"Unspecified" means a value the proof may not depend on: the solver may pick any value, so no obligation is proven by relying on it. Contracts must not read one at all: each term that could adds a well-definedness obligation, proven in the context where the term occurs (ADR 0010), so a verified contract can be evaluated on every input that satisfies PRE.

## What "verified" means

A node is **verified** when its contract is non-vacuous, every one of its obligations is proven, and every node it calls is verified, or is a Mesh binding whose evidence the caller's policy accepts (docs/09). Then, for every input that satisfies PRE and every θ in the LEARNABLE box:

1. evaluation never traps;
2. the results satisfy POST, and every INVARIANT holds at every step of its fold;
3. effects stay within EFFECTS, and only through the handles that reach the node;
4. ops and arena use stay within COST, in the cost model of docs/05.

The guarantee is relative to the trusted computing base (docs/01) and covers only what the contract states. BUDGET is measured, never proven. A proof that relied on a `spec:` call is exactly as strong as that SPEC node is faithful to the intent it names, which is why R-14 exists.

**Trust boundary (ADR 0009).** Callers outside VeriGraph are not verified, so each exported or root node also gets a checked entry point: it validates every input against PRE, rejects Bool bytes other than 0 and 1 and float inputs holding a non-canonical NaN pattern, sets the default floating-point environment for the call, and only then calls the internal function, which assumes PRE (docs/06).

## Equivalence and canonical form

Canonicalization (docs/02) computes a normal form. Two VG-Sym texts of one node must canonicalize to byte-identical .vgir exactly when one can be turned into the other by these moves; G1's property tests generate equivalence classes by applying them at random (docs/16).

| Move | Example |
| --- | --- |
| Rename ports, parameters, bound variables and INVARIANT labels consistently | `%3` → `%17`; `x` → `sample` in SIGNATURE and every formula |
| Reorder statements, keeping every port defined before its use | swap two independent ops |
| Duplicate a pure op and redirect some uses to the copy | merged again by step 5 |
| Add a pure op that no output, effect or live INVARIANT uses | dropped by step 5 |
| Swap the operands of a commutative op (docs/14 marks them) | `ADD.wrap(a, b)` → `ADD.wrap(b, a)` |
| Bind a literal or a port through IDENTITY | `%0 = IDENTITY(1.0:f32)` |
| Change comments, whitespace, intent strings or labels of callees that resolve to the same hash | DEBUG only |

Everything else changes the hash, including the order of parameters, of results, of LEARNABLE entries and of conjuncts in a clause. Several clauses of one kind join into one conjunction in source order, so `PRE: a; PRE: b;` equals `PRE: a && b;` (ADR 0007).

## Foundations, and the laws they give

Each line names the mathematics a part of VeriGraph rests on and the law an implementation must pass; docs/16 turns each law into tests.

| Structure | Where | Law to test |
| --- | --- | --- |
| Term graphs up to bisimulation, whose fully collapsed form is the unique representative of its class (Ariola and Klop, 1996) | Body hashing, merging, dead-op removal | Equal body_hash exactly when the bodies unravel to the same terms, up to the commutative swaps |
| Cartesian structure of pure ops: copying and discarding are natural (Fox, 1976) | Merging equal ops, dropping dead ones | Only pure ops are ever merged or dropped |
| Premonoidal and Freyd categories, where effects run on a threaded wire (Power and Robinson, 1997; Román, 2022) | The Eff token | Every effect op lies on the one Eff chain; no move reorders effects |
| Join-semilattice of effect sets under OR, with PURE as bottom | Effect masks | Each node's mask contains the masks of everything it calls |
| Costs with + for sequence, multiplication by n for iteration and max for branches | COST | Computed cost equals the sum over the canonical graph, exactly |
| The quotient of IEEE bit patterns that identifies every NaN | Floats | Every op respects the quotient; encoding then decoding is the identity on values |
| De Bruijn indices; hashing modulo α-equivalence (Maziarz et al., PLDI 2021) | Bound variables in formulas | Renaming bound variables never changes contract_hash |
| Lists as an initial algebra; induction over a fold | FOLD_BOUNDED, INVARIANT, `fold:` | The FOLD rule is induction on k; `fold:` satisfies its two defining equations |
| Behavioral subtyping (Liskov and Wing, 1994) | CALL_HASH by contract; Mesh matching | A candidate replaces a query when PRE_q ⇒ PRE_c and PRE_q ∧ POST_c ⇒ POST_q |
| Lens laws of bidirectional transformations (Foster et al., 2007) | VG-Sym and VG-IR | `asm(disasm(b)) = b`; `disasm(asm(s))` is the canonical print of s; parse after print is the identity |
