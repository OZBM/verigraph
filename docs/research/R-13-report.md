# R-13 report · Python lifting (typed, pure, int-only subset)

Track prompt: [R-13-dynamic-language-lifting.md](R-13-dynamic-language-lifting.md). Protocol:
[PROTOCOL.md](PROTOCOL.md). Scope set by the owner on 2026-11-25: Python only (no JavaScript),
a typed int-only subset, translated to C and lifted through OmniLift's C path, then merged
into main. The JavaScript half of the track and the NumPy spec library were not attempted.

## Question and hypotheses

Question: can pure Python integer code be lifted into VG-IR with a sound contract?

Written before the experiment ran (after the front end was prototyped):

- **H1 (soundness).** Translating a typed subset of Python to C whose undefined behavior sits
  exactly where CPython raises or leaves int64, compiled at -O0, gives lifts that never
  return a value CPython would not: zero WRONG cases over all inputs.
- **H2 (coverage).** At least 30 functions from the CPython standard library and one more
  permissively licensed library translate and lift.
- **H3 (precision).** Err appears where CPython returns an int64 value (err_partial) in under
  1% of cases on inputs drawn from small and edge-value ranges.

Metrics per function, over 1e5 seeded inputs (fewer for loops driven by an int argument, see
Results): agree_value, agree_err, err_overflow, err_partial, WRONG (definitions in
`research/r-13/compare.py`). Track criteria: 30 or more functions with 100% differential
agreement (read here as zero WRONG) and at least half with a non-trivial POST.

## Survey

Listed starting points:

- [CrossHair](https://github.com/pschanely/crosshair): symbolic execution of Python with Z3
  proxy objects; finds counterexamples to PEP 316 contracts and asserts, but its absence of a
  counterexample is not a proof. Not used: OmniLift's evidence is Alive2 plus fuzzing.
- Concolic testing (DART, Godefroid et al., PLDI 2005; CUTE, Sen et al., FSE 2005): the
  model for mixing concrete and symbolic runs; not built here.
- Type inference from traces: [MonkeyType](https://pypi.org/project/MonkeyType/) records
  call types through `sys.setprofile`; [RightTyper](https://arxiv.org/abs/2507.16051)
  (2025) samples runtime types with far lower overhead. Not built: the subset takes
  annotations and defaults unannotated parameters to int.
- LLM-assisted transpilation with verification:
  [LLMLift / Verified Code Transpilation with LLMs](https://arxiv.org/abs/2406.03003)
  (NeurIPS 2024) proposes with an LLM and checks with a verifier;
  [PyVeritas](https://arxiv.org/abs/2508.08171) (2025) transpiles Python to C with an LLM and
  model-checks the C with CBMC.

Newer or closer sources:

- [PyVeritas](https://arxiv.org/abs/2508.08171) is the nearest design: Python to C, then a C
  verifier. Its translation is an LLM's, so the C can mean something else than the Python;
  here the translation is a fixed, rule-based one whose every rule is tested against CPython.
- [Codon](https://docs.exaloop.io/developers/compilation/) compiles typed Python through
  LLVM and maps `int` to `i64` with silent wraparound, which its docs call a deviation from
  Python. This track keeps int64 but turns leaving it into Err, so no wrapped value escapes.
- [Type tracing with Pynguin](https://arxiv.org/abs/2507.01477) (2025) infers parameter and
  return types during test generation; the obvious next step for unannotated code.
- [Alive2](https://github.com/AliveToolkit/alive2) (PLDI 2021) checks the lift against the
  translated C, as for every OmniLift language.
- [mypyc](https://mypyc.readthedocs.io/) compiles annotated Python to C extensions with
  tagged ints that fall back to big ints; it keeps CPython's semantics by keeping its
  runtime, which a VG-IR lift cannot.

## What was built, and how to run it

- **Front end**: `crates/vg-omnilift/src/pylift.py`, about 1,050 lines over Python's own `ast`.
  It checks the subset (types, definite assignment, stable module globals, no recursion) and
  writes UB-free C. Each operation (`py_add`, `py_mul` from 32-bit limbs, `py_floordiv` and
  `py_mod` on magnitudes with a divisor `umax(|b|, 1)`, shifts masked to 63, `py_pow` by
  squaring, bounds-checked indexing) ORs an error bit where CPython raises or leaves int64.
  The entry function ends in `py_fail(e, v) = v + 1 / (1 - e) - 1`, the only UB.
- **Ingest**: `vg lift FILE.py FUNCTION` (or `--lang python`); ADR 0012 gives the pipeline.
- **Verification**: OmniLift's own, unchanged in kind: Alive2 at unroll 8 between the lift
  and the C, then 1e6 native differential fuzz cases. One Python-specific step: the Alive2
  source is optimized with `py_fail` hidden as a declaration, then re-inlined with passes that
  never assume UB away (`always-inline,instsimplify,simplifycfg`); without that, instcombine
  assumes the error bit is 0, the source becomes more defined than CPython, and Alive2
  rejects correct lifts.
- **Experiment**: `cargo test --release -p vg-omnilift --test r13 -- --ignored --nocapture`
  lifts every function of `research/r-13/corpus.tsv`, runs each lift in vg-interp on 1e5
  inputs (seed 13: small, medium, edge and full-width ints; bytes up to 12; ints in
  [-64, 2048] when an int drives a loop), and `research/r-13/compare.py` classifies each case
  against CPython 3.13.2. `research/r-13/native_check.py` checks the C translation itself
  against CPython, without the lifter (1e5 cases per function, seed 13).
- **Raw results**: `research/r-13/results/lift.json` (per-function lift outcome),
  `summary.json` (per-function counts and first examples), `native.txt`. The 620 MB case
  file is regenerated in `target/r13/` and not committed.

## Results

Population (scan of every top-level function, 2026-11-25):

| Source | Functions | Accepted by the front end |
| --- | ---: | ---: |
| CPython 3.13.2 stdlib (tests and idlelib excluded) | 2969 | 40 |
| idna 3.18 | 32 | 1 |
| python-dateutil 2.9.0.post0 | 30 | 1 |
| packaging 25.0 | 82 | 0 |
| Total | 3113 | 42 (1.3%) |

The most frequent first rejection in the stdlib: a default or global that is not an int
constant (380, mostly `None` defaults), a parameter type outside the subset (154), keyword
arguments (139), comprehensions (137), nested functions and lambdas (108).

Lift (each with Alive2 at unroll 8 and 1e6 fuzz cases): **32 of 42**. The 10 failures, all
rejections, none a wrong lift:

| Function | Failure |
| --- | --- |
| calendar.leapdays, _pydatetime._days_before_year, _pydecimal._rshift_nearest, idna._encode_range | Alive2 timeout (10 s per query; 64-bit division and multiplication) |
| ipaddress._count_righthand_zero_bits, enum._high_bit | Alive2 value mismatch on the 64-round `bit_length` loop; the lift itself matches CPython in vg-interp (checked at 0, 3, 4, 255, 2^61+2^60, 2^62), so this is the verifier capping 64-round loops at its unroll bound |
| _pydatetime._days_in_month, _pydatetime._divide_and_round | lifter result plumbing on merged paths (VG-T0108), caught by the lifter's own type check |
| _pydecimal._sqrt_nearest, punycode.adapt | `while` loop without a computable trip count |

Differential agreement over the 32 lifts, 1e5 inputs each (3.2e6 cases):

| Class | Cases | Meaning |
| --- | ---: | --- |
| agree_value | 2,229,878 | lift Ok(v), CPython returned v |
| agree_err | 390,258 | lift Err, CPython raised |
| err_overflow | 49,162 | lift Err, CPython returned a value outside int64 |
| err_non_int | 411,941 | lift Err, CPython returned None (no-op functions, `_validate_month` falling off its end) |
| err_partial | 53,318 | lift Err, CPython returned an int64 value: an intermediate left int64 (T, `_is_single_bit` at INT64_MIN, `_div_nearest`, `within_delta`) |
| err_unchecked | 65,443 | lift Err on an argument above 2^20 in a function that shifts (RGB, `_eq_mtime` without source); CPython not run, it would build an int of up to 2^63 bits |
| **WRONG** | **0** | lift Ok with another value, or Ok where CPython raises, or a trap |

Per function: 25 of the 32 agree exactly (every case agree_value, agree_err or err_non_int);
4 have err_partial cases (T, `_is_single_bit`, `_div_nearest`, `within_delta`), 2 have
err_unchecked cases (RGB, `_eq_mtime`), and ARRAY and `_div_nearest` have err_overflow cases.
The native check of the translation agrees with CPython on all 42 accepted functions,
1e5 cases each, with no WRONG case.

Of the 32 lifted functions, 13 are trivial (identity, no-op or always-raise); 19 compute.
No lift carries a POST: OmniLift's lifts have empty contracts (contract inference is R-04).

H1 holds (0 WRONG). H2 holds narrowly (32 lifted, 19 of them non-trivial). H3 fails:
err_partial is 1.67% of cases overall, concentrated in four functions.

## Decision

Against the track's criteria: **no-go**. 32 functions lift with no wrong value over 1e5
inputs each, but none has a non-trivial POST (criterion: at least half), and the JavaScript
half was not attempted. The owner decided on 2026-11-25 to merge the Python front end into
main regardless (ADR 0012); this report records the criteria as measured.

## What would change the answer

- Contract inference (R-04) on the lifted nodes: the POST criterion is the gap.
- Lifter: guards inside branch arms and their result plumbing (the VG-T0108 failures), loops
  over 8-byte slices (`list[int]` parameters do not lift yet), and `while` loops with a
  provable bound.
- Verifier: a larger Alive2 budget or a cheaper encoding for 64-bit division, and Alive2's
  handling of 64-round loops at unroll 8.
- Coverage: methods and classes, keyword arguments, `None` defaults and strings are where
  library code lives; each is a front-end extension with its own semantic rules.
