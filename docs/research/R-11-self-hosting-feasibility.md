# R-11 · Self-hosting feasibility

**Question.** What is the smallest language extension that lets the VeriGraph compiler be written in VeriGraph, at acceptable speed?
**Why it is research.** A compiler needs recursion, maps and I/O that the MVP language lacks, and functional data structures can be slow.
**Fallback that ships.** Rust stays the production compiler; only the canonicalizer and checker self-host.  **Time-box:** 4 weeks.  **Starts after:** M2.

## Proposed solution

Add the fewest features a compiler needs: byte buffers, persistent maps in arenas, SCC recursion, and file and process effects. Port the canonicalizer first as a pilot, because hash equality with vg-core is a perfect oracle. Get in-place updates from uniqueness, as Lean 4 does, so a functional compiler stays fast.

## Go if

100% hash agreement with vg-core on the whole corpus and no more than 3× slower than Rust; then freeze the stage 1 feature list.

## Starting points

- Perceus and functional-but-in-place updates (Lean 4, Koka)
- [Unison's codebase format](https://unison-lang.org/docs/language-reference/hashes)
- Diverse double-compiling (David A. Wheeler)

## Prompt

```text
Run research track R-11 (self-hosting feasibility) following docs/research/PROTOCOL.md.
Question: what is the smallest language extension that lets the VeriGraph compiler be written in VeriGraph, at acceptable speed?
Build behind feature r-11: (1) byte buffers, persistent maps (a HAMT in arenas), SCC recursion with DECREASES, FILE_READ, FILE_WRITE and SPAWN support; (2) the canonicalizer and hasher rewritten in VG-Sym as a pilot; (3) in-place updates for uniquely owned values.
Oracle: the pilot must produce byte-identical hashes to vg-core on the whole corpus.
Measure: hash agreement, speed versus the Rust canonicalizer, lines of VG-Sym versus Rust, verification time of the pilot's own contracts.
Read first: Perceus and functional-but-in-place updates (Lean 4, Koka), Unison's codebase format, compiler bootstrapping and diverse double-compiling (Wheeler).
Go if: 100% hash agreement and no more than 3x slower than Rust; then freeze the stage 1 feature list. Otherwise Rust stays the production compiler and only the canonicalizer and checker self-host.
```
