# 09 · VG-Mesh

VG-Mesh stores verified nodes and lets agents find them by contract instead of by package name.

## Identity by specification

Program equivalence is undecidable in general, so two independent SHA-256 implementations never share a structural hash. VG-Mesh groups them by *spec class* instead. A canonical SPEC node (for SHA-256, the FIPS 180-4 algorithm written plainly) has a `spec_hash`, and every implementation shown to refine it joins that class. Agents ask for the spec, and Mesh returns the best implementation for their target whose evidence their policy accepts. Equality saturation with egg can also make small kernels converge structurally (R-02).

Membership records how it was shown, and a query states the minimum it accepts:

| Evidence | Meaning |
| --- | --- |
| `proved` | The implementation's POST `result == spec:s(…)` is proven for every input, typically with a fold invariant (ADR 0008) |
| `bounded(n)` | Refinement is proven for every input of size at most n, by solving at fixed sizes |
| `tested(m)` | Only m differential test cases agree; never enough for a caller's proof to rely on |

A caller's proof that uses a Mesh binding's POST is only as strong as that binding's evidence, so `vg_verify` reports, for each node, the weakest evidence among the bindings its proof used.

## Objects

| Object | Keyed by | Holds |
| --- | --- | --- |
| Node | node_hash | The canonical .vgir bytes |
| Spec | spec_hash | A reference SPEC node and its interface |
| Proof record | Obligation hash | Verdict, solver, version, and a certificate when one exists |
| Attestation | Signer and statement | "Node N refines spec S, checked by checker C", signed with Ed25519 |
| Benchmark | node_hash and target | Cycles per byte, p50 and p99 latency |
| Provenance | node_hash | Lift source, commit, compiler, flags |
| Name | Human name | A signed, movable pointer to a hash, like a git ref |

## Query pipeline

1. **Exact spec match.** A query that names a spec class gets its members back in microseconds.
2. **Structural filter.** An index on type skeleton and effect mask keeps candidates with `(node & ~allowed) == 0`.
3. **Semantic rank** by cosine similarity of intent embeddings, which live in the index, not in nodes. Brute force is fine below a few hundred thousand vectors; an HNSW index comes later.
4. **Behavioral check.** For the top candidates, the solver proves PRE_query ⇒ PRE_candidate and PRE_query ∧ POST_candidate ⇒ POST_query: behavioral subtyping (Liskov and Wing, 1994), where the postcondition may rely on the query's precondition. Results are cached; uncached checks run asynchronously, outside the 5 ms target that steps 1–3 meet.
5. **Rank** by verified cost, then by measured benchmarks on the caller's target.

```json
{
  "input_types": ["Slice<u8>", "u32"],
  "output_types": ["u32"],
  "max_effects": [],
  "pre": "true",
  "post": "result == spec:crc32_ieee(arg0, arg1)",
  "max_cost": "ops <= 12 * len(arg0) + 64",
  "intent": "IEEE 802.3 CRC-32 over a byte slice, with seed"
}
```

## Ingestion gate, for every submission

1. Recompute the canonical hash; reject on any mismatch.
2. Validate the structure and re-run vg-types.
3. Re-verify every obligation, or check its certificate once R-05 lands.
4. Check effect transitivity against every callee.
5. Verify the submitter's signature and append the entry to a transparency log, an append-only Merkle log as in Certificate Transparency. The MVP uses the C2SP formats rather than inventing one: statements and checkpoints are signed notes with Ed25519 keys (c2sp.org/signed-note, c2sp.org/tlog-checkpoint), and the log is served as static tiles (c2sp.org/tlog-tiles), so standard verifiers and, later, witnesses (c2sp.org/tlog-witness, R-12) can audit it.
6. Deduplicate by node_hash, and add the node to its spec classes.

## Trust, stated plainly

Proofs guarantee only what contracts say. A malicious node with a weak contract can still return wrong answers inside its effects, and side channels are out of scope. Effects confine what a node can touch; spec strength (R-04's mutation score), attestations and the log make the rest auditable. Federation, spam and trust policies are R-12. The MVP runs one local registry with an HTTP API.
