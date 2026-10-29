# R-12 · VG-Mesh at global scale

**Question.** How does a public, federated Mesh stay trustworthy, fast and spam-resistant?
**Why it is research.** Proofs cover only what contracts say; open registries attract spam, equivocation and weak specs.
**Fallback that ships.** One signed registry over HTTP.  **Time-box:** 4 weeks.  **Starts after:** M2.

## Proposed solution

Bytes replicate freely, because hashes verify them. Trust moves to signed attestations recorded in a transparency log, and each consumer chooses which attesters to believe. Spam is priced by making submitters pay their own verification cost, and spec strength is published so weak contracts are visible.

## Go if

Every attack in the suite (tampered bytes, forged attestations, log equivocation, flooding) is detected, and indexed queries stay under 5 ms at p99 on 1M nodes.

## Starting points

- Certificate Transparency; Sigstore and Rekor
- The C2SP specifications the MVP log already uses: [signed-note](https://c2sp.org/signed-note), [tlog-checkpoint](https://c2sp.org/tlog-checkpoint), [tlog-tiles](https://c2sp.org/tlog-tiles), and for federation [tlog-cosignature](https://c2sp.org/tlog-cosignature) and [tlog-witness](https://c2sp.org/tlog-witness)
- IPFS and content-addressed networks; SLSA and in-toto

## Prompt

```text
Run research track R-12 (VG-Mesh at global scale) following docs/research/PROTOCOL.md.
Question: how does a public, federated Mesh stay trustworthy, fast and spam-resistant?
Build behind feature r-12: (1) a threat model document; (2) content-addressed replication across 3 federated nodes; (3) signed attestations and a Merkle transparency log with inclusion and consistency proofs; (4) per-consumer trust policies; (5) submitter rate limits and verification-cost accounting; (6) published spec-strength scores from R-04.
Tests: tampered bytes, forged attestations, log equivocation, flooding.
Measure: detection rate per attack, query p99 on 1M nodes for indexed paths, replication lag.
Read first: Certificate Transparency, Sigstore and Rekor, IPFS and content-addressed networks, software supply-chain frameworks such as SLSA and in-toto.
Go if: every attack in the suite is detected and indexed queries stay under 5 ms at p99. Otherwise run one signed registry over HTTP.
```
