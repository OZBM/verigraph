# 0001 · Byte encodings of the .vgir format

Status: proposed on 2026-10-30 for the owner's approval, with the G1 report (reports/gate-g1.md). It
completes the stub written in P0.6. The format and canonicalization freeze when G1 passes; after
that, any change here needs a new ADR, the owner's approval and a format_version bump.

## Context

docs/02-vg-ir-format.md fixes the 128-byte header, the section directory and the section kinds, and
proposed encodings for the sections, the numbering of tables and the hashes. docs/14 proposed the
opcode numbers. P1 implemented them in vg-core (`codec.rs`, `canon.rs`) and found five places where
the proposal could not be implemented as written or would have broken a law of docs/13. This ADR
records every encoding as built, marks each amendment, and pins the result with golden files.

## Decision

### Header and layout (as docs/02)

- 128-byte header, little-endian: magic `VGIR`, format_version `0x0002`, flags (bit 0 has_debug,
  bit 2 spec_node; bit 1 scc_member is rejected until REC_CALL arrives in the alpha), effect_mask,
  section_count, node_hash, body_hash, contract_hash, 16 reserved zero bytes.
- Directory entries `{kind u16, flags u16, count u32, offset u32, length u32}`, flags zero, in
  strictly ascending kind order. `count` is the number of entries; for CONTRACT, the number of
  clauses; for DEBUG, 1. `length` excludes padding.
- **A section with no entries is left out.** The first section starts at the first 16-byte boundary
  after the directory, each next one at the first 16-byte boundary after the previous one, and the
  file ends at the 16-byte boundary after the last. Padding is zero. So the layout is a function of
  the section lengths, and decoding rejects any other.

### Sections

| Kind | Section | Entry |
| --- | --- | --- |
| 0x01 | TYPES | `{tag u8, argc u8, reserved u16, args u32[argc]}` |
| 0x02 | SIGNATURE | `{type u32, role u16, flags u16}`: role 0 parameters, 1 results, 2 θ, in that order |
| 0x03 | CONSTS | `{type u32, len u32, bytes[len]}`, padded to 4 bytes with zeros |
| 0x04 | OPS | `{opcode u16, mode u8, result_count u8, operand_start u32, operand_count u32, attr u32}`, 16 bytes |
| 0x05 | OPERANDS | `{src_kind u8, src_slot u8, reserved u16, src_index u32}`, 8 bytes |
| 0x06 | RESULTS | one TYPES index (u32) per op result, op by op |
| 0x07 | CALLS | 32-byte hashes, sorted ascending, without duplicates |
| 0x08 | CONTRACT | clause table, then the node array |
| 0x09 | LEARNABLE | `{param u32, type u32, lo u32, hi u32, loss u32}`, 20 bytes |
| 0x7F | DEBUG | `version u32` (1), then the node's VG-Sym source as written, UTF-8; never hashed |

- **TYPES (amended).** Tags as docs/02: 0x01 Bool, 0x02 UInt, 0x03 SInt, 0x04 Float, 0x05 BFloat16,
  0x06 Int, 0x10 Array, 0x11 Slice, 0x12 Tensor, 0x13 Tuple, 0x14 Result, 0x15 Arena, 0x16 Eff,
  0x17 Handle. Arguments that are types are TYPES indices; **bit widths (8, 16, 32, 64; floats 16,
  32, 64), Array lengths, Tensor dimensions, Arena sizes and handle kinds (FileIn 0, FileOut 1,
  Socket 2, Clock 3, GpuQueue 4) are u32 immediates**, not CONSTS indices. docs/02 proposed CONSTS
  indices for sizes, which makes `u64` depend on a constant of type `u64`; the static limit of
  docs/02 (at most 2^32 − 1 elements) makes u32 enough. Tag 0x18 (refinement) is reserved: a refined
  parameter or result is stored as its base type, and its formula joins PRE or POST as ADR 0007 says,
  so `x: {v: u8 | v < 10}` and `x: u8` with `PRE: x < 10` are the same node.
- **Numbering.** Each distinct type and constant appears once, numbered in order of first reference
  while walking SIGNATURE (parameters, results, θ), LEARNABLE (lo, hi), each op in canonical order
  (its result types, then its constant operands), **the node's outputs**, then CONTRACT (clauses in
  order, terms in pre-order). A type's nested types are numbered before it, so every TYPES entry
  refers only to earlier ones.
- **CONSTS.** Bytes as docs/13: integers little-endian, Bool 0 or 1, floats bit-exact with NaN only
  as the canonical NaN of ADR 0002, arrays and tensors row-major. An Int constant (formulas only)
  holds its shortest two's-complement bytes, little-endian, at least one byte; canonical MVP
  formulas hold only non-negative Ints, because a minus sign is a NEG term.
- **OPERANDS (amended).** src_kind 0 input (parameters, then θ), 1 op result (src_slot = result
  index), 2 constant, 3 callee (CALLS index), 4 invariant (index of the INVARIANT clause). src_slot is
  zero for every other kind. Ops take consecutive ranges from the start, and **the node's outputs
  follow them, one operand per SIGNATURE result, in result order**: docs/02 had no place for what
  RETURN returns. An op may refer only to an earlier op, which also makes every node acyclic.
- **OPS.** attr is 0. The mode byte is docs/14's: checked 0, wrap 1, sat 2; s 1, u 2, f 3;
  ASSERT_GUARD 0 and `.branch` 1; 0 for every other op, CAST included.
- **LEARNABLE.** param is the SIGNATURE index of the θ entry (parameters + results + j), type its
  float type, lo and hi CONSTS indices of that type, loss a CALLS index or 0xFFFFFFFF.

### CONTRACT

- Clause table `{kind u8, reserved u8, reserved u16, root u32}`, kinds PRE 1, POST 2, COST 3,
  BUDGET 4, DECREASES 5, INVARIANT 6, in that order; one clause per kind that has conjuncts, then the
  INVARIANT clauses that a live FOLD_BOUNDED uses, without duplicates, sorted by their hash. An
  INVARIANT no fold uses is dropped (VG-W0002).
- The node array follows the table: `{tag u16, argc u16, payload u32}`, each clause's tree in
  pre-order, the trees consecutive; root is the index of the clause's first node, which is an AND
  whose children are the clause's conjuncts after flattening its top-level `&&` (ADR 0007). Inside a
  conjunct, `&&` and `||` are binary and nest as written.
- Tags and payloads:

| Tag | Term | argc | payload |
| --- | --- | --- | --- |
| 0x01 | parameter or θ | 0 | input position |
| 0x02 | `result`, `result.j` of a node with several results | 0 | result index |
| 0x03 | port (INVARIANT only) | 0 | op index << 8 \| result slot |
| 0x04 | bound variable | 0 | de Bruijn index (0 innermost; an INVARIANT binds its step count, then its accumulator) |
| 0x05 | constant | 0 | CONSTS index |
| 0x06 | resource | 0 | heap_bytes 0, arena_bytes 1, stack_bytes 2, ops 3, latency_p99_us 4 |
| 0x10, 0x11 | `!`, unary `-` | 1 | 0 |
| 0x20 … 0x2D | `&&` `\|\|` `==>` `==` `!=` `<` `<=` `>` `>=` `+` `-` `*` `/` `%` | 2 (AND at a root: the conjunct count) | 0 |
| 0x30 | indexing `x[i, …]` | 1 + indices | 0 |
| 0x31 | projection `.j` | 1 | j |
| 0x32 | `forall` | 2n + 1: lo, hi of each range, then the body | n |
| 0x40 | built-in | its arity | len 1, is_finite 2, is_nan 3, abs 4, min 5, max 6, as_bytes 7, same 8, is_ok 9, ok 10, err 11 |
| 0x41, 0x42, 0x43 | `spec:s(…)`, `fold:f(…)`, a kernel applied in a formula (ADR 0008) | arguments | CALLS index |

- An untyped number takes the type of the term it meets (docs/03): a decimal is stored as the float
  it rounds to, so `x <= 0.99` and `x <= 0.99:f32` are the same contract; an integer number is an
  Int constant.

### Hashes (amended)

docs/02 listed the sections each hash covers. Hashing those bytes would make body_hash depend on
constants and callees that only the contract uses, which breaks the law of docs/13 that two bodies
share a body_hash exactly when they unravel to the same terms; the test
`hashes_follow_unraveling` failed that way. So every hash is computed from structure, never from a
table position. All use BLAKE3 derive-key mode with the contexts of docs/02; a leading byte keeps
the different preimages of one context apart.

- **Operand hash** (context op): input `0x00, position u32, type`; op result `0x01, op hash,
  slot u8`; constant `0x02, type, len u32, bytes`; callee `0x03, node hash`; invariant `0x04,
  invariant hash`. "type" is the type's structural bytes: `tag u8, argc u8`, then each argument,
  a nested type's structural bytes or a u32 immediate.
- **Op hash** (context op): `0x05, opcode u16, mode u8, attr u32, result_count u8, each result type,
  operand_count u32, each operand hash`, with the operands of commutative ops sorted by hash first
  (step 4 of docs/02; ADD, MUL, AND, OR, XOR, EQ, NE, BAND, BOR, FADD, FMUL, FMIN, FMAX, and the first
  two operands of FMA).
- **Term bytes**: each node's `tag u16, argc u16`, then its payload, except that a port is its op
  result's operand hash, a constant its `type, len u32, bytes`, and a callee its hash; children
  follow in pre-order. **Invariant hash** (context contract): `0x49, conjunct count u32, each
  conjunct's term bytes`.
- **body_hash** (context body): `0x42, output count u32, each output's operand hash, θ count u32,
  then per θ: type, lo and hi as constants, 0 or 1 and the loss hash`.
- **contract_hash** (context contract): `0x43`, then for each clause in table order: `kind u8,
  conjunct count u32, each conjunct's term bytes`.
- **node_hash** (context node, or spec for a SPEC node, whose header then holds the spec_hash):
  `format_version u16, effect_mask u32, parameter count u32, each parameter type, result count u32,
  each result type, θ count u32, each θ type, body_hash, contract_hash`.
- The scc hash waits for REC_CALL (alpha): the MVP rejects recursion (VG-S0007), so no node has one.

### Opcodes

The numbers of docs/14 are confirmed as they stand, with the non-MVP names reserved at the numbers
docs/02 and docs/14 give them: 0x0001 CONST (rejected), 0x0002 IDENTITY (text only, never in .vgir),
0x0003–0x000C structure, 0x0010–0x0015 integer arithmetic, 0x0020–0x0022 tensors [ALPHA],
0x0030–0x0037 bits, 0x0040–0x004B floats, 0x0050–0x0058 compare and logic, 0x0060–0x0064
collections, 0x0100–0x0107 combinators (0x0102–0x0104 and 0x0106 [ALPHA]), 0x0200 ARENA_ALLOC
[ALPHA], 0x0300 CALL_HASH, 0x0400–0x0402 effects and 0x0403–0x040A [later], 0x0500 SAMPLE [R-07].
The table lives in `crates/vg-core/src/opcode.rs`.

### Around the bytes

- `intent`, `export` and `inline` never reach a hash; with `vg asm --keep-debug` they are in DEBUG
  with the rest of the source text.
- A `@MESH_QUERY` label reaches CALLS only through its vg.lock resolution: its `node` for calls,
  its `spec` for `spec:` terms. An unresolved query is the new code **VG-S0009** (appended to
  docs/15).
- The decoder accepts only canonical bytes: it rebuilds the node, canonicalizes it and requires that
  encoding gives back exactly the input, DEBUG included, so every hash, order and padding rule above
  is enforced by one comparison (VG-S0204), after the specific checks of VG-S0201 to VG-S0203.
- Resource limits keep every pass off the stack's edge: the parser enters at most 64 nested levels
  (parentheses, unary operators, calls, types, array literals) and builds formula trees at most 128
  deep; types nest at most 64 deep; a decoded formula is at most 256 deep. They reject only text
  that tools/vgsym.lark accepts in principle, which no corpus file comes near.

## Golden files

`tests/golden/<stem>.vgs` assembles to exactly `tests/golden/<stem>.<label>.vgir`, checked by the
test `golden_files` (`UPDATE_GOLDEN=1` rewrites them, which this ADR forbids after G1 without a
format_version bump): `minimal.inc`, the soft-knee draft, and `sections.*`, which uses every
section, every operand kind and every MVP clause kind.

`minimal.inc.vgir`, from `NODE @inc { SIGNATURE: (x: u8) -> (u8); … POST: result == x + 1; …
%0 = ADD.wrap(x, 1:u8); RETURN(%0); }`, is 432 bytes:

```text
0x000  56 47 49 52 | 02 00 | 00 00 | 00 00 00 00 | 07 00 00 00   magic, version 2, flags 0, PURE, 7 sections
0x010  43 c4 35 41 … a7 0f 33 e3                                node_hash
0x030  35 bb 85 f8 … 7d 5d 0c 34                                body_hash
0x050  79 bf 84 ff … 82 5c a5 40                                contract_hash
0x070  00 × 16                                                  reserved
0x080  01 00 00 00 02 00 00 00 f0 00 00 00 0c 00 00 00          TYPES: 2 entries at 0x0f0, 12 bytes
0x090  02 00 00 00 02 00 00 00 00 01 00 00 10 00 00 00          SIGNATURE: 2 at 0x100, 16
0x0a0  03 00 00 00 02 00 00 00 10 01 00 00 18 00 00 00          CONSTS: 2 at 0x110, 24
0x0b0  04 00 00 00 01 00 00 00 30 01 00 00 10 00 00 00          OPS: 1 at 0x130, 16
0x0c0  05 00 00 00 03 00 00 00 40 01 00 00 18 00 00 00          OPERANDS: 3 at 0x140, 24
0x0d0  06 00 00 00 01 00 00 00 60 01 00 00 04 00 00 00          RESULTS: 1 at 0x160, 4
0x0e0  08 00 00 00 01 00 00 00 70 01 00 00 38 00 00 00          CONTRACT: 1 clause at 0x170, 56
0x0f0  02 01 00 00 08 00 00 00 | 06 00 00 00 | 00 × 4           type 0 u8 (UInt, 8); type 1 Int; padding
0x100  00 00 00 00 00 00 00 00 | 00 00 00 00 01 00 00 00         x: type 0, role 0; result: type 0, role 1
0x110  00 00 00 00 01 00 00 00 01 00 00 00                      const 0: u8, 1 byte, 0x01, padded
0x11c  01 00 00 00 01 00 00 00 01 00 00 00                      const 1: Int, 1 byte, 0x01 (from POST)
0x130  10 00 | 01 | 01 | 00 00 00 00 | 02 00 00 00 | 00 00 00 00   ADD, wrap, 1 result, operands 0..2, attr 0
0x140  00 00 00 00 00 00 00 00 | 02 00 00 00 00 00 00 00         input 0 (x); const 0, in hash order
0x150  01 00 00 00 00 00 00 00                                  output: op 0, slot 0
0x160  00 00 00 00                                              result type of op 0: type 0
0x170  02 00 00 00 00 00 00 00                                  clause POST, root node 0
0x178  20 00 01 00 00 00 00 00 | 23 00 02 00 00 00 00 00         AND of 1 conjunct; ==
0x188  02 00 00 00 00 00 00 00 | 29 00 02 00 00 00 00 00         result 0; +
0x198  01 00 00 00 00 00 00 00 | 05 00 00 00 01 00 00 00         x (input 0); const 1 (Int 1)
0x1a8  00 × 8                                                   padding to 0x1b0
```

## Consequences

The bytes, numbering and hashes above freeze with G1. Equal programs give equal bytes on every
machine, and every amendment makes a law of docs/13 hold that the proposal would have broken: the
outputs have a place, sizes do not depend on constants, and body_hash and contract_hash depend only
on what they name. Agents and tools that need a node's meaning read it through vg-core's decoder,
which refuses anything non-canonical, so a hash is never attached to bytes that mean something else.

Open for the owner (PROGRESS.md): VG-Sym has no literal for NaN or ±∞, so a node holding such a
constant (as mutation testing in P3.6 may build) has no disassembly; a grammar addition such as
`inf:f32` and `nan:f32` would close that before the format freezes.
