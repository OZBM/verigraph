# 0003 · MVP memory: one arena per exported call

Status: proposed on 2026-10-29 for the owner's approval; applied to docs 02, 04, 05, 06, 13 and 14.

## Context

The spec listed `Arena<N>` and ARENA_ALLOC as MVP features but never typed ARENA_ALLOC, and never said where a new slice lives: the output of SIMD_MAP over a Slice, of GENERATE with a runtime count, of CONCAT, or of FILE_READ. vg-types, vg-interp and vg-codegen cannot be built without that answer, and the COST clause `arena_bytes` had nothing to measure.

## Decision

- Each call of an exported or root node receives one bump arena from its caller. Ops that create a slice of non-constant length allocate in it; views (SLICE, AS_BYTES, inputs) never allocate. The arena is released when the outermost exported call returns.
- A node that allocates, or calls a node that does, has ARENA in EFFECTS. ARENA needs no Eff token.
- An UPDATE of a slice is done in place only when the slice is the result of an allocating op with exactly one use; views and inputs are never updated in place, because other values may share their memory. Other UPDATEs of slices copy. Fixed-size values never allocate.
- A slice that reaches a node's results must view an input or arena memory, never a fixed-size value computed in the node (VG-T0120), so no view outlives the frame it points into.
- Kernels and accumulators are fixed-size, so loops never allocate.
- arena_bytes is computed exactly, as a symbolic expression over input and intermediate lengths, with each allocation rounded up to 16 bytes; COST bounds it and vg-smt proves the bound.
- `Arena<N>` and ARENA_ALLOC, explicit scoped regions, move to [ALPHA].

## Consequences

Every MVP program has a defined memory story that the cost model can bound exactly, and the M1 example needs no change. Programs that need memory to outlive one exported call, or to be reset inside a loop, wait for the alpha's explicit regions, which R-11 needs for the self-hosted compiler.
