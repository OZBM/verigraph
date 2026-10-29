# 0001 · Byte encodings of the .vgir format

Status: stub, written in P0.6. W1 completes it in P1.6, before G1 freezes the format.

## Context

docs/02-vg-ir-format.md fixes the 128-byte header, the section directory and the section
kinds, and lists "proposed encodings" for TYPES, CONSTS, OPERANDS.src_kind, CALLS and
INVARIANT ordering, LEARNABLE.loss and SIGNATURE.role. It also leaves the exact opcode
numbering inside each opcode range to this ADR.

## Decision

Not yet made. To be fixed here in P1.6:

- the byte layout of every section, confirming or amending the proposals in docs/02;
- opcode numbers inside each range of the opcode table, starting from docs/14's proposal;
- the CONTRACT layout and formula tags, starting from ADR 0007;
- the golden files that pin them.

## Consequences

Once G1 passes, any change to these encodings needs a new ADR, the owner's approval and a
format_version bump (AGENTS.md section 2).
