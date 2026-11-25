# 08 · OmniLift

OmniLift turns legacy functions into verified VG-IR nodes without pretending that different implementations share a hash.

## Pipeline

1. **Ingest.** Compile the source to LLVM IR with unrolling and vectorization off, so loops stay recognizable (crates/vg-omnilift/src/ingest.rs; `vg lift --lang`):

   | Language | Front end and flags | Notes |
   | --- | --- | --- |
   | C | clang `-O1 -fno-unroll-loops -fno-vectorize -fno-slp-vectorize` | the G8 pipeline |
   | C++ | the same, with `-x c++ -std=c++20` | entry points `extern "C"`, or pass the mangled name |
   | Rust | rustc `opt-level=1`, `-unroll-threshold=0`, `no-vectorize-loops`, `no-vectorize-slp` | `#[unsafe(no_mangle)] extern "C"` entry points |
   | Zig | `zig build-obj -O ReleaseSmall -fstrip` | Zig has no -O1 or loop switches; ReleaseSmall neither unrolls nor vectorizes and has no safety calls. `export fn` aliases are folded back into definitions, `minsize`/`optsize` hints are dropped, and normalization adds `loop-rotate` |
   | Swift | swiftc `-O -emit-ir`, `-Xllvm -vectorize-loops=false -vectorize-slp=false -unroll-threshold=0` | `@_cdecl` entry points. Trapping arithmetic (`+`) lowers to overflow intrinsics and a trap, which are rejected; the wrapping operators (`&+`, `&*`) lift |
   | GLSL | `glslc` validates the source as a compute shader, then clang compiles it as C++ with `-fwrapv` and a prelude (`uint`, `min`, `max`, `clamp`, `abs`) | the scalar integer subset. GLSL int arithmetic wraps, hence `-fwrapv`. Rejected because their C++ meaning differs: unsuffixed hex or octal literals above `0x7FFFFFFF` (an `int` in GLSL, `unsigned` in C++), and any pointer parameter after lowering (an array or struct passed by value in GLSL becomes a reference) |

   | Python | `crates/vg-omnilift/src/pylift.py` (run with `python3 -I`) translates a typed, pure, int-only subset to C; clang compiles it at `-O0` without optnone; normalization adds a block-merging `simplifycfg` and `loop-rotate` | ADR 0012 and docs/research/R-13-report.md. The lift returns CPython's value, or Err where CPython raises or a value leaves int64. Unannotated parameters are int; bytes and read-only `list[int]` / `tuple[int, ...]` parameters; module int constants and tables nothing can rebind. Everything else is rejected with the reason |

   Every language shares the rest of the pipeline, so its lifts get the same Alive2 check and differential fuzzing. A shading language other than GLSL was tried: Homebrew's clang ships no HLSL headers, and the Metal compiler emits typed-pointer IR for the `air64` triple, which LLVM 23's `opt` and Alive2 cannot read.
2. **Normalize.** Run `mem2reg`, loop-simplify, LCSSA and the induction-variable passes with `opt`.
3. **Lift mechanically.** Every counted loop becomes FOLD_BOUNDED over its whole mutable state, pointers become (buffer, offset) pairs with bounds, and constant tables, such as crc32's lookup table and SHA-256's round constants, become array constants. This lift always works and becomes the reference.
4. **Cure undefined behavior.** Where the original has UB (out-of-bounds access, signed overflow, division by zero), strengthen PRE or insert an ASSERT_GUARD that returns `Result::Err`. The lifted node agrees with the original wherever the original is defined, and fails safely where it is not.
5. **Check against the original.** Lower the cured lift back to LLVM IR through a thin adapter with the original's signature: PRE becomes `llvm.assume` and `Result::Err` becomes `unreachable`, both undefined behavior, which Alive2 accepts in the lowered side only where the original's behavior is undefined too. Run Alive2 (`alive-tv original.ll lowered.ll`) on the pair; it proves refinement up to a loop-unroll bound, and it fails if a cure rejects an input the original defines. Differential fuzzing of both binaries backs it up on inputs where a sanitizer build of the original reports no UB. Unbounded loops need invariants (R-03, R-04).
6. **Rewrite idiomatically** into SIMD_MAP, SCAN_PREFIX or PARTITION_BY, proven equal to the cured lift inside VeriGraph, where both sides share one semantics. Two folds over the same input are related by an invariant such as `acc == fold:lifted_step(init, xs, k)`, which reduces the proof to one quantifier-free query about the two step kernels (ADR 0008).
7. **Publish** with provenance: repository, commit, file, function, compiler and flags.

## MVP scope

Functions in the languages above with no I/O, recursion or pointer-to-pointer data, whose loops have computable trip counts. When a lift folds over a `GENERATE`d index range, its trip count is a scalar argument, and the fuzzer draws scalars from `[0, 4096)` so that each case ends; Alive2 still covers every value up to its unroll bound. Targets for G8: zlib's `crc32` and `adler32`, one C implementation of SHA-256, and the portable backend of Rust's `sha2` crate. Both SHA-256 lifts must join the SHA-256 spec class (docs/09), with `proved` evidence where a fold invariant carries the proof and `bounded(n)` evidence otherwise, stated in the G8 report.

## Prior art

Verified lifting (LLMLift, NeurIPS 2024: an LLM proposes, a verifier checks); Tenspiler and C2TACO for lifting to tensor DSLs; SAW for equivalence proofs of cryptographic code; Alive2 (PLDI 2021) for bounded translation validation.
