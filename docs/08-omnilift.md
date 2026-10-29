# 08 · OmniLift

OmniLift turns legacy functions into verified VG-IR nodes without pretending that different implementations share a hash.

## Pipeline

1. **Ingest.** Compile C, C++, Rust or Zig to LLVM IR at `-O1`, with unrolling and vectorization off (`-fno-unroll-loops -fno-vectorize -fno-slp-vectorize` for clang; the equivalent codegen options for rustc), so loops stay recognizable.
2. **Normalize.** Run `mem2reg`, loop-simplify, LCSSA and the induction-variable passes with `opt`.
3. **Lift mechanically.** Every counted loop becomes FOLD_BOUNDED over its whole mutable state, pointers become (buffer, offset) pairs with bounds, and constant tables, such as crc32's lookup table and SHA-256's round constants, become array constants. This lift always works and becomes the reference.
4. **Cure undefined behavior.** Where the original has UB (out-of-bounds access, signed overflow, division by zero), strengthen PRE or insert an ASSERT_GUARD that returns `Result::Err`. The lifted node agrees with the original wherever the original is defined, and fails safely where it is not.
5. **Check against the original.** Lower the cured lift back to LLVM IR through a thin adapter with the original's signature: PRE becomes `llvm.assume` and `Result::Err` becomes `unreachable`, both undefined behavior, which Alive2 accepts in the lowered side only where the original's behavior is undefined too. Run Alive2 (`alive-tv original.ll lowered.ll`) on the pair; it proves refinement up to a loop-unroll bound, and it fails if a cure rejects an input the original defines. Differential fuzzing of both binaries backs it up on inputs where a sanitizer build of the original reports no UB. Unbounded loops need invariants (R-03, R-04).
6. **Rewrite idiomatically** into SIMD_MAP, SCAN_PREFIX or PARTITION_BY, proven equal to the cured lift inside VeriGraph, where both sides share one semantics. Two folds over the same input are related by an invariant such as `acc == fold:lifted_step(init, xs, k)`, which reduces the proof to one quantifier-free query about the two step kernels (ADR 0008).
7. **Publish** with provenance: repository, commit, file, function, compiler and flags.

## MVP scope

C and Rust functions with no I/O, recursion or pointer-to-pointer data, whose loops have computable trip counts. Targets for G8: zlib's `crc32` and `adler32`, one C implementation of SHA-256, and the portable backend of Rust's `sha2` crate. Both SHA-256 lifts must join the SHA-256 spec class (docs/09), with `proved` evidence where a fold invariant carries the proof and `bounded(n)` evidence otherwise, stated in the G8 report.

## Prior art

Verified lifting (LLMLift, NeurIPS 2024: an LLM proposes, a verifier checks); Tenspiler and C2TACO for lifting to tensor DSLs; SAW for equivalence proofs of cryptographic code; Alive2 (PLDI 2021) for bounded translation validation.
