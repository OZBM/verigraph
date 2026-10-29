# 14 · Opcode reference

One normative entry per MVP op: its number, VG-Sym form, typing rule, meaning, obligation and commutativity. The validator (P1.3), vg-types (P2.1), vg-interp (P2.4), vg-smt (P3.1), vg-codegen (P4.2, P4.3) and the canonicalizer's step 4 all read this table; when it disagrees with another chapter, fix one of them through an ADR. Opcode numbers inside each range are this chapter's proposal; ADR 0001 confirms them with golden files in P1.6.

## Conventions

- **Type letters.** `I` is any integer type (u8 … u64, i8 … i64) and N its width. `F` is f32 or f64: f16 and bf16 are storage-only in the MVP, so they enter and leave arithmetic only through CAST. `S` is a scalar type or Bool, `T` any value type, `MIN` and `MAX` the bounds of I.
- **Operands.** Each VG-Sym argument becomes one operand, in argument order; named arguments come last, in the order listed here. A literal argument becomes a constant operand. The OPS `attr` field is 0 for every MVP op.
- **Typing is local.** An op's result types follow from its opcode, mode and operand types alone; nothing is inferred from uses. The two exceptions take their type from the port annotation (`%r: Result<T, E> = OK(v);`): OK and ERR.
- **Modes.** Where a row lists modes, exactly one is required; the mode byte encodes it. No op has a default mode.
- **Obligations** are named `<OP>.<kind>`; the full obligation ID adds the node and port (docs/15). An op with no obligation is total.
- **C** marks a commutative op: canonicalization step 4 sorts its operands by hash. FMA is commutative in its first two operands only.
- **Cost** is 1 unit of `ops` per op unless a row says otherwise (docs/05).

| Modes | Mode byte |
| --- | --- |
| checked, wrap, sat (integer arithmetic) | 0, 1, 2 |
| checked, wrap (SHL, SHR) | 0, 1 |
| s, u, f (LT, LE, GT, GE) | 1, 2, 3 |
| ASSERT_GUARD (guard), ASSERT_GUARD.branch | 0, 1 |
| none (every other op, CAST included: its target is its result type) | 0 |

## Structure · 0x0001–0x000F

| Opcode | Form | Typing | Meaning | Obligation | C |
| --- | --- | --- | --- | --- | --- |
| 0x0001 | CONST | — | Reserved. Constants are operands (src_kind 2); the validator rejects this opcode | — | |
| 0x0002 | `IDENTITY(v)` | T → T | v. Text only: step 2 rewires its uses to v, and .vgir never contains it | — | |
| 0x0003 | `TUPLE(v1, …, vn)` | n ≥ 1 → Tuple<T1, …, Tn> | the tuple | — | |
| 0x0004 | `PROJ(t, j)` | Tuple<T0, …, Tn−1>, j a u32 literal below n → Tj | member j, from 0 | — | |
| 0x0005 | `SELECT(c, a, b)` | Bool, T, T → T | c ? a : b; both a and b are evaluated | — | |
| 0x0006 | `CAST.<S2>(v)` | S1 → S2 | see the CAST table | CAST.in_range when lossy | |
| 0x0007 | `AS_BYTES(v)` | a scalar or Bool, or an Array, Tensor or Slice of them → Slice<u8> | the bytes of docs/13: little-endian, row-major, canonical NaN; a view, never a copy | — | |
| 0x0008 | `%r: Result<T, E> = OK(v)` | T → Result<T, E> | Ok(v) | — | |
| 0x0009 | `%r: Result<T, E> = ERR(e)` | E → Result<T, E> | Err(e) | — | |
| 0x000A | `IS_OK(r)` | Result<T, E> → Bool | true for Ok | — | |
| 0x000B | `GET_OK(r)` | Result<T, E> → T | the Ok value | GET_OK.is_ok | |
| 0x000C | `GET_ERR(r)` | Result<T, E> → E | the Err value | GET_ERR.is_err | |

**CAST.** The mode names the target scalar type, and the result type is that type.

| From → to | Meaning | Obligation |
| --- | --- | --- |
| integer → integer that holds every source value (u8 → u16, u8 → i16, i8 → i64) | the same integer | — |
| integer → integer that does not (narrowing, or a sign change such as i32 → u32) | the same integer | CAST.in_range: the value fits |
| Bool → integer | false → 0, true → 1 | — |
| integer → float | nearest-even rounding; overflows to ±∞ as IEEE 754 specifies (only possible into f16) | — |
| float → integer | truncation toward zero | CAST.in_range: not NaN, finite, and the truncated value fits |
| float → float | exact when widening; nearest-even when narrowing, overflowing to ±∞; NaN stays NaN | — |
| to the same type; integer → Bool; float ↔ Bool | rejected (VG-T0113): compare with NE instead | — |

Wrapping a value into a narrower type takes two explicit steps, `AND` with a mask and then CAST, so a lossy conversion is never silent.

## Integer arithmetic · 0x0010–0x001F

Operands and result share one integer type I. "Wrapped" is the unique value of I congruent to the exact result modulo 2^N; "clamped" is the exact result limited to [MIN, MAX].

| Opcode | Form | checked | wrap | sat | C |
| --- | --- | --- | --- | --- | --- |
| 0x0010 | `ADD.<mode>(a, b)` | a + b; ADD.no_overflow | wrapped | clamped | C |
| 0x0011 | `SUB.<mode>(a, b)` | a − b; SUB.no_overflow | wrapped | clamped | |
| 0x0012 | `MUL.<mode>(a, b)` | a · b; MUL.no_overflow | wrapped | clamped | C |
| 0x0013 | `DIV.<mode>(a, b)` | a / b truncated toward zero; DIV.nonzero, and for signed I, DIV.no_overflow (not MIN / −1) | MIN / −1 = MIN; DIV.nonzero | MIN / −1 = MAX; DIV.nonzero | |
| 0x0014 | `MOD.<mode>(a, b)` | a − b · (a / b), with the sign of a; MOD.nonzero | same; MIN % −1 = 0 | same | |
| 0x0015 | `NEG.<mode>(a)` | −a; NEG.no_overflow (signed: a ≠ MIN; unsigned: a = 0) | wrapped | clamped | |

MOD cannot overflow, so its three modes mean the same; the mode is still written, so every integer op reads alike.

## Bits · 0x0030–0x003F

Operands share one integer type I; shift and rotate amounts are u32. Bitwise ops act on the N-bit two's complement pattern. Bool uses BAND, BOR and BNOT instead.

| Opcode | Form | Meaning | Obligation | C |
| --- | --- | --- | --- | --- |
| 0x0030 | `AND(a, b)` | bitwise and | — | C |
| 0x0031 | `OR(a, b)` | bitwise or | — | C |
| 0x0032 | `XOR(a, b)` | bitwise exclusive or | — | C |
| 0x0033 | `NOT(a)` | bitwise complement | — | |
| 0x0034 | `SHL.<checked\|wrap>(a, s)` | bits shifted left, zero-filled, truncated to N bits; wrap shifts by s mod N | checked: SHL.amount (s < N) | |
| 0x0035 | `SHR.<checked\|wrap>(a, s)` | logical for unsigned I, arithmetic for signed I; wrap shifts by s mod N | checked: SHR.amount (s < N) | |
| 0x0036 | `ROTL(a, s)` | rotate left by s mod N | — | |
| 0x0037 | `ROTR(a, s)` | rotate right by s mod N | — | |

SHL never checks the value for overflow; to multiply with an overflow check, use MUL.checked.

## Floating point · 0x0040–0x004F

Operands and result share one type F. Every op is the IEEE 754-2019 operation rounded to nearest-even, with the one NaN of docs/13. There are no other float ops in the MVP: no transcendentals (R-10), no fast-math, no implicit contraction of a multiply and an add.

| Opcode | Form | Meaning | Obligation | C |
| --- | --- | --- | --- | --- |
| 0x0040 | `FADD(a, b)` | a + b | — | C |
| 0x0041 | `FSUB(a, b)` | a − b | — | |
| 0x0042 | `FMUL(a, b)` | a · b | — | C |
| 0x0043 | `FDIV(a, b)` | a / b; ±∞ or NaN when b is zero, as IEEE specifies | — | |
| 0x0044 | `FMA(a, b, c)` | a · b + c with a single rounding | — | C in a, b |
| 0x0045 | `FNEG(a)` | sign flipped; NaN stays NaN | — | |
| 0x0046 | `FABS(a)` | magnitude; NaN stays NaN | — | |
| 0x0047 | `FMIN(a, b)` | IEEE `minimum`: NaN if either operand is NaN; −0 is below +0 | — | C |
| 0x0048 | `FMAX(a, b)` | IEEE `maximum`: NaN if either operand is NaN; +0 is above −0 | — | C |
| 0x0049 | `FCLAMP(x, lo, hi)` | FMIN(FMAX(x, lo), hi): NaN when x is NaN | FCLAMP.bounds: lo and hi not NaN, lo ≤ hi | |
| 0x004A | `FCOPYSIGN(x, y)` | the magnitude of x with the sign of y; NaN when x is NaN | FCOPYSIGN.sign: y is not NaN, because NaN has no sign (docs/13) | |
| 0x004B | `FSQRT(a)` | correctly rounded square root; FSQRT(−0) = −0; NaN below −0 | — | |

## Compare and logic · 0x0050–0x005F

Every result is Bool. Operands share one type: an integer type, F, or Bool for EQ and NE.

| Opcode | Form | Meaning | C |
| --- | --- | --- | --- |
| 0x0050 | `EQ(a, b)` | equality; for floats IEEE (`NaN` equals nothing, `−0` equals `+0`) | C |
| 0x0051 | `NE(a, b)` | not EQ, so true when either float is NaN | C |
| 0x0052 | `LT.<s\|u\|f>(a, b)` | a < b; the mode must match the type (s for signed, u for unsigned, f for F); false when a float is NaN | |
| 0x0053 | `LE.<s\|u\|f>(a, b)` | a ≤ b | |
| 0x0054 | `GT.<s\|u\|f>(a, b)` | a > b | |
| 0x0055 | `GE.<s\|u\|f>(a, b)` | a ≥ b | |
| 0x0056 | `BAND(a, b)` | Bool and (both operands evaluated) | C |
| 0x0057 | `BOR(a, b)` | Bool or | C |
| 0x0058 | `BNOT(a)` | Bool not | |

The s, u or f mode repeats what the operand type already says. The validator checks that the two agree (VG-T0103), so a model that confuses signedness is told at once.

## Collections · 0x0060–0x006F

`C<T>` is Array<T, N> or Slice<T>; indices and lengths are u64. Tensors reach only SIMD_MAP, AS_BYTES, SELECT, TUPLE, PROJ, CALL_HASH and formulas in the MVP.

| Opcode | Form | Typing | Meaning | Obligation |
| --- | --- | --- | --- | --- |
| 0x0060 | `INDEX(xs, i)` | C<T>, u64 → T | element i | INDEX.in_bounds: i < len(xs) |
| 0x0061 | `UPDATE(xs, i, v)` | C<T>, u64, T → C<T> | xs with element i replaced; a slice is updated in place only when it is an allocating op's result with one use, and copied into the arena otherwise (docs/13) | UPDATE.in_bounds |
| 0x0062 | `LEN(xs)` | C<T> → u64 | the length | — |
| 0x0063 | `SLICE(xs, lo, hi)` | C<T>, u64, u64 → Slice<T> | a view of elements lo … hi − 1 | SLICE.in_bounds: lo ≤ hi ≤ len(xs) |
| 0x0064 | `CONCAT(a, b)` | C<T>, C<T> → Array<T, N + M> when both are arrays, else Slice<T> | a followed by b; a slice result is allocated in the arena | CONCAT.length: len(a) + len(b) < 2^64, for slices |

## Combinators · 0x0100–0x0107

A **kernel** (the first operand of SIMD_MAP, FOLD_BOUNDED or GENERATE) is the label of a NODE with an empty effect set, fixed-size parameter and result types, and exactly one result. ASSERT_GUARD's branches are ordinary callees: they may have effects, allocate and return several results. Kernels and callees reach the op as operands of src_kind 3. Obligations follow the proof rules of docs/05, where "fresh" means an arbitrary element or index.

| Opcode | Form | Typing | Result | Obligations | Cost |
| --- | --- | --- | --- | --- | --- |
| 0x0100 | `SIMD_MAP(@f, xs, b…)` | xs: Array<T, N>, Tensor<T, D> or Slice<T>; f: (T, B…) → (U) | the same shape over U; a slice result is allocated | MAP.pre.i: PRE_f for a fresh element | 1 + n · cost(f) |
| 0x0101 | `FOLD_BOUNDED(@f, xs, b…, init=a, invariant=@I)` | f: (A, T, B…) → (A); xs: Array<T, N> or Slice<T> | A | FOLD.base, FOLD.pre.i, FOLD.step | 1 + n · cost(f) |
| 0x0105 | `ASSERT_GUARD(c, @f, a…, else=@g)` and `ASSERT_GUARD.branch(…)` | c: Bool; f and g share one signature (A…) → (R…) | R…, from the branch taken | GUARD.then.pre.i under c; GUARD.else.pre.i under ¬c | 1 + max(cost(f), cost(g)) |
| 0x0107 | `GENERATE(@f, n, b…)` | n: u64; f: (u64, B…) → (U) | Array<U, n> when n is a literal, else Slice<U>, allocated | GEN.pre.i: PRE_f for a fresh index below n | 1 + n · cost(f) |
| 0x0102, 0x0103, 0x0104, 0x0106 | PARTITION_BY, SCAN_PREFIX, MATCH_DISJOINT, REC_CALL | [ALPHA] | — | rejected by the MVP validator (VG-S0104) | — |

FOLD_BOUNDED's invariant names an INVARIANT clause of the same node; its two bound names are the step count k and the accumulator. In a FOLD.step query, k is fresh with k < len(xs).

## Memory and composition · 0x0200, 0x0300

| Opcode | Form | Typing | Meaning | Obligations | Cost |
| --- | --- | --- | --- | --- | --- |
| 0x0200 | ARENA_ALLOC | — | [ALPHA] (ADR 0003); rejected by the MVP validator | — | — |
| 0x0300 | `CALL_HASH(@f, a…)` | the arguments match f's parameters → f's results | f applied to a; the callee's body never enters the caller's proof | CALL.pre.i | 1 + cost(f) from f's COST |

`@f` names a NODE or a `@MESH_QUERY` binding. A SPEC node is called only from formulas, as `spec:f(…)` (VG-S0110), which keeps reference specifications apart from implementations.

## Effects · 0x0400–0x04FF

An effect op takes the Eff token first and returns the next token first. Its value results come from the environment: in SMT they are fresh variables, constrained only by the facts listed here.

| Opcode | Form | Typing | Effects | Meaning |
| --- | --- | --- | --- | --- |
| 0x0400 | `FILE_READ(e, h, max)` | Eff, FileIn, u64 → Eff, Result<Slice<u8>, u32> | FILE_READ, ARENA | The whole file when it has at most max bytes; Err(1) on an I/O error, Err(2) when it is larger. Arena use at most max bytes |
| 0x0401 | `FILE_WRITE(e, h, data)` | Eff, FileOut, Slice<u8> → Eff, Result<u64, u32> | FILE_WRITE | Appends every byte and returns Ok(len(data)); Err(1) on an I/O error |
| 0x0402 | `CLOCK_READ(e, c)` | Eff, Clock → Eff, u64 | CLOCK | Monotonic nanoseconds from an arbitrary origin; never decreases along one Eff chain |
| 0x0403 … 0x040A | NET_READ, NET_WRITE, GPU_DISPATCH, RANDOM, PARAM_UPDATE, EMIT, HOST_CALL, SPAWN | — | — | not in the MVP; rejected (VG-S0104) |

## Formula built-ins

These are functions of the contract logic, not ops; docs/13 gives their meaning.

| Built-in | Arguments → result |
| --- | --- |
| `len(xs)` | Array, Slice or Tensor → integer (a tensor's element count) |
| `is_finite(x)`, `is_nan(x)` | float → Bool |
| `abs(x)`, `min(a, b)`, `max(a, b)` | integers (mathematical), or floats (FABS, FMIN and FMAX semantics) |
| `as_bytes(v)` | as AS_BYTES → Slice<u8> |
| `same(a, b)` | any two values of one type → Bool |
| `is_ok(r)`, `ok(r)`, `err(r)` | Result → Bool, T, E |
| `fold:f(init, xs, k, b…)` | f a kernel of FOLD shape; k an integer term → the accumulator type |
| `spec:s(args)` | s a SPEC node → its result |

## SMT encoding, by family

docs/05 fixes formula widths and the obligation pipeline; this is the op side.

- **Sorts.** Bool is `Bool`; uN and iN values are `(_ BitVec N)`; f16, bf16, f32 and f64 are `(_ FloatingPoint 5 11)`, `(_ FloatingPoint 8 8)`, `(_ FloatingPoint 8 24)` and `(_ FloatingPoint 11 53)`. An array is `(Array (_ BitVec 64) T)` with its static length; a slice is a `(_ BitVec 64)` length and an array; a tensor is one array over the row-major flat index. Tuples and Results are flattened into one term per member, plus a Bool tag for a Result, because Bitwuzla has no datatypes.
- **Integers.** `bvadd`, `bvsub`, `bvmul`, `bvsdiv`/`bvudiv`, `bvsrem`/`bvurem`, `bvneg`. Checked obligations compute the exact result at N + 1 bits (add, subtract, negate) or 2N bits (multiply) and compare it with the bounds; sat is an `ite` on the same comparison. Shifts zero-extend or truncate the u32 amount to N bits; wrap reduces it with `bvurem` by N first.
- **Floats.** `fp.add`, `fp.sub`, `fp.mul`, `fp.div`, `fp.fma`, `fp.sqrt` with `RNE`, plus `fp.neg` and `fp.abs`. Never use `fp.min` or `fp.max`: SMT-LIB leaves their result on (−0, +0) unspecified and they return the non-NaN operand. Encode FMIN as `ite(isNaN(a) ∨ isNaN(b), NaN, ite(a < b, a, ite(b < a, b, ite(isNegative(a), a, b))))`, FMAX symmetrically, FCOPYSIGN as `ite(isNegative(y), −|x|, |x|)`.
- **CAST.** `sign_extend`, `zero_extend` and `extract`; `(_ to_fp e s)` and `(_ to_fp_unsigned e s)` with `RNE`; `(_ fp.to_sbv N)` and `(_ fp.to_ubv N)` with `RTZ`, whose argument the obligation keeps in range.
- **AS_BYTES of floats.** SMT-LIB has no float-to-bits function, so each element gets a fresh bitvector b with `ite(isNaN(x), b = canonical NaN, to_fp(b) = x)`, which pins b uniquely because every non-NaN float has exactly one encoding.
- **Collections.** `select` and `store`; LEN is the length term.
- **Combinators** follow the proof rules of docs/05 and are never unrolled.

## LLVM lowering, by family

docs/06 is normative for codegen; these are the per-op choices.

- **Integers.** `add`, `sub` and `mul` carry `nsw` (signed) or `nuw` (unsigned) in checked mode, where the obligation proves them, and no flags in wrap mode. sat uses `llvm.sadd.sat`, `llvm.uadd.sat`, `llvm.ssub.sat`, `llvm.usub.sat`, and `llvm.smul.fix.sat` or `llvm.umul.fix.sat` with scale 0. `sdiv`, `udiv`, `srem` and `urem` are emitted as is when proven safe; signed DIV in wrap or sat mode and signed MOD in every mode add a `select` for MIN / −1, whose LLVM result is undefined.
- **Shifts.** Wrap masks the amount with N − 1 before `shl`, `lshr` or `ashr`; checked emits the shift alone. ROTL and ROTR are `llvm.fshl` and `llvm.fshr` with both value operands equal.
- **Floats.** `fadd`, `fsub`, `fmul`, `fdiv` with no fast-math flags; FMA is `llvm.fma`, never `llvm.fmuladd`; FMIN and FMAX are `llvm.minimum` and `llvm.maximum`; FCOPYSIGN is `llvm.copysign`; FSQRT is `llvm.sqrt`. NaN canonicalization at stores and exports is docs/06's.
- **CAST.** `sext`, `zext`, `trunc`, `sitofp`, `uitofp`, `fpext`, `fptrunc`; `fptosi` and `fptoui` where the obligation proves the value in range.
- **Compare.** `icmp` with the signed or unsigned predicate; `fcmp oeq` for EQ, `une` for NE, and `olt`, `ole`, `ogt`, `oge`.
- **Collections.** `getelementptr inbounds` where the index is proven in range; LEN reads the length field of the slice.
