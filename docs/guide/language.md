# The VG-Sym language

VG-Sym is the text form of VeriGraph programs. `vg asm` turns it into canonical VG-IR, the binary
graph that is the source of truth, and `vg disasm` turns any node back into VG-Sym. This chapter is
a working summary. The normative sources are docs/03-vg-sym.md (grammar and authoring rules),
tools/vgsym.lark (the executable grammar), docs/14-opcode-reference.md (every op) and
docs/13-semantics.md (what every op and built-in means).

## Modules and definitions

A file holds one module: `module name;`, then definitions. A definition is used only after it, so
write kernels first and the root last; then any prefix of the file can be checked (ADR 0005).

| Definition | What it is |
| --- | --- |
| `NODE @name [attrs] { … }` | A function with a signature, effects, a contract and a body. Attributes: `intent: "…"` (a one-line purpose), `export`, `inline`. |
| `SPEC @name { … }` | An executable reference specification. Bodies cannot call it; contracts use it as `spec:name(…)`. |
| `@MESH_QUERY @name { … }` | Binds a label to a component found on VG-Mesh. Version 0 resolves it only through a pin in `vg.lock`. |

Every node has the same four parts, in this order: `SIGNATURE`, `EFFECTS`, `CONTRACT`, `BODY`
(a `LEARNABLE` block may come between the contract and the body). This module uses a SPEC as the
reference for a node:

```vgsym
// expect: verify
module specs;

SPEC @clamp01 {
  SIGNATURE: (x: f32) -> (f32);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = FCLAMP(x, 0.0:f32, 1.0:f32);
    RETURN(%0);
  }
}

NODE @saturate [intent: "Clamp a finite sample into [0, 1]"] {
  SIGNATURE: (x: f32) -> (f32);
  EFFECTS: [];
  CONTRACT {
    PRE:  is_finite(x);
    POST: same(result, spec:clamp01(x));
  }
  BODY {
    %0 = FMAX(x, 0.0:f32);
    %1 = FMIN(%0, 1.0:f32);
    RETURN(%1);
  }
}
```

## Types

| Type | Notes |
| --- | --- |
| `u8` `u16` `u32` `u64`, `i8` `i16` `i32` `i64` | Integers. Every integer op names its overflow mode. |
| `f32` `f64` | IEEE 754 floats with one canonical NaN (docs/13). |
| `f16` `bf16` | Storage only: CAST to f32 or f64 before arithmetic (VG-T0105). |
| `Bool` | `true`, `false`. |
| `Array<T, N>`, `Tensor<T, [d1, d2]>` | Fixed-size collections, row-major. |
| `Slice<T>` | A view of a collection, with a run-time length below 2^64. |
| `Tuple<T1, …, Tn>` | Projected with `PROJ(t, j)`, or `t.j` in formulas. |
| `Result<T, E>` | Built with OK and ERR, which need a port annotation. |
| `Eff` | The effect token of effectful nodes. |
| `FileIn` `FileOut` `Socket` `Clock` `GpuQueue` | Capability handles, passed only from the root. |
| `Arena<N>` | Arena memory of N bytes. |
| `{x: T \| formula}` | A refinement, allowed only at the top of a SIGNATURE. |

## Effects

`EFFECTS: []` means pure. An effectful node lists its effects, takes `Eff` as its first parameter and
returns it as its first result (VG-E0103), and uses the token exactly once (VG-E0102). The effects are
ARENA, CLOCK, FILE_READ, FILE_WRITE, NET_READ, NET_WRITE, GPU, NONDET, PARAM_UPDATE, TELEMETRY,
HOST_CALL and SPAWN; the MVP has ops for the first four: FILE_READ, FILE_WRITE and CLOCK_READ, with
ARENA for what they allocate.

```vgsym
module effects_tour;

NODE @read_config {
  SIGNATURE: (io: Eff, f: FileIn) -> (Eff, Result<Slice<u8>, u32>);
  EFFECTS: [FILE_READ, ARENA];
  CONTRACT {}
  BODY {
    %0, %1 = FILE_READ(io, f, 4096:u64);
    RETURN(%0, %1);
  }
}
```

Keep effects in a thin root node and everything else pure: pure nodes are the ones the verifier
reasons about best, and the only ones that can be kernels.

## Bodies

A body is a list of statements, each assigning fresh ports, and one final `RETURN`:

```text
%3 = ADD.checked(acc, %1);            // one result
%0, %1 = FILE_READ(io, f, 4096:u64);  // several results
%4: Result<u8, u32> = ERR(1:u32);     // a port annotation, checked against the inferred type
RETURN(%3);
```

- **Ports** (`%0`, `%1`, …) are assigned once and used only after their statement.
- **Arguments** are ports, parameter names, typed literals or labels. There are no inline expressions:
  a condition is an op too.
- **Typed literals**: `1:u32`, `-3:i8`, `0.5:f32`, `true`, `[1, 2]:Array<u8, 2>`. A decimal rounds to
  nearest-even in its type; VG-W0003 warns when it is not exact.
- **Modes**: integer arithmetic takes `.checked`, `.wrap` or `.sat`; SHL and SHR take `.checked` or
  `.wrap`; ordered comparisons take `.s`, `.u` or `.f`, matching the operand type (VG-T0103); CAST
  names its target type (`CAST.f32(x)`). No op has a default mode.
- **Named arguments** come last: `init=`, `invariant=`, `else=`.
- **SELECT is strict**: it evaluates both inputs. To protect an op that could fail, put it in a kernel
  and call it through `ASSERT_GUARD.branch`, which runs only the branch it takes.

## Ops

| Family | Ops |
| --- | --- |
| Structure | IDENTITY, TUPLE, PROJ, SELECT, CAST, AS_BYTES, OK, ERR, IS_OK, GET_OK, GET_ERR |
| Integer arithmetic | ADD, SUB, MUL, DIV, MOD, NEG, each `.checked`, `.wrap` or `.sat` |
| Bits | AND, OR, XOR, NOT, SHL, SHR, ROTL, ROTR |
| Floating point | FADD, FSUB, FMUL, FDIV, FMA, FNEG, FABS, FMIN, FMAX, FCLAMP, FCOPYSIGN, FSQRT |
| Compare and logic | EQ, NE, LT, LE, GT, GE (`.s`, `.u`, `.f`), BAND, BOR, BNOT |
| Collections | INDEX, UPDATE, LEN, SLICE, CONCAT |
| Combinators | SIMD_MAP, FOLD_BOUNDED, GENERATE, ASSERT_GUARD and ASSERT_GUARD.branch |
| Composition | CALL_HASH |
| Effects | FILE_READ, FILE_WRITE, CLOCK_READ |

Float ops are the IEEE 754 operations rounded to nearest-even: no transcendentals, no fast-math, no
implicit fused multiply-add. PARTITION_BY, SCAN_PREFIX, MATCH_DISJOINT, REC_CALL, ARENA_ALLOC and the
network, GPU, random and host ops are not in the MVP (VG-S0104). docs/14 gives each op's typing,
meaning and obligations.

Collections and `Result` at work:

```vgsym
// expect: verify
module collections;

NODE @get {
  SIGNATURE: (xs: Array<u32, 8>, i: u64) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  i < 8;
    POST: result == xs[i];
  }
  BODY {
    %0 = INDEX(xs, i);
    RETURN(%0);
  }
}

NODE @validate {
  SIGNATURE: (b: u8) -> (Result<u8, u32>);
  EFFECTS: [];
  CONTRACT {
    POST: is_ok(result) == (b <= 100);
  }
  BODY {
    %0 = LE.u(b, 100:u8);
    %1: Result<u8, u32> = OK(b);
    %2: Result<u8, u32> = ERR(1:u32);
    %3 = SELECT(%0, %1, %2);
    RETURN(%3);
  }
}
```

## Combinators and calls

A **kernel**, the first argument of SIMD_MAP, FOLD_BOUNDED or GENERATE, is the label of a pure NODE
with fixed-size types and one result. It captures nothing: extra values are passed as broadcast
arguments after the collection.

| Form | Result | Obligations |
| --- | --- | --- |
| `SIMD_MAP(@f, xs, b…)` | f applied to every element | the kernel's PRE for a fresh element |
| `GENERATE(@f, n, b…)` | `[f(0), …, f(n − 1)]` | the kernel's PRE for a fresh index |
| `FOLD_BOUNDED(@f, xs, b…, init=a, invariant=@I)` | the fold of f over xs from a | the invariant holds at 0, the kernel's PRE holds under it, and each step keeps it |
| `ASSERT_GUARD.branch(c, @f, a…, else=@g)` | f(a…) when c, else g(a…) | each branch's PRE under its condition |
| `CALL_HASH(@f, a…)` | f(a…) | f's PRE; the caller sees only f's contract |

```vgsym
// expect: verify
module combinators;

NODE @scale {
  SIGNATURE: (x: f32, g: f32) -> (f32);
  EFFECTS: [];
  CONTRACT {
    PRE:  abs(x) <= 1.0 && abs(g) <= 1.0;
    POST: abs(result) <= 1.0;
  }
  BODY {
    %0 = FMUL(x, g);
    RETURN(%0);
  }
}

NODE @scale_all {
  SIGNATURE: (xs: Array<f32, 4>, g: f32) -> (Array<f32, 4>);
  EFFECTS: [];
  CONTRACT {
    PRE:  forall(i in 0..4) { abs(xs[i]) <= 1.0 } && abs(g) <= 1.0;
    POST: forall(i in 0..4) { abs(result[i]) <= 1.0 };
  }
  BODY {
    %0 = SIMD_MAP(@scale, xs, g);
    RETURN(%0);
  }
}

NODE @square {
  SIGNATURE: (i: u64) -> (u64);
  EFFECTS: [];
  CONTRACT {
    PRE:  i < 16;
    POST: result == i * i;
  }
  BODY {
    %0 = MUL.checked(i, i);
    RETURN(%0);
  }
}

NODE @squares {
  SIGNATURE: () -> (Array<u64, 16>);
  EFFECTS: [];
  CONTRACT {
    POST: forall(i in 0..16) { result[i] == i * i };
  }
  BODY {
    %0 = GENERATE(@square, 16:u64);
    RETURN(%0);
  }
}
```

A fold names an INVARIANT clause of its node. Its two names are the step count k and the
accumulator; the invariant must hold before the first step and after every step, and the verifier
then knows it at k = len(xs):

```vgsym
// expect: verify
module folds;

NODE @add_step {
  SIGNATURE: (acc: u32, x: u32) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  acc <= 3000 && x <= 1000;
    POST: result == acc + x;
  }
  BODY {
    %0 = ADD.checked(acc, x);
    RETURN(%0);
  }
}

NODE @sum4 [intent: "Sum four readings of at most 1000 each"] {
  SIGNATURE: (xs: Array<u32, 4>) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  forall(i in 0..4) { xs[i] <= 1000 };
    POST: result <= 4000;
    INVARIANT @bounded(k, acc): acc <= 1000 * k;
  }
  BODY {
    %0 = FOLD_BOUNDED(@add_step, xs, init=0:u32, invariant=@bounded);
    RETURN(%0);
  }
}
```

`ASSERT_GUARD.branch` protects an op that would fail on some inputs. Here INDEX would fail on an empty
slice, so it lives in `@head`, which runs only when the slice is not empty:

```vgsym
// expect: verify
module guards;

NODE @head {
  SIGNATURE: (xs: Slice<u32>) -> (u32);
  EFFECTS: [];
  CONTRACT {
    PRE:  len(xs) > 0;
  }
  BODY {
    %0 = INDEX(xs, 0:u64);
    RETURN(%0);
  }
}

NODE @zero {
  SIGNATURE: (xs: Slice<u32>) -> (u32);
  EFFECTS: [];
  CONTRACT {
    POST: result == 0;
  }
  BODY {
    RETURN(0:u32);
  }
}

NODE @head_or_zero [intent: "The first element, or 0 for an empty slice"] {
  SIGNATURE: (xs: Slice<u32>) -> (u32);
  EFFECTS: [];
  CONTRACT {
    POST: len(xs) == 0 ==> result == 0;
  }
  BODY {
    %0 = LEN(xs);
    %1 = GT.u(%0, 0:u64);
    %2 = ASSERT_GUARD.branch(%1, @head, xs, else=@zero);
    RETURN(%2);
  }
}
```

Without `POST: result == 0` on `@zero`, the last POST would come back as a contract gap: a caller
knows a callee only through its contract ([Verification](verification.md#contract-gaps)).

## Contracts

| Clause | Meaning |
| --- | --- |
| `PRE: f;` | Assumed on entry; callers must prove it. |
| `POST: f;` | Proven on exit; may use `result` (or `result.j` with several results). |
| `INVARIANT @name(k, acc): f;` | A fold invariant; may also name ports of the body. |
| `DECREASES: f;` | For REC_CALL, which is not in the MVP. |
| `COST: f;` | Provable resources: `heap_bytes`, `arena_bytes`, `stack_bytes`, `ops`. Version 0 does not prove COST yet. |
| `BUDGET: f;` | Measured targets such as `latency_p99_us`, never claimed as proven. |

Several clauses of one kind form one conjunction in source order, and its top-level `&&` conjuncts are
numbered from 0: `POST.0` is the first conjunct. That is how obligation IDs name them.

**Formulas** use `==>` (implies), `||`, `&&`, `== != < <= > >=`, `+ - * / %`, `!` and unary `-`,
indexing `xs[i]`, tuple members `t.j`, bounded quantifiers `forall(i in lo..hi) { … }`, the built-ins
`len`, `is_finite`, `is_nan`, `abs`, `min`, `max`, `as_bytes`, `same`, `is_ok`, `ok` and `err`, and
the calls `spec:s(…)` and `fold:f(init, xs, k, b…)` (what a fold has computed after k steps).

- **Integer terms are mathematical integers**: `acc + 1` never wraps, and `/` truncates toward zero.
- **Float terms follow IEEE**: `==` is IEEE equality (NaN equals nothing, −0.0 equals 0.0); use
  `same(a, b)` for identity. An untyped decimal takes the float type of the term it meets: with
  `knee: f32`, `knee <= 0.99` compares against `0.99:f32`.
- **Well-definedness**: a term that may be undefined (a division, an index, `ok`, `err`, a `spec:`
  call) must be guarded, as in `d != 0 ==> result == x / d`; the verifier checks this with `.wd`
  obligations.

## Lexical rules and pitfalls

- `//` starts a comment that runs to the end of the line. Strings are double-quoted, with `\"` and
  `\\` escapes.
- Names and labels start with a lowercase letter or `_`; op names are uppercase. Every word the
  grammar spells out (`result`, `in`, `f32`, `NODE`, `FILE_READ`, …) is reserved (VG-P0004).
- A port is one token, so `x%2` is `x` followed by the port `%2`: write `x % 2`.
- `t.0.1` lexes as `t`, `.`, `0.1`: write `(t.0).1`.
- Numbers carry no sign: `x-1` is `x`, `-`, `1`. In a formula, a `-` before a typed literal belongs
  to it, so `-128:i8` is the i8 minimum.
