# 0012 · Python ingest in OmniLift (R-13), merged at the owner's request

Status: accepted by the owner on 2026-11-25 ("finish the task and merge it with main, and do a
production-ready implementation"); applied to crates/vg-omnilift (ingest.rs, pylift.py,
lift.rs, analysis.rs, verify.rs), `vg lift --lang python` and docs/08. The evidence is in
docs/research/R-13-report.md.

## Context

R-13 (docs/research/R-13-dynamic-language-lifting.md) is a research track: by AGENTS.md its code
stays on research/r-13 behind the `r-13` feature until its report says go, and Python is used
only for research experiments. The owner asked on 2026-11-25 for Python in OmniLift, to start as
R-13 on its own branch, and then to finish it and merge it into main.

OmniLift lifts LLVM IR. Python has no static IR, and its ints are unbounded, so a lift needs a
front end that fixes a type for every value and a rule for what happens beyond int64.

## Decision

- **Subset.** A typed, pure, int-only subset of Python (`crates/vg-omnilift/src/pylift.py`):
  int, bool, bytes and read-only `list[int]` / `tuple[int, ...]` parameters; int or bool
  results; module constants and int tables that nothing in the module can rebind (any binding
  form, star imports included); top-level functions without recursion, decorators,
  comprehensions, nested functions, try, with, floats or strings. Anything else is rejected
  with VG-L0001 and the reason; the front end never guesses.
- **Meaning.** The lift returns CPython's value, or Result::Err where CPython raises (any
  exception) or where a value leaves int64. pylift.py writes UB-free C in which every operation
  sets an error bit at those points, and the C entry function ends in the only UB:
  `py_fail(e, v)` divides v by 1 - e. OmniLift cures it into Err as for any C division.
- **Pipeline.** `python3 -I` runs the embedded pylift.py; clang compiles its C at -O0 without
  optnone (so no fold sees through py_fail); opt normalizes with mem2reg, a simplifycfg that
  only merges blocks (no speculation, hoisting or sinking), loop-simplify, loop-rotate, lcssa
  and indvars. Alive2 and the differential fuzzer then run as for every language, except that
  the Alive2 source of a Python lift skips the passes that exploit UB (instcombine, gvn,
  reassociate), which would otherwise assume the error bit is 0 and fold the Err away.
- **Toolchain.** Python 3.11 or newer becomes a dependency of `vg lift --lang python` only;
  every other command and language works without it. AGENTS.md's "Python only for research
  experiments" now reads: and for OmniLift's Python front end.
- **Feature flag.** The `r-13` feature is removed on merge: Python ingest is part of the
  default build.

## Consequences

- The lifter fixes that R-13 exposed are general and stay for every language: an out-of-bounds
  read of a constant table is cured to Err instead of reading a clamped element; a signed value
  is reinterpreted as unsigned with AND, CAST and SELECT instead of CAST.in_range, which trapped
  on negative values; interval analysis reads llvm.umin and llvm.umax; i1 values that cross a
  branch merge are typed Bool; slices support 2- and 8-byte little-endian loads.
- The R-13 go criteria (30 functions with 100% agreement over 1e5 inputs, half with a
  non-trivial POST) are reported as measured in R-13-report.md; the merge does not depend on
  them, by the owner's decision. JavaScript and the NumPy spec library were not attempted and
  stay in R-13's scope.
- Real library code mostly falls outside the subset (methods, strings, floats, generics), and
  loops whose trip count is not computable fail in the lifter: the share of a library that
  lifts is small, and the report gives it.
