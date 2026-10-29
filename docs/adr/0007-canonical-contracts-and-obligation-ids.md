# 0007 · Canonical contracts and obligation IDs

Status: proposed on 2026-10-29 for the owner's approval; applied to docs 02, 03, 05 and 15. It changes the CONTRACT encoding, so it must be settled before G1.

## Context

The spec hashed "the canonical CONTRACT section" without defining the formula encoding: whether parameter names, bound-variable names or clause order affect the hash, how `result.j` and ports are represented, and how one obligation of a clause is named. The example report already used `POST.0` without a rule behind it.

## Decision

- **Names never reach a hash.** Parameters become input positions, `result.j` becomes result index j, ports in INVARIANT clauses become canonical op indices (op hashes while hashing), and bound variables of `forall` and of INVARIANT become de Bruijn indices. Labels become CALLS indices.
- **Clause order is fixed by kind:** PRE, POST, COST, BUDGET, DECREASES, then INVARIANT clauses sorted by hash. Several clauses of one kind join into one conjunction in source order. Refined parameter types join PRE after it, refined result types join POST, in signature order.
- **Conjunct order is kept.** Formulas are hashed as written, after the renaming above; swapping two conjuncts changes contract_hash, and R-02's normal forms may later identify such variants.
- **Obligation IDs** number the conjuncts of the joined clause from 0 after flattening its top-level `&&` (docs/15).
- **Encoding:** CONTRACT holds the clause table `{kind u8, reserved u8, reserved u16, root u32}` and a prefix-encoded node array `{tag u16, argc u16, payload u32}`, where payload is an input position, a result index, a canonical op index, a de Bruijn index, a CONSTS index, a CALLS index or a built-in id, as the tag says. Types are not stored: the formula checker derives them again. A decimal number is stored as the float it rounds to, typed by the term it meets; an integer number as an Int constant (docs/02). ADR 0001 pins the tag values with golden files.
- **Liveness:** an op referenced by the INVARIANT of a live FOLD_BOUNDED is live, so step 5 never removes it.

## Consequences

Renaming anything never changes a hash, so agents can rename freely; conjunct order stays meaningful, so obligation IDs follow what the agent wrote. G1's alpha families include renaming of parameters and bound variables, and its distinctness families include conjunct swaps.
