# 06 · Codegen and runtime

Proven obligations are erased before lowering, and the proofs pay their way: no-overflow and in-bounds facts become LLVM flags that C code only gets from undefined behavior or hand annotations.

## Pipeline

1. **Erase.** Drop CONTRACT, DEBUG and every proven ASSERT_GUARD. Keep unproven guards, which the build reports as its residual guard count, and the checked entry points of exported and root nodes (ADR 0009). An `ASSERT_GUARD.branch` is control flow, not a guard: it stays a branch and is never counted.
2. **Lower combinators to loops.** SIMD_MAP and GENERATE become loops with vectorization metadata, FOLD_BOUNDED a loop with its accumulator in registers, SCAN_PREFIX a sequential or blocked parallel scan.
3. **Place memory.** Fixed-size values go in caller-provided buffers or on the stack, uniquely owned values are updated in place, and the call's arena is a bump allocator (ADR 0003). A fixed-size intermediate larger than 4 KiB goes in a scratch block at the start of the arena instead of the stack, because secondary threads may have stacks as small as 512 KiB; its size is known at compile time, and the arena size function adds it.
4. **Attach proof facts.** `nsw` and `nuw` where no-overflow is proven, `inbounds` where indices are, `noalias` for unique buffers, `llvm.assume` for proven preconditions. Never fast-math flags.
5. **Emit textual LLVM IR**, one internal function per node, named `vgi_<12-hex hash>`, plus a checked entry `vg_<12-hex hash>` for each exported or root node. Effects become calls into vg-rt.
6. **Build with clang** per target and link vg-rt. Outputs: the binary, its sandbox manifest, a symbol map (hash to symbol) and a guard map (guard to obligation) for telemetry.

Every build can also be made with `--no-proof-facts`; differential tests compare both builds with vg-interp (docs/16), so a wrong fact shows up as a disagreement instead of a silent miscompilation.

## ABI (MVP)

`ctx` is the runtime context: handles, the call's arena, the telemetry sink. The Eff token disappears at lowering: it only fixes the order of calls.

| VG type | C representation |
| --- | --- |
| Bool | `uint8_t`, 0 or 1 |
| uN, iN | `uintN_t`, `intN_t` |
| f32, f64 | `float`, `double`; f16 and bf16 as `uint16_t` bit patterns |
| Array<T, N>, Tensor<T, [d…]> | `T[N]` or `T[d1 × … × dn]`, row-major, passed by pointer |
| Slice<T> | `struct { T *ptr; uint64_t len; }`, with `const T *` for inputs |
| Tuple<…> | a struct with the members in order and natural C alignment |
| Result<T, E> | `struct { uint8_t is_ok; union { T ok; E err; } v; }` |
| Handles | `uint32_t`, an index into ctx's handle table |

- **Internal:** `void vgi_<hash12>(vg_ctx *ctx, inputs…, outputs…)`. Scalars pass by value, aggregates by pointer, inputs as `const`, and outputs go into caller-provided buffers or, for slices, into the arena. It assumes PRE and is called only from verified code.
- **Checked entry:** `int32_t vg_<hash12>(vg_ctx *ctx, inputs…, outputs…)` validates the inputs, sets the default floating-point environment (below), calls the internal function, and restores the caller's environment. It returns 0 on success, 1 when PRE fails, 2 for an invalid input encoding (a Bool byte other than 0 or 1, a non-canonical NaN pattern), and 3 when the arena is too small. Outputs are written only on success.
- **Arena size:** every exported node gets `uint64_t vg_<hash12>_arena_bytes(…)`, which returns E for the caller's inputs plus the static scratch size, where E comes from the node's COST conjunct `arena_bytes <= E` over its parameters (required when the node allocates; 0 otherwise).
- **Header:** `<module>.h` declares the checked entries and arena functions, with readable aliases such as `#define audio_limiter_limit_frame vg_<hash12>`. Twelve hex digits are unique within one binary; on a clash every clashing symbol grows to 16, then 24 digits, deterministically.

## Floating point on every target

The float rules of docs/13 hold on every target only if codegen keeps them; these rules are what makes vg-interp, x86_64, ARM64 and WASM agree byte for byte.

- **No fast-math and no contraction.** No fast-math flag on any instruction; FMUL followed by FADD is never fused; FMA is always `llvm.fma`, never `llvm.fmuladd`. vg-rt's own C is compiled with `-ffp-contract=off`.
- **One NaN in memory.** Before a float that may be NaN is stored to memory a VG op or a caller can read (output buffers, arena slices, tensor results), codegen replaces NaN by the canonical NaN (ADR 0002). A value proven not NaN skips the select; in `@limit_frame`, `@soft_knee`'s POST proves it for every sample. A node's scalar float result is not such memory: it reaches its caller as it is, and the caller canonicalizes it where it becomes bytes (a SIMD_MAP or GENERATE element, the checked entry's output), so a FOLD_BOUNDED step carries no compare and select on its accumulator.
- **Default environment only.** Verified code runs with rounding to nearest-even and subnormals neither flushed nor treated as zero, the default on all three targets (MXCSR 0x1F80 on x86_64, FPCR 0 on Darwin AArch64; WebAssembly has no other mode). vg-rt sets it at startup, and every checked entry saves the caller's MXCSR or FPCR, sets the default, and restores the saved value on return. Audio hosts often set flush-to-zero for speed; without this, an exported node called from one would compute different bits than vg-interp.
- **Indices on wasm32.** Indices and lengths stay u64 in the language and the ABI; on wasm32, codegen truncates an index to 32 bits only after the in-bounds proof, since no slice there can exceed 2^32 bytes.
- **Min, max and sign.** FMIN and FMAX lower to `llvm.minimum` and `llvm.maximum`, FCOPYSIGN to `llvm.copysign`; never to the libm functions `fmin` and `fmax`, whose NaN rule differs.

## Targets

| Target | Triple | CPU baseline | Phase |
| --- | --- | --- | --- |
| Linux x86_64 | x86_64-unknown-linux-gnu | x86-64-v3 (AVX2, FMA, BMI2), checked at startup | [MVP] |
| macOS ARM64 | aarch64-apple-darwin | apple-m1 | [MVP] |
| WASM | wasm32-wasip1 (wasi-sdk) | simd128 on, relaxed-simd off (its results are non-deterministic) | [MVP] |
| Linux ARM64, Windows x86_64 | aarch64-unknown-linux-gnu, x86_64-pc-windows-msvc | [ALPHA] |
| Tensors and GPU | MLIR linalg to LLVM; GPU through the MLIR gpu dialect or Metal shaders | After [ALPHA] |

Text comes first because emitting textual LLVM IR avoids binding to one LLVM version's C++ API. MLIR joins in the alpha for tensors, also as text through `mlir-opt` and `mlir-translate`.

## Overhead, measured

The source spec's "0% overhead" holds only for proven checks; explicit guards that could not be proven still run. Each build reports its residual guard count. Gate G4: within 20% of equivalent `clang -O2` C on five kernels. M2: within 5%.

Suggested kernels: CRC-32 over 1 MiB, the audio limiter on 512×2 frames, SHA-256 over 1 MiB, a 256×256 f32 matrix multiply, a saturating 8-bit image blend.

## Runtime (vg-rt)

About 1,000 lines of C, compiled per target by clang:

- An entry point that checks the CPU baseline and the floating-point environment, opens the manifest's handles (docs/04), sizes the arena from the root's COST, and calls the root's checked entry.
- The bump arena, and effect shims for files and the clock, then sockets.
- On Linux, it applies the Landlock ruleset and seccomp filter at startup.
- A telemetry writer: JSON lines such as `{"event": "guard", "guard": "g3", "obligation": "@n/%4/INDEX.in_bounds", "node": "<hash12>", "target": "<triple>"}`. Fatal errors abort, with no unwinding.
- Opt-in effect-trace recording, size-capped, in the trace format of docs/16, so a field failure replays in vg-interp.

The symbol map is a JSON object from symbol to node_hash; the guard map a JSON object from guard id to obligation ID and obligation hash.

vg-rt is part of the trusted base: keep it small, fuzz its parsers, and never add features the manifest cannot describe.

## Learnable parameters

- Parameters are implicit trailing inputs (θ). A kernel stays pure and deterministic for a given θ, and its hash never changes when θ does.
- The verifier proves every contract for all θ in the declared box, so tuning can never break a proof.
- A separate tuner node with [PARAM_UPDATE, CLOCK, NONDET] updates θ and projects it back into the box after each step.
- Smooth kernels get gradients from a forward-mode autodiff transform of the graph [ALPHA]. Hard thresholds, such as PARTITION_BY on θ, have zero gradient almost everywhere, so they use gradient-free tuners such as SPSA or bandits (R-07).
- Release builds load a frozen θ file, so outputs are reproducible; online tuning is opt-in. The MVP has no tuner, so a parameter's `loss:` label stays optional until tuning lands (P9, R-07).
