# PROGRESS

Agents append; nobody rewrites history. Add lines, never edit or delete one, so git's union merge (.gitattributes) can combine parallel branches. One entry per change of state, under its section:
`YYYY-MM-DD · W-ID · what · evidence (tests, numbers, report paths)`

## In progress

2026-10-30 · W1 · P1 Core IR, all of P1.1-P1.11 in one pass, ahead of the Nov 6 plan; Gemini 3.8 Flash writes the alpha and distinct corpus families in parallel (Gemini 4 Argon refused: credits exhausted, HTTP 429) · plan: vg-core data model, lexer, parser and printer checked against tools/vgsym.lark; lowering with the validator rules of docs/15 (S00xx, S01xx); canonicalizer steps 1-7 and BLAKE3 hashes; .vgir codec and golden files with ADR 0001 completed; disasm; vg-store with SQLite index and vg.lock; vg asm, disasm, hash; alpha and distinct families with seeded random equivalence moves; unraveling law; gen feature; fuzz targets; scripts/gate-g1.sh written without the G0 review's bugs B1-B7
2026-10-29 · W0 · Spec deepening requested by the owner, with Gemini in parallel: normative semantics, opcode reference, agent interface, conformance and gates; ADRs for every spec change; literature and mathematics survey folded into the research tracks · plan: write docs/13-16, align docs 00-12, ROADMAP, README and the skill, check every citation against its source, run tools/check_examples.py and scripts/gate-g0.sh

## Done

2026-10-29 · W0 · docs/13-semantics.md, docs/14-opcode-reference.md, docs/15-agent-interface.md, docs/16-conformance-and-gates.md written; docs 00, 02-09, 11, 12, ROADMAP task references, README map and the synthesis skill aligned with them · tools/check_examples.py now parses every VG-Sym block in docs/ and the skill: 20 of 20 cases ok (lark 1.3.1), including new lexical cases for port annotations and `fold:`; scripts/gate-g0.sh run directly: fmt, clippy, tests exit 0, criterion 3 met with 14 tracks, gate still FAILED only on CI (not a git repository); `make` itself is blocked on this host until the Xcode license is accepted
2026-10-29 · W0 · ADRs 0002-0010 proposed and applied to the draft: one NaN written canonically; one arena per exported call; Result ops and port type annotations; define-before-use; counterexample replay and the contract_gap verdict; canonical contracts and obligation IDs; `fold:` and SPEC unfolding; checked entry points; well-defined contracts · docs/adr/0002-0010; grammar change in docs/03 and tools/vgsym.lark
2026-10-29 · W8 · docs/research/SURVEY-2026-10.md and new track R-14 (intent validation); R-01, R-02, R-04, R-08, R-09, R-10 and R-12 updated with verified sources · every arXiv id checked against the arXiv API; LLVM LangRef and the WebAssembly spec read for ADR 0002. Gemini 4 Argon jobs ended on exhausted credits with no output; Gemini 3.8 Flash delivered a 53-finding spec audit (2 rejected with reasons in the survey, the rest merged or already covered), a cross-target memo and a 25-paper sweep (one wrong author list corrected; unread entries left out)

2026-10-29 · W0 · Owner decision recorded: the repository stays private during the MVP and goes public at M1, after the license is chosen · docs/12 open question ticked; ROADMAP task M1.4 added
2026-10-29 · W0 · Private repository github.com/OZBM/verigraph created and main pushed; CI green on linux-x86_64 and macos-arm64 for commit 29d25ea (run 40837848645); `make gate-g0` exits 0, every G0 criterion holds, awaiting the owner's approval · reports/gate-g0.md
2026-10-30 · W1 · P1 Core IR built ahead of plan (planned Oct 28 → Nov 6): vg-core data model, lexer, parser, printer, lowering and validator, canonicalizer with Merkle hashes, .vgir codec, disasm, generator; vg-store; vg asm, disasm, hash; fuzz targets; scripts/gate-g1.sh. P1.1-P1.4, P1.6-P1.11 ticked; P1.5 done except the scc hash (question below) · commits a9ba3a2, d52e676, 04174aa, 4a7f7c9; cargo test --workspace: 26 tests green; laws on 206 files and 386 nodes (parse(print(t)) = t, canonicalization idempotent, decode(encode(n)) = n, asm(disasm(b)) = b, disasm(asm(s)) fixed); 56 alpha families (168 files) plus 560 seeded random variants hash identically (seed 0x5eed20261030); 16 distinct families, 18 pairs, all differ; 400 generated bodies: 400 variants equal, 163 near-misses differ in terms and hash, 559 equal in both (test hashes_follow_unraveling); Lark parity on 245 files (226 accepted, 19 rejected) with equal token texts; 7 golden .vgir files; 60 s smoke fuzz: 0.8 M parse, 3.4 M codec, 30 k canon runs, no crash
2026-10-30 · W1 · ADR 0001 completed with five amendments to docs/02, applied to docs/02 and docs/15 (new code VG-S0009): u32 immediates in TYPES, node outputs after the ops' operands, refinements stored as PRE/POST, structural body/contract/node hashes, canonical section layout · test hashes_follow_unraveling failed with section-based hashes, because contract-only constants changed body_hash; it passes with structural hashes
2026-10-30 · W1 · Gemini: Gemini 4 Argon refused every job ("credits balance is too low", HTTP 429); Gemini 3.8 Flash, sandboxed with no tools, drafted the 56 alpha and 16 distinct families in 5 parallel jobs (3 relaunched after they tried to run a command and returned nothing) · corrections by W1: non-numeric ports in 34 files renumbered, an uppercase INVARIANT label, a port in a POST (VG-C0105), two added `PRE: true` clauses that are not an equivalence move; every file then parses with Lark and vg-core and hashes as its family says. A Flash adversarial review of canon.rs, codec.rs and lower.rs was launched; its result is logged below when it arrives
2026-10-30 · W1 · Fuzzing found one crash, in test code: the class B generator overflowed i128 on the interval of a u64 product · fixed with saturating interval arithmetic; regression test class_b_intervals_do_not_overflow; the input is kept in fuzz/regressions/canon and seeds every run
2026-10-30 · W1 · Gemini 3.8 Flash adversarial review of canon.rs, codec.rs and lower.rs, sandboxed with the source inlined: 12 test pairs before its output limit cut it off (10 to 25 were asked), 7 claimed equal and 5 claimed different · all 12 behave as claimed under vg hash (2 needed their non-numeric ports renumbered first); no soundness or completeness bug found. The pairs were not added to the corpus, so the gate-g1 run of commit bf3935d stays current

## Blocked

## Questions for the owner

_Format: question, options, recommendation._

2026-10-29 · W0 · Approve ADRs 0002-0010 before G1 freezes the format? Options: approve all; approve with changes; reject some. Recommendation: approve all; each closes a gap that would have let two workstreams build incompatible things or let a wrong program verify (docs/12).
2026-10-29 · W0 · Linux x86_64 CPU baseline? Options: x86-64-v3 (AVX2, FMA, BMI2), or x86-64-v2, where FMA becomes a slow libcall. Recommendation: x86-64-v3, checked at startup (docs/06).
2026-10-29 · W0 · Gate wording: add docs/16's distinctness families to G1, and count a G5 task as solved only when the node verifies against the task's reference contract? Options: both, one, neither. Recommendation: both; without them a hash-merging bug passes G1 and a `POST: true` passes G5.
2026-10-29 · W0 · Approve G0 (reports/gate-g0.md)? Options: approve now; run the Gate review prompt (PROMPTS.md) in a fresh session first. Recommendation: the review first, as README step 5 says.
2026-10-29 · W0 · macOS sandbox mechanism? Options: `sandbox_init` with a generated profile (deprecated by Apple, works with argv paths), or App Sandbox entitlements (supported, fixed paths only). Recommendation: let P4.6 test both on the pinned macOS; docs/04 limits macOS roots to fixed paths until then.
2026-10-30 · W1 · Approve ADR 0001, which freezes the .vgir format with G1? Options: approve; approve with changes; reject an amendment. Recommendation: approve; each amendment makes a law of docs/13 hold that the docs/02 proposal broke (reports/gate-g1.md, docs/adr/0001).
2026-10-30 · W1 · P1.5 lists the scc hash, but the MVP rejects recursion (VG-S0007), so no node can have one and nothing can test it. Options: move the scc hash to P9 with REC_CALL; keep it in P1 as untested code. Recommendation: move it to P9; ADR 0001 already says so.
2026-10-30 · W1 · VG-Sym has no literal for NaN or ±∞, so a node holding one (mutation testing in P3.6 builds such constants) cannot be disassembled, although .vgir stores it. Options: add `nan:f32`, `inf:f32` and `-inf:f32` literals by ADR in P3; leave such nodes without a disassembly. Recommendation: the ADR in P3, before P3.6; the .vgir format needs no change.
2026-10-30 · W1 · The parser bounds nesting (64 levels) and formula depth (128), so it rejects some deeply nested text that tools/vgsym.lark accepts; without the bounds a fuzz input exhausts the stack. Options: keep the bounds as a documented resource limit (ADR 0001); raise them and run the parser on a large stack. Recommendation: keep them; no corpus file comes within a factor of 5.

## Token log

W0 appends one row per phase when its gate closes.

| Phase | Agent tokens (approx.) | Notes |
| --- | --- | --- |
