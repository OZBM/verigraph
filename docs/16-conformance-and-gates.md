# 16 · Conformance, tests and gates

What every test suite contains, the file formats that let a gate script read a test's expectation, the laws that property tests check, and what each `make gate-gN` runs. A gate's criteria stay those of ROADMAP.md; this chapter makes them executable.

## Test files

| Path | Holds | Expectation |
| --- | --- | --- |
| `tests/corpus/good/*.vgs` | programs that must verify | `// expect: verify` |
| `tests/corpus/bad/*.vgs` | programs that must be refuted | `// expect: reject <obligation ID>` |
| `tests/corpus/errors/*.vgs` | programs a static check must reject (P2.6) | `// expect: error <code> <node> <port>` |
| `tests/corpus/alpha/<family>/*.vgs` | variants that must canonicalize to identical bytes | the folder is the family |
| `tests/corpus/distinct/<family>/*.vgs` | near-miss variants that must all hash differently | the folder is the family |
| `tests/conformance/<opcode>.vgs` | one node per op and mode, with vectors (P2.5) | `// run:` lines |
| `tests/golden/*.vgir` | byte-exact encodings pinned by ADR 0001 | file name = source name |

**Expectation lines** are comments at the top of a file, so every file stays valid VG-Sym, and examples/ uses them too:

```text
// expect: reject @soft_knee/POST.0
// expect: error VG-T0103 @scale %2
// run: @soft_knee {"knee": "0.5", "x": "3e38"} => ["1.0"]
// run: @limit_frame file:vectors/limit_frame.json
```

A `reject` passes only when that obligation's verdict is `counterexample` and the counterexample reproduces concretely in vg-interp. An `error` passes only when that code is reported at that node and port, and no other error is. A `run` line gives inputs and expected outputs in the JSON encoding of docs/15; a `file:` vector holds `{"node": "@n", "cases": [{"inputs": {}, "theta": {}, "trace": [], "outputs": []}]}`.

**Effect traces** script the environment for vg-interp (docs/13): a list of steps in Eff-chain order, each `{"op": "FILE_READ", "result": {"ok": [104, 105]}}`. A step whose op does not match the next effect op is a test error, not a program error.

## Laws and their property tests

These are the laws of docs/13's foundations table. Each runs with a fixed, logged seed.

| Law | Test |
| --- | --- |
| Parse after print is the identity | every corpus file and every fuzz input that parses |
| `asm(disasm(b)) = b` | every corpus file's .vgir and every golden file |
| `disasm(asm(s))` is a fixed point | printing the result again changes nothing |
| Canonicalization is idempotent | canonicalizing a canonical node changes no byte |
| Equivalence moves keep the hash | each alpha family gets at least 10 random variants per run, built by random sequences of the moves of docs/13 |
| Inequivalent graphs get different hashes | every pair of files in each distinct family; plus random near-misses: swap a non-commutative operand pair, change a mode, flip a constant's bits (0.0 to −0.0 included), swap two effect ops, reorder parameters or conjuncts |
| Hashes follow unraveling | for random graphs of at most 30 ops, two bodies share a body_hash exactly when a slow reference implementation unravels them to the same term, up to the commutative swaps |

The distinctness tests guard against the worse failure: a canonicalizer that merges two different programs passes every alpha family. They belong in the G1 suite (owner question in PROGRESS.md).

## Interpreter conformance (P2.5)

One node per opcode and mode, with vectors over these values:

- **Integers:** 0, 1, MAX, MAX − 1, 2^(N/2), and for signed types −1, MIN, MIN + 1; plus 16 seeded random values.
- **Floats:** ±0, ±the smallest and largest subnormals, ±the smallest normal, ±1, ±the largest finite value, ±∞, NaN, 0.1, and operand pairs whose exact result lies halfway between two floats, to test ties-to-even.
- **Shift amounts:** 0, 1, N − 1, N, N + 1, 2^32 − 1.

Expected outputs come from `tools/gen_conformance.py`, which computes each result in exact rational or big-integer arithmetic and then rounds as IEEE 754 specifies; it never calls the implementation under test. NaN results are compared as NaN, and every output's bytes as docs/13 encodes them.

## Verifier suites (P3.6, G3)

- **Known-good** files verify, and their `run` vectors pass.
- **Known-bad** files cover every obligation kind of docs/15 and every op obligation of docs/14 with at least one case each.
- **Mutation testing** applies these operators to each known-good node: flip a comparison (LT and LE, GT and GE, EQ and NE); change a mode (checked, wrap, sat; s and u); replace a constant (±1, 0, MIN, MAX, −0.0 for 0.0, NaN, ±∞); swap the operands of a non-commutative op; replace an operand with another port of the same type; replace a plain ASSERT_GUARD with a direct call to its then branch; swap SELECT's branches.
- **Classifying a mutant that verifies.** Search for a violation: the pre-screen inputs, 10,000 seeded random inputs, and every counterexample found so far. If an input satisfies PRE and makes the mutant trap or break POST in vg-interp, the verifier accepted a contract-breaking program, and CI fails. Otherwise the mutant survives: it is equivalent, or the contract is too weak to tell. Survivors fail nothing; they lower the contract's strength score (R-04).

## Differential testing (P4.7, G4)

vg-core's `gen` feature (W1) generates well-typed programs from a seed, in two classes:

- **Class A, total.** Integer ops in wrap and sat mode, every float op, comparisons, SELECT, bit ops, widening CASTs, and Array indexing whose index is reduced modulo a constant length. Contracts are `true`, so the only obligations are proven by construction.
- **Class B, contracted.** Checked ops whose safety follows from generated PREs (input ranges), plus SIMD_MAP and FOLD_BOUNDED with generated kernels and invariants.

Programs have 5 to 60 ops. Each one is verified, built for the three targets, and run on 100 inputs, boundary values first; outputs must match vg-interp byte for byte. Each program is built twice, with and without proof facts (`--no-proof-facts`); a disagreement in the build with facts only means a wrong fact reached LLVM, which is a soundness bug.

## Fuzzing (P1.9)

| Target | Input | Must hold |
| --- | --- | --- |
| `parse` | arbitrary bytes | no panic; if it parses, print then parse gives the same tree |
| `codec` | arbitrary bytes | no panic; if it decodes as canonical .vgir, encoding gives the same bytes |
| `canon` | arbitrary well-formed trees (the `arbitrary` crate) | canonicalization is idempotent, and a random equivalence move keeps the hash |

Seeds are every corpus file and its .vgir. A change runs each target for 10 minutes; nightly and gate runs use `-max_total_time=3600` per target. Every crash is minimized and added to the corpus.

## Benchmarks (P4.8, G4, M1, M2)

- Each kernel lives in `benchmarks/kernels/<name>/` with its VG-Sym, an equivalent C file, a seeded input file and the expected output. VeriGraph and C must produce identical bytes on that input, or the benchmark is invalid.
- C is built with `clang -O2 -ffp-contract=off`, for the same triple and CPU baseline as VeriGraph (docs/06), without `-ffast-math`, so both sides compute the same function.
- The harness alternates the two binaries for 10 rounds. Each round makes 100 warm-up calls, then calls until 1 second or 1,000 calls have passed, timing each call with the monotonic clock.
- The statistic is the median call time; the ratio is median(VeriGraph) / median(C), reported with a seeded bootstrap 95% interval. G4 requires a ratio of at most 1.20 for every kernel on both native targets; WASM ratios are reported.
- BUDGET clauses use the same harness: `latency_p99_us` is the 99th percentile of at least 10,000 timed calls after warm-up, on each target.
- Raw timings go in `reports/bench/`, with CPU model, OS, clang version and flags.

## What each gate script checks

| Gate | `make gate-gN` runs and requires |
| --- | --- |
| G1 | The round-trip and idempotence laws on every corpus and golden file; at least 50 alpha families × 10 variants with identical hashes; agreement of vg-core's parser with tools/vgsym.lark on every corpus file and lexical case; each fuzz target for 3,600 s with no crash |
| G2 | Every file in `tests/corpus/errors` rejected with exactly its code and location; every conformance vector bit-exact in vg-interp; the capability-escalation cases (an effect op in a pure node, a callee mask not contained, an effect in a kernel, an Eff token used twice, a handle without its bit) all rejected |
| G3 | Every known-good file proven, with two solvers in release mode; every known-bad file rejected at its obligation with a concrete counterexample, covering every obligation kind; zero contract-breaking mutants verified; examples/soft_knee_draft.vgs rejected at `@soft_knee/POST.0` and examples/audio_limiter.vgs verified; no VG-V0008 or VG-V0009 anywhere |
| G4 | 1,000 generated programs agree with vg-interp on three targets, with and without proof facts; five kernels at a ratio of 1.20 or less; a root that opens an ungranted file is refused by the OS sandbox on each target, and the test asserts the refusal |
| G5 | 30 tasks with a hosted model, at most 5 repair rounds each. A task succeeds only when the final node verifies **and** satisfies the task's reference contract, proven by vg-smt, **and** passes the task's tests; a model that weakens POST to `true` does not succeed. Success on at least 18 tasks; zero unsound accepts, meaning zero verified nodes that break their own contract on any test input |
| G6 | On the 60 seeded bugs, the failing slice has 5 ops or fewer on at least 48 |
| G7 | On 100,000 synthetic nodes, the p99 of 10,000 warm indexed queries is under 5 ms; every tampering in the suite is rejected: a flipped byte, a wrong hash, a forged or missing signature, an understated effect mask, a reordered or truncated log, a replayed submission |
| G8 | crc32, adler32 and both SHA-256 implementations lifted; Alive2 passes at an unroll bound of 8 or more; 1e6 differential fuzz cases pass per function; both SHA-256 lifts join one spec class, with the evidence level recorded (docs/09) |
