# 02 · VG-IR format

A VG-IR node is one function, stored as a canonical binary object with 16-byte-aligned sections. Its identity is a BLAKE3 hash over its canonical body, contract, signature and effects. The format and canonicalization freeze at G1; byte-level encodings are fixed by docs/adr/0001-format-encodings.md, with golden files.

## Vocabulary

A *node* is the content-addressed unit: one function. Inside its body, *ops* are combinator instances, and each op reads *operands*: inputs, constants or other ops' results. The source spec used "node" for both levels; this spec does not.

## Hashes

| Hash | Covers | BLAKE3 derive-key context |
| --- | --- | --- |
| op hash | opcode, mode, attributes, result types, operand hashes in port order | `VeriGraph 2026 op v2` |
| body_hash | the canonical op graph: the Merkle hashes of the outputs and the LEARNABLE box (ADR 0001) | `VeriGraph 2026 body v2` |
| contract_hash | the canonical contract: each clause's formulas, with constants, callees and ports by hash (ADR 0001) | `VeriGraph 2026 contract v2` |
| node_hash | format_version, effect_mask, the signature's types, body_hash, contract_hash | `VeriGraph 2026 node v2` |
| scc hash | a group of mutually recursive nodes | `VeriGraph 2026 scc v2` |
| spec_hash | a SPEC node used as a reference specification | `VeriGraph 2026 spec v2` |
| obligation hash | one SMT query, as printed SMT-LIB2 | `VeriGraph 2026 obligation v2` |

Callers reference `node_hash`. Different contexts guarantee that hashes of different kinds never collide.

## Binary layout

```text
Fixed header: 128 bytes, little-endian
  off size field
    0  4   magic           "VGIR"
    4  2   format_version  0x0002
    6  2   flags           bit0 has_debug, bit1 scc_member, bit2 spec_node
    8  4   effect_mask     u32, see docs/04-types-effects.md
   12  4   section_count   u32
   16 32   node_hash
   48 32   body_hash
   80 32   contract_hash
  112 16   reserved        must be zero
Section directory: 16 bytes per entry
  kind u16 | flags u16 | count u32 | offset u32 | length u32
Sections: ascending kind order, each on a 16-byte boundary, zero-padded
  0x01 TYPES      canonical type descriptors
  0x02 SIGNATURE  inputs and outputs {type u32, role u16, flags u16}
  0x03 CONSTS     typed constants, bit-exact
  0x04 OPS        {opcode u16, mode u8, result_count u8,
                   operand_start u32, operand_count u32, attr u32}
  0x05 OPERANDS   {src_kind u8, src_slot u8, reserved u16, src_index u32}
  0x06 RESULTS    one type index (u32) per op result
  0x07 CALLS      32-byte hashes of the nodes referenced by label
  0x08 CONTRACT   clause table and prefix-encoded formulas (ADR 0007)
  0x09 LEARNABLE  {param u32, type u32, lo u32, hi u32, loss u32}
  0x7F DEBUG      names, spans, intent text, guard map; never hashed
```

Encodings (ADR 0001 gives every byte, its amendments to this list, and the golden files that pin them):

- **TYPES:** variable-length descriptors `{tag u8, argc u8, reserved u16, args u32[argc]}`. Tags: 0x01 Bool; 0x02 UInt(bits); 0x03 SInt(bits); 0x04 Float(bits); 0x05 BFloat16; 0x10 Array(elem, len); 0x11 Slice(elem); 0x12 Tensor(elem, dims…); 0x13 Tuple(members…); 0x14 Result(ok, err); 0x15 Arena(bytes) [ALPHA]; 0x16 Eff; 0x17 Handle(kind); 0x18 Refinement(base, formula offset); 0x06 Int, the unbounded integers of formulas, used only by CONTRACT constants. Type arguments are indices into TYPES; bit widths, lengths, dimensions, arena sizes and handle kinds are u32 immediates, and nested types are numbered first.
- **CONSTS:** `{type u32, len u32, bytes[len]}` padded to 4 bytes; floats keep their exact bits, except that NaN is always stored as the canonical NaN of docs/13 (ADR 0002); an Int constant stores its shortest two's-complement bytes, little-endian. Array and tensor constants store their elements in row-major order.
- **Numbering of TYPES and CONSTS:** each distinct type and constant appears once, numbered in order of first reference while serializing SIGNATURE, then LEARNABLE, then each op in canonical order (its result types, then its constant operands), then the node's outputs, then CONTRACT.
- **OPS.attr** is 0 for every MVP op; docs/14 keeps every op's information in its opcode, mode, operands and result types.
- **OPERANDS.src_kind:** 0 input (src_index = input position), 1 op result (src_index = op index, src_slot = result index), 2 constant (src_index = CONSTS index), 3 callee (src_index = CALLS index: a combinator's kernel, CALL_HASH's callee, ASSERT_GUARD's branches), 4 invariant (src_index = the INVARIANT clause's index in CONTRACT, for FOLD_BOUNDED's `invariant=`). Each VG-Sym argument becomes one operand, in argument order; named arguments come last, in the order the opcode table fixes. The node's outputs follow the ops' operands, one per result.
- **CALLS and INVARIANT clauses:** sorted by hash, without duplicates, so declaration order never changes a hash.
- **LEARNABLE:** `param` is the SIGNATURE index of the θ entry, `type` its float type, `lo` and `hi` CONSTS indices of the bounds in that type; `loss` is a CALLS index, or 0xFFFFFFFF when the parameter has no loss.
- **SIGNATURE:** one 8-byte entry per value, `role` 0 for inputs in parameter order, then 1 for outputs in result order, then 2 for θ in LEARNABLE order (the implicit trailing inputs); `flags` is 0 in the MVP, and a refined type is simply a TYPES entry with tag 0x18.
- **Static limits** (VG-T0118): a fixed-size type holds at most 2^32 − 1 elements and 16 MiB; larger data is a Slice. Array lengths and tensor dimensions are u32 immediates in TYPES; LEN still returns u64.
- **Section directory:** `count` is the number of entries (types, constants, ops, operands, results, hashes, clauses or parameters) and `length` the section's bytes before padding; `flags` is 0.
- **SPEC nodes** set header flag bit 2, and their header's node_hash field holds the spec_hash, computed over the same preimage with the `spec` context, so a spec and an implementation can never share an identity.

Rules: unknown section kinds are rejected; every index must be in range; reserved bytes must be zero; proofs and intent embeddings are never stored in a node (proofs live in the store keyed by obligation hash; embeddings live in the Mesh index keyed by node hash and embedding model).

## Opcodes

| Opcodes | Family | Ops | Phase |
| --- | --- | --- | --- |
| 0x0001–0x000C | Structure | CONST (reserved), IDENTITY (text only), TUPLE, PROJ, SELECT, CAST, AS_BYTES, OK, ERR, IS_OK, GET_OK, GET_ERR | [MVP] |
| 0x0010–0x001F | Integer math | ADD, SUB, MUL, DIV, MOD, NEG; mode byte = checked, wrap or sat | [MVP] |
| 0x0020–0x002F | Tensors | TENSOR_DOT, TENSOR_CONV, TENSOR_RESHAPE | [ALPHA] |
| 0x0030–0x003F | Bits | AND, OR, XOR, NOT, SHL, SHR, ROTL, ROTR | [MVP] |
| 0x0040–0x004F | Float | FADD, FSUB, FMUL, FDIV, FMA, FNEG, FABS, FMIN, FMAX, FCLAMP, FCOPYSIGN, FSQRT | [MVP] |
| 0x0050–0x005F | Compare, logic | EQ, NE, LT, LE, GT, GE (signed, unsigned, float), BAND, BOR, BNOT | [MVP] |
| 0x0060–0x006F | Arrays | INDEX, UPDATE, LEN, SLICE, CONCAT | [MVP] |
| 0x0100–0x0107 | Combinators | SIMD_MAP, FOLD_BOUNDED, PARTITION_BY, SCAN_PREFIX, MATCH_DISJOINT, ASSERT_GUARD, REC_CALL, GENERATE | [MVP]: SIMD_MAP, FOLD_BOUNDED, ASSERT_GUARD, GENERATE |
| 0x0200 | Memory | ARENA_ALLOC | [ALPHA] (ADR 0003) |
| 0x0300 | Composition | CALL_HASH | [MVP] |
| 0x0400–0x04FF | Effects | FILE_READ, FILE_WRITE, CLOCK_READ, NET_READ, NET_WRITE, GPU_DISPATCH, RANDOM, PARAM_UPDATE, EMIT, HOST_CALL, SPAWN | [MVP]: the first three |
| 0x0500–0x05FF | Probabilistic | SAMPLE and distribution constructors | [R-07] |

The source spec's opcode numbers are kept; new families fill unused ranges. docs/14-opcode-reference.md proposes the exact number, typing, meaning and obligation of every op, and ADR 0001 confirms the numbers with golden files.

Semantics notes for the MVP (docs/13 and docs/14 are normative):

- `checked` arithmetic carries a no-overflow obligation and has no hidden runtime check; `wrap` is modular; `sat` saturates. DIV and MOD also require a non-zero divisor, and signed DIV excludes MIN / −1.
- Float ops follow IEEE 754 with round-to-nearest-even, and floats have one NaN, written as the canonical NaN wherever it becomes bytes (ADR 0002). FMIN and FMAX follow IEEE 754-2019 `minimum` and `maximum`. FCLAMP(x, lo, hi) is NaN-propagating.
- SELECT evaluates both inputs (the graph is strict); make operands safe before a division instead of selecting after it. When no safe operand exists, such as an INDEX into a slice that may be empty, put the step in a kernel and call it through ASSERT_GUARD, which runs only the branch it takes (docs/05). Write `ASSERT_GUARD.branch` when either branch is a normal outcome, and plain ASSERT_GUARD for a safety check.
- LEN returns u64, and INDEX, UPDATE and SLICE take u64 indices. Which collections each op accepts, and what AS_BYTES returns, is in docs/04.

## Canonicalization

1. Validate structure: indices in range, arities match the opcode table, no cycles.
2. Resolve labels to hashes; drop IDENTITY ops and the DEBUG section.
3. Hash bottom-up. An op's hash covers its opcode, mode, attributes, result types and its operands' hashes in port order. An input hashes as its position and type, a constant as its type and exact bits, a callee as its node_hash, an op result as its op's hash and its result index, and an invariant as the hash of its canonical formula (ADR 0007).
4. Sort the operands of the ops docs/14 marks commutative by hash: ADD, MUL, AND, OR, XOR, EQ, NE, BAND, BOR, FADD, FMUL, FMIN, FMAX, and the first two operands of FMA. With one NaN (ADR 0002), float addition and multiplication are commutative bit for bit.
5. Merge ops with equal hashes, and drop ops that no output, Eff chain or live INVARIANT depends on (ADR 0007).
6. Number the ops by a post-order depth-first walk from the outputs, in port order. Unlike "topological order", this order is unique for a given graph.
7. Serialize the sections deterministically; the hashes come from the structure, not from table positions (ADR 0001).

Canonicalization is idempotent: canonicalizing a canonical node changes no byte. G1 tests this on every corpus file.

## Effects have an order

Effectful ops take an `Eff` token and return a new one, so the token chain is the program's order of effects. Two writes never hash alike, and step 5 can never merge them. The Eff token disappears at lowering; it only orders calls.

## Recursion

A node cannot contain its own hash. Mutually recursive nodes form one strongly connected component (SCC): calls inside it become `REC_CALL(i)`; each member is first hashed with intra-SCC references masked; members are sorted by that hash (ties broken by structure, and true ties are the same function); the SCC is hashed as a whole; each member is addressed as (SCC hash, index). Unison hashes cycles the same way. Every REC_CALL needs a DECREASES measure. REC_CALL is [ALPHA]; the MVP language is total through bounded combinators.

## Short hashes and vg.lock

Text shows `#` plus 12 hex digits, unique within a module; binaries always carry all 32 bytes. An 8-digit prefix reaches even odds of a collision at about 77,000 nodes; 12 digits only at about 20 million, and the toolchain lengthens a prefix on any clash, as git does.

`vg asm` writes every resolved label to `vg.lock`:

```toml
[resolved]
"@sha256" = { node = "<64 hex>", spec = "<64 hex>", source = "mesh" }
"@soft_knee" = { node = "<64 hex>", source = "local" }
```
