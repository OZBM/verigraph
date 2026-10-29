# 0002 · One NaN, written canonically

Status: proposed on 2026-10-29 for the owner's approval; applied to docs 02, 05, 06, 13 and 14. It changes canonicalization, so it must be settled before G1 freezes the format.

## Context

The spec promised bit-exact floats, NaN payloads included, and kept float ops out of commutative sorting "because NaN payloads depend on operand order". No target delivers that. LLVM LangRef: when an operation returns NaN, "the result has a non-deterministic sign; the quiet bit and payload are non-deterministically chosen" among several options. x86_64 and AArch64 produce different default NaNs, and WebAssembly's NaN propagation is non-deterministic outside its deterministic profile, which produces "only positive canonical NaN". SMT-LIB's floating-point theory has a single NaN. So payload-exact semantics could be neither compiled portably nor verified.

## Decision

- A float value has one NaN, with no sign and no payload, as in SMT-LIB.
- Wherever a float becomes bytes (AS_BYTES, `as_bytes`, stores to memory, outputs of exported nodes, CONSTS), NaN is written as the canonical quiet NaN with a zero sign: 0x7E00, 0x7FC0, 0x7FC00000, 0x7FF8000000000000 for f16, bf16, f32, f64. Every NaN pattern read from bytes is NaN. CONSTS holding any other NaN pattern are not canonical (VG-S0204).
- FCOPYSIGN requires its sign operand not to be NaN (FCOPYSIGN.sign).
- FADD, FMUL, FMIN, FMAX, EQ and NE on floats become commutative for canonicalization, and FMA in its first two operands.
- vg-codegen canonicalizes a float at every store to memory and every exported output unless the verifier proved it is not NaN; inputs of exported nodes holding a non-canonical NaN pattern are rejected at the checked entry (ADR 0009).

## Consequences

vg-interp, vg-smt and all three targets agree on every byte. Canonicalization merges more float code. The cost is one compare and select per possibly-NaN float store, removed wherever a proof covers it, as it does in `@limit_frame`. Programs can no longer carry information in NaN payloads; none of the MVP's targets let them do so portably anyway.
