# 0009 · Checked entry points at the trust boundary

Status: proposed on 2026-10-29 for the owner's approval; applied to docs 06 and 13.

## Context

Proof facts make compiled code assume PRE: `llvm.assume`, `nsw`, `inbounds`. A C, Swift or Rust caller of an exported node is not verified, so a bad input would turn those facts into undefined behavior. docs/06 already kept "the validation of data entering the root node", but did not say what is validated or how an exported node reports a rejection.

## Decision

- Each exported or root node gets two symbols: `vgi_<hash12>`, the internal function that assumes PRE and is called only from verified code, and `vg_<hash12>`, the checked entry point.
- The checked entry evaluates PRE and the refined parameter types on the actual inputs, compiled from the formulas (bounded `forall` becomes a loop, `spec:` calls run the SPEC node), rejects Bool bytes other than 0 and 1 and float inputs holding a non-canonical NaN pattern, checks that the caller's arena is large enough, and only then calls `vgi_<hash12>`.
- It also saves the caller's floating-point control register, sets the default environment (round to nearest-even, no flushing of subnormals) for the call and restores the saved value afterwards, because a host that sets flush-to-zero, as audio hosts often do, would otherwise make the node compute different bits than vg-interp.
- It returns an `int32_t`: 0 on success, 1 when PRE fails, 2 for an invalid input encoding, 3 when the arena is too small. Outputs are written only on success.

## Consequences

Proofs stay sound at the boundary, at the cost of one pass over the inputs per external call, which is about 1,024 finiteness checks for `@limit_frame`. Calls between verified nodes pay nothing.
