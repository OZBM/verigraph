# Diagnostics

Every problem `vg` reports has a stable code. Codes are never renumbered, so an agent or a script can
act on them. docs/15-agent-interface.md is the full catalog, including the codes of later phases;
this chapter lists the codes version 0 reports.

## Reading a diagnostic

```json
{
  "code": "VG-T0103",
  "col": 0,
  "line": 0,
  "message": "LT mode does not match operand type i32",
  "node": "@less",
  "op": null,
  "severity": "error"
}
```

`line` and `col` are 1-based. Parse and structure errors point at the source; type and effect errors
are found on the canonical node, so they carry `line` 0 and name the node (the Antigravity extension
then underlines the node's `NODE` line). That diagnostic comes from this module:

```vgsym
// expect: error VG-T0103
module wrong_mode;

NODE @less {
  SIGNATURE: (a: i32, b: i32) -> (Bool);
  EFFECTS: [];
  CONTRACT {}
  BODY {
    %0 = LT.u(a, b);
    RETURN(%0);
  }
}
```

The fix is `LT.s`, the signed comparison that matches `i32`.

## Parse errors (P)

| Code | Rule | Fix |
| --- | --- | --- |
| VG-P0001 | Unexpected token | Read the message: it names what was expected. A missing `;` is the usual cause. |
| VG-P0002 | Unterminated string | Close the `"`; escape inner quotes as `\"`. |
| VG-P0003 | Integer literal does not fit its type | Use a wider type, or a value in range. |
| VG-P0004 | Reserved word used as a name | Rename it: `result`, `in`, `f32`, `NODE`, … are keywords. |
| VG-P0005 | Character that no token can start with | Remove it. |
| VG-P0006 | Decimal literal overflows its float type | Use a smaller value or `f64`. |

## Structure (S)

| Code | Rule |
| --- | --- |
| VG-S0001 | Port assigned twice |
| VG-S0002 | Port used before the statement that defines it |
| VG-S0003 | Unknown port or parameter |
| VG-S0004 | Label used before its NODE, SPEC or `@MESH_QUERY`: move the definition up |
| VG-S0005 | Unknown label |
| VG-S0006 | Label defined twice |
| VG-S0007 | Recursion or a call cycle (REC_CALL is not in the MVP) |
| VG-S0008 | RETURN missing, repeated, or not the last statement |
| VG-S0009 | `@MESH_QUERY` label with no pin in `vg.lock` |
| VG-S0101 | Unknown opcode |
| VG-S0102 | Mode missing, unknown, or not allowed for this opcode: every integer op needs `.checked`, `.wrap` or `.sat` |
| VG-S0103 | Wrong number of arguments |
| VG-S0104 | Op not available in the MVP |
| VG-S0105 | Named argument unknown, repeated, or before a positional one |
| VG-S0106 | Number of result ports does not match the op |
| VG-S0107 | FOLD_BOUNDED without `invariant=`, or naming no INVARIANT of this node |
| VG-S0108 | An argument that must be a literal is not one (the PROJ index) |
| VG-S0109 | A kernel or callee argument is not a label |
| VG-S0110 | A SPEC called from a body: use `spec:` in a formula |
| VG-S0201 … VG-S0204 | A `.vgir` file is malformed or not canonical |

## Types (T)

| Code | Rule |
| --- | --- |
| VG-T0101 | Operand type does not match |
| VG-T0102 | Operands of one op have different types |
| VG-T0103 | Mode does not match the operand type, such as `LT.u` on `i32` |
| VG-T0104 | Integer op on floats, or float op on integers |
| VG-T0105 | `f16` or `bf16` in arithmetic or a comparison: CAST it first |
| VG-T0106 | Kernel signature does not fit the combinator |
| VG-T0107 | Kernel has effects, several results, or a type that is not fixed-size |
| VG-T0108 | RETURN does not match the SIGNATURE results |
| VG-T0109 | Call arguments do not match the callee's parameters |
| VG-T0110 | ASSERT_GUARD branches have different signatures |
| VG-T0111 | Condition is not Bool |
| VG-T0112 | Index or length not `u64`, or shift amount not `u32` |
| VG-T0113 | CAST not allowed between these types: compare with NE to get a Bool |
| VG-T0114 | AS_BYTES of a type with no byte form |
| VG-T0115 | Port annotation disagrees with the inferred type, or OK or ERR without one |
| VG-T0116 | Refinement type nested inside another type |
| VG-T0117 | LEARNABLE parameter not a float, a bound not finite, or lo above hi |
| VG-T0118 | Static size above the format limits of docs/02 |
| VG-T0119 | Eff or a handle where only plain values are allowed |
| VG-T0120 | A returned slice may view the node's own frame: return the fixed-size value instead |

## Effects (E)

| Code | Rule |
| --- | --- |
| VG-E0101 | Op or call needs an effect that EFFECTS lacks; the message names it |
| VG-E0102 | Eff token used twice or never used |
| VG-E0103 | Effectful node without Eff as its first parameter and first result |
| VG-E0104 | Handle parameter whose effect is not declared |
| VG-E0105 | Reserved effect bit set |
| VG-E0106 | Op allocates, but ARENA is missing |

## Contracts (C)

| Code | Rule |
| --- | --- |
| VG-C0101 | Formula type error: an integer term meets a float term, or two float types meet |
| VG-C0102 | Decimal number with no float term to meet: write a typed literal |
| VG-C0103 | Unknown built-in, `spec:` or `fold:` name |
| VG-C0104 | `result` outside POST, or `result.j` out of range |
| VG-C0105 | Port referenced outside an INVARIANT |
| VG-C0106 | Unknown resource in COST or BUDGET, or a measured resource (`latency_p99_us`) in COST |
| VG-C0107 | `spec:` names a node that is not a SPEC |
| VG-C0108 | `fold:` names something that is not a FOLD-shaped kernel, or its arguments do not fit |

## Verification (V)

These are the codes of `vg verify` verdicts; [Verification](verification.md#verdicts) says what to do
about each.

| Code | Verdict |
| --- | --- |
| VG-V0001 | Counterexample |
| VG-V0002 | Contract gap |
| VG-V0003 | Vacuous contract |
| VG-V0004 | Unknown: timeout |
| VG-V0005 | Unknown: the solver answered unknown, or the obligation is not encoded yet (COST) |
| VG-V0006 | Unknown: the model violates an assumption that was only partly instantiated |
| VG-V0007 | Unknown: the model is too large to replay |
| VG-V0008 | Verifier error |

## Build (B) and warnings (W)

| Code | Rule |
| --- | --- |
| VG-B0002 | The target toolchain failed or is missing: clang could not compile or link |
| VG-W0001 | Op dropped by canonicalization: no output, effect or invariant uses it |
| VG-W0002 | INVARIANT declared but unused |
