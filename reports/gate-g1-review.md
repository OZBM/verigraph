# Gate G1 · Independent review

Reviewer: an independent Claude Code session (Claude Opus 5.5), started from the W1 session on 2026-10-30. `reports/gate-g1.md` was not used as evidence. Every claim below comes from the repository, a rerun of `make gate-g1`, or a test I wrote and ran. No code was fixed, nothing was committed or pushed in this checkout, and no CI run was triggered.

Reviewed: commit `fe499c8f8bb66c3a282e3f03804962d945bcae98`, HEAD of `main`. Its gate inputs equal those of `bf3935d`, the commit that `reports/gate-g1.md` names; fe499c8 only adds that report. Criteria: the G1 row of ROADMAP.md (line 68), the G1 row of docs/16 "What each gate script checks" (line 95), the P1 tasks, and AGENTS.md.

## Verdict

| # | G1 criterion | Verdict | Evidence |
| --- | --- | --- | --- |
| 1 | `.vgs` and `.vgir` round-trip byte-exact (docs/16: the laws on every corpus and golden file) | **met on the corpus; false in general** | Section 2.1; B3 |
| 2 | 50 or more alpha families hash identically under random renaming and reordering | **met** | Section 2.2 |
| 2b | Near-misses hash differently (the distinctness families, which the gate already requires while the owner decides) | **not met** | B1, B2 |
| 3 | vg-core's parser agrees with tools/vgsym.lark on every corpus file and lexical case | **met** | Section 2.3 |
| 4 | Fuzzers run an hour without a crash | **met** | Section 2.4 |

`make gate-g1` exits 0 on the real tree, and its rows are true for the inputs it looks at. But the hash is not injective:

- **B1:** two programs with different signature types get the same node, body and contract hash.
- **B2:** a program that returns y and a program that returns x get the same node hash.

Both are reachable from ordinary VG-Sym text that the Lark grammar accepts, and both come from a count written into one byte (`len as u8`). AGENTS.md calls a soundness bug of this kind the worst possible bug. Hash identity is what every later phase trusts: the store, CALL_HASH, proofs keyed by hash, and Mesh.

I ran 23 hash pairs, 8 Rust tests and 8 gate copies. The results:

- 5 new tests fail, each one a finding: B1, B2, B3, B4 and B5.
- 26 pairs and tests behave as docs/13 and ADR 0001 require (23 pairs, 3 tests).
- 4 broken trees reach "met" on every criterion except fuzzing, which a 60 s run cannot meet by design (B6, B7, B8, G-1).

Recommendation for the owner: **do not approve G1 yet.** AGENTS.md freezes the .vgir format and canonicalization when G1 passes. The fix for B1 to B3 either widens a count field in the format or adds a width limit to the language. Either is cheap now and needs an ADR and a format_version bump after the freeze. Once B1 to B3 are fixed and r01 to r03 (appendix A) join the corpus as distinct and round-trip cases, the remaining findings don't block G1. The gate-script bugs B6 to B8 should be fixed before gate-g1.sh becomes the template for G2.

## 1. What I ran

Host: macOS 27.2 (26B118), arm64. Tools: rustc 1.90.0 (1159e78c4 2025-09-14) pinned; for fuzzing, rustc 1.101.0-nightly (92b5b8c87 2026-10-29) and cargo-fuzz 0.13.2; lark 1.3.1 in `target/venv`; `/bin/sh` as the script's shell.

1. `make gate-g1` on the real tree at fe499c8, full length (FUZZ_SECONDS 3600): **exit 0**, from 2026-10-30T17:44:42Z to 18:45:09Z, `wrote reports/gate-g1.md: every criterion holds; awaiting the owner's approval`. The regenerated report differed from the committed one in four lines: the commit name and the three fuzz rows (section 2.4). Afterwards I restored the committed bytes with `git checkout -- reports/gate-g1.md`. SHA-256 43e3f9b9…6efb6 before and after.
2. `vg hash` on 23 pairs of programs (section 3.1), with the `vg` binary built from fe499c8 in a scratch copy.
3. `cargo test -p vg-core --test review_adversarial` in a scratch copy, with the 8 tests of appendix A: **exit 101**, 3 passed, 5 failed (section 3.2).
4. `vg asm` and `vg hash` on the B1, B2 and B3 programs (section 3.2).
5. 8 gate runs on scratch copies, each `FUZZ_SECONDS=60 make gate-g1`, with a stub `gh` first on PATH that refuses every call (section 3.3). With 60 s, row 4 reads "not met" and make exits 2, by design. A check counts as "not caught" when rows 0 to 3 all read "met".

Every scratch copy was a `cp -R` of this checkout outside the repository, with `git remote set-url --push origin file:///dev/null/push-disabled`. Changes the gate must see were committed in the copy only.

## 2. Evidence per criterion

### 2.1 Round-trip: met on the corpus, false in general

- The rerun's `cargo test --workspace --locked` exited 0: 26 tests, 0 failed, 0 ignored, across 27 harnesses.
- The law evidence it wrote, from `target/gate-g1/`:
  - print.txt: 206 files, parse after print is the identity;
  - asm.txt: 205 files and 386 nodes, canonicalization idempotent, decode(encode(n)) = n, asm(disasm(b)) = b, disasm(asm(s)) fixed; examples/audio_limiter.vgs is not assembled because it needs a @MESH_QUERY resolution;
  - golden.txt: 7 golden .vgir files reproduced byte for byte;
  - generated.txt: 600 generated programs and 888 nodes, assembled, canonical and decoded back.
- I checked that the decoder refuses a flipped header hash byte (node, body and contract), 16 trailing bytes, a file truncated by 16 bytes, and a NaN constant with a payload. These are r04, r05 and r08, and all pass.
- **False in general.** A node whose signature has a 256-member tuple, or a tensor with 255 dimensions, parses, lowers and hashes, but `vg asm` cannot store it (B3). The text is valid for the grammar and for vg-core. docs/16 limits the law to corpus files, so the gate's row is true as written. ROADMAP's sentence is not.

### 2.2 Alpha families: met

alpha.txt from the rerun reads: 56 alpha families, 168 hand-written files, 560 random variants (10 per family), seed 0x5eed20261030, all identical. That is at least 50 × 10.

I added five completeness pairs, and all hash alike, as docs/13 says they should:

- t08a: renaming a bound variable that shadows a parameter;
- t12b: `PRE: a; PRE: b && c;` against `PRE: a && b && c;` (ADR 0007);
- t15: a dead op dropped;
- t17: a refined parameter against the same PRE;
- t19: `-0:i8` against `0:i8`.

### 2.2b Near-misses: not met

The rerun's distinct.txt says "16 distinct families, 18 pairs: all different", and unravel.txt says 400 generated bodies with 163 near-misses that differ. These pass. But B1 and B2 are near-miss pairs that hash alike. The generator and the corpus never build a type with 256 members or an op with 257 results, so no test reaches them.

### 2.3 Lark parity: met

`target/venv/bin/python tools/lark_parity.py --check` exited 0. parity.txt reads: 245 files (226 accepted, 19 rejected by Lark), same verdict and token texts in vg-core, with the fixture checked against lark 1.3.1.

### 2.4 Fuzzing: met

All three targets exited 0 with these final lines from the rerun:

| Target | Final line | Coverage |
| --- | --- | --- |
| `parse` | Done 4733412 runs in 3613 second(s) | 7996 |
| `codec` | Done 20210273 runs in 3613 second(s) | 3588 |
| `canon` | Done 545030 runs in 3613 second(s) | 5544 |

The committed report's runs, at 4184 s, were not used. None of the three targets checks soundness: `canon` checks that equivalence moves keep hashes (completeness), and `parse` and `codec` check round-trips (G-2).

## 3. Adversarial checks

### 3.1 Hash pairs (`vg hash`, node hash compared)

Each pair is two .vgs files, a and b, for one node @f; appendix B lists them. "Want" is what docs/13 requires: "same" for an equivalence move, and "diff" for anything else.

| ID | a versus b | Want | Got |
| --- | --- | --- | --- |
| t04 | `PRE: n < 128` vs `PRE: n < -128` (Int constant encoding) | diff | diff |
| t05 | f16 literal on the midpoint 1.00048828125 vs the same digits plus 10^-24 (double rounding through f64) | diff | diff |
| t05b | the same for bf16 (1.00390625) | diff | diff |
| t06 | f32 midpoint 1.000000059604644775390625 vs the same plus 10^-28 | diff | diff |
| t06b | t05 as an op constant; disasm shows `1.0:f16` and `1.0009766:f16`, both correct | diff | diff |
| t07 | `POST: same(result, 0.0)` vs `same(result, -0.0)` | diff | diff |
| t08a | `forall(n in 0..4) { xs[n] > 0 }`, where the binder shadows parameter n, vs `forall(i …) { xs[i] > 0 }` | same | same |
| t08b | `xs[i]` vs `xs[n]` inside `forall(i …)` | diff | diff |
| t09 | `forall(i, j) { xs[i] <= xs[j] }` vs `{ xs[j] <= xs[i] }` | diff | diff |
| t09b | the inner binder renamed j vs shadowing i | diff | diff |
| t10 | `INVARIANT @inv(k, acc): acc >= k` vs `@inv(acc, k): acc >= k` | diff | diff |
| t11 | `FCLAMP(x, lo, hi)` vs `FCLAMP(x, hi, lo)` | diff | diff |
| t12 | `(a && b) \|\| c` vs `a && (b \|\| c)` | diff | diff |
| t12b | `PRE: a; PRE: b && c;` vs `PRE: a && b && c;` | same | same |
| t13 | `a ==> b ==> c` vs `(a ==> b) ==> c` | diff | diff |
| t14 | `n - 1 - 2` vs `n - (1 - 2)` | diff | diff |
| t15 | a dead `DIV.checked(x, 0:i32)` vs no op (see O3) | same | same |
| t16 | `[export]` vs no attribute (ADR 0001 line 147) | same | same |
| t17 / t17b | `x: {v: f32 \| v > 0.0}` vs `PRE: x > 0.0` / vs plain `x: f32` | same / diff | same / diff |
| t18 | `POST: result.0 > result.1` vs `result.1 > result.0` | diff | diff |
| t19 | `ADD.wrap(x, -0:i8)` vs `ADD.wrap(x, 0:i8)` | same | same |
| t20 | `LT.u(x, y)` vs `LT.u(y, x)` | diff | diff |

All 23 pairs came out as wanted (t17 and t17b share a row), and each `vg hash` exited 0.

### 3.2 Rust tests (appendix A) and the wide programs

| Test | What it requires | Result |
| --- | --- | --- |
| r01 | `Tuple<Tuple<u8 ×257>, u8>` and `Tuple<Tuple<u8>, u8 ×257>` signatures hash differently | **FAILED**: equal node hashes (B1) |
| r02 | Returning slot 0 (y) and returning slot 256 (x) of a 257-result CALL_HASH hash differently | **FAILED**: equal node hashes (B2) |
| r03 | A `Tuple<u8 ×256>` identity node assembles, decodes and re-encodes | **FAILED**: `vg asm` output fails to decode, "unknown type tag 0x13" (B3) |
| r04 | A flipped header hash byte is refused | passed |
| r05 | Trailing or truncated bytes are refused | passed |
| r06 | Two CLOCK_READs on one Eff token stay two ops | **FAILED**: merged into one (B4) |
| r07 | The decoder refuses .vgir whose Eff token feeds two effect ops | **FAILED**: accepted (B5) |
| r08 | A NaN constant with a payload is refused | passed |

The same programs through the CLI (files in the notes folder; see section 6):

- `vg hash t01_P.vgs` and `vg hash t01_Q.vgs` each exit 0 and print the same three hashes, `node e271d220…1c2f body 8626705a…3010 contract 41a86d26…7d02`.
- `vg asm` refuses both, with exit 1 and the misleading `VG-S0202 unknown type tag 0x00` and `VG-S0202 section longer than its entries`.
- `vg hash t02_ret0.vgs` and `t02_ret256.vgs` each exit 0, and @f has node `3196ece2…4a35` in both.
- `t21_tensor255.vgs`, an identity on `Tensor<u8, [1 ×255]>`: `vg hash` exits 0, and `vg asm` exits 1 with `VG-S0202 unknown type tag 0x12`.

### 3.3 Gate copies

| ID | What the check does | Rows 0–3 | Outcome |
| --- | --- | --- | --- |
| g0 | Control: unmodified copy | all met (row 4: 60 s, not met) | Baseline |
| g1 | r01–r08 committed (5 fail) and the copy left unformatted; untracked `.cargo/config.toml` with a test runner that appends `--skip r0` | green: not met (fmt exit 1); fuzz exit 1, because the runner also wrapped the fuzz binaries | Caught, by fmt alone |
| g1b | g1 with fmt applied and committed; the runner skips only `target/debug/deps/*` | **all met**; `cargo test`: exit 0, "8 filtered out" | **Not caught** (B6) |
| g2 | A 57th alpha family, untracked and hidden by `.git/info/exclude` | test exit 101: parity fixture out of date | Caught, by the Lark fixture |
| g2b | g2 with the regenerated parity fixture committed | **all met**; tree: met; "57 alpha families"; parity "248 files" | **Not caught** (B7) |
| g3 | All 16 distinct families deleted, committed | test exit 101: parity fixture out of date | Caught, by the Lark fixture |
| g3b | g3 with the regenerated fixture committed | **all met**; "0 distinct families, 0 pairs: all different" | **Not caught** (B8) |
| g4 | `fuzz/fuzz_targets/codec.rs` emptied to `fuzz_target!(\|_data\| {})`, committed | all met; codec exit 0, coverage 14 (3500 when intact) | **Not caught** (G-1) |

Things I checked by reading the code rather than by a gate run:

- The script captures real exit codes. Lines 23-24 are `"$@" >log; code=$?`, so G0's B4 is fixed.
- A failed report write exits 1 (lines 156-158), so G0's B6 is fixed.
- A `.cargo` alias named clippy, fmt or test marks the tree not met (lines 53-59), so G0's G-4 is fixed for aliases.

## 4. Bugs and gaps

Severity levels:

- **High:** two inequivalent programs share a hash, or the gate reports a G1 criterion met while it is false.
- **Medium:** a law fails on valid input, a stated invariant is broken, or the gate can be fooled by a change outside its inputs.
- **Low:** fails safe, or is hard to reach.

No fix was made.

### High

**B1. A type's member count is hashed as one byte, so different types share structural bytes** (crates/vg-core/src/types.rs:191 Tuple, :184 Tensor; ADR 0001 lines 34 and 116 specify `argc u8`).

- `Tuple<ms>` writes `[TUPLE, ms.len() as u8]`, then each member.
- With 258 members the count byte reads 2. So `Tuple<Tuple<u8 ×257>, u8>` and `Tuple<Tuple<u8>, u8 ×257>` both serialize as `[TUPLE,2][TUPLE,1]` followed by 258 × `u8`.
- Every hash takes types in through these bytes: input operands (canon.rs:267-275), op results (canon.rs:460), and the signature in node_hash (canon.rs:528-531). So r01's two nodes share node, body and contract hash.
- Tensor's `(1 + dims.len()) as u8` has the same flaw at 255 dimensions. I did not build a Tensor collision.
- Nothing bounds widths: lower_type (lower.rs:56-104) limits nesting depth only, and ADR 0001's resource limits (line 155) list no width.

Fix: reject widths the format cannot hold, in lowering and in validate(). A tuple has at most 255 members and a tensor at most 254 dimensions, each with a diagnostic code. The other way is a wider argc, which changes the frozen-to-be format. Add r01 as a distinct family either way.

**B2. Op results beyond slot 255 alias earlier slots** (crates/vg-core/src/lower.rs:875; canon.rs:459; codec.rs:218).

- Lowering binds port k of an op as `Operand::Op(index, slot as u8)`, so with 257 results port %256 becomes slot 0.
- In r02, `RETURN(%256)` returns @g's slot 0 (the value y) instead of slot 256 (x), and hashes exactly like `RETURN(%0)`.
- The op hash's `result_count` (canon.rs:459) and the codec's `nres` (codec.rs:218) also wrap at 256. Port slots are u8 by design (ir.rs, `Operand::Op(u32, u8)`), so the bug is the missing limit.
- An op gets 257 results from CALL_HASH or ASSERT_GUARD on a callee with 257 results. Nodes may declare any number of results.

Fix: reject an op or a node with more than 256 results (VG-S01xx), and add r02 as a distinct family.

### Medium

**B3. Valid wide programs hash but cannot be stored** (codec.rs:162, `args.len() as u8` in TYPES; codec.rs:218).

- `vg hash` prints a hash for a 256-member tuple, a 255-dimension tensor (t21) or a 257-result op.
- `vg asm` then fails, because the store decodes what encode wrote, with a message about a corrupt file ("unknown type tag", "section longer than its entries").
- So the round-trip law fails on text the grammar accepts, and a hash is printed for bytes that cannot exist.
- Root cause and fix: the same as B1 and B2. The parse fuzz target could find it only with inputs over about 1.3 KB of repetitive text.

**B4. Canonicalization merges effect ops, contrary to the opcode table** (canon.rs:562-570; opcode.rs:166).

- opcode.rs:166 says effect ops "are never merged or dropped", and docs/13 says "Only pure ops are ever merged or dropped". But step 5 merges every pair of ops with equal Merkle hashes, and `is_effect` is never called outside its own test.
- In r06, two CLOCK_READs on one Eff token and one clock handle become one op. The node hashes like a node with a single read whose result is used twice.
- VG-Sym cannot produce this, because lowering rejects an Eff used twice (lower.rs:1027, VG-E0102). The API and the decoder can (B5).

Fix: exclude effect ops from merging and dead-op removal, as the comment promises, or check Eff linearity in validate().

**B5. The decoder accepts a forked Eff token** (codec.rs:390-703; canon.rs:50).

- validate() checks structure only. Eff linearity is checked in lowering alone (lower.rs:539).
- r07 builds two CLOCK_READs that share one Eff input but read different clock handles. The node is canonical, so `decode` accepts it.
- ADR 0001 lines 203-204 make the decoder the gate for every node that comes back from the store or from Mesh. Such a node has no total effect order, which docs/13 says every node has.
- vg-types (P2.2) may catch it later. Today nothing does.

Fix: run the Eff-linearity check in validate(), so both paths share it.

**B6. A cargo configuration outside the gate's inputs can hide failing tests** (scripts/gate-g1.sh:41-59).

- The inputs list leaves out `.cargo/`. The config check only greps for clippy, fmt and test aliases.
- In g1b, an untracked `.cargo/config.toml` set `[target.aarch64-apple-darwin] runner`, and that runner appended `--skip r0` to the test binaries. Five failing soundness tests were "filtered out", `cargo test` exited 0, and rows 0 to 3 read met.
- The same holds for a config in any parent directory, `$CARGO_HOME/config.toml`, and `CARGO_TARGET_<triple>_RUNNER` or `CARGO_BUILD_*` in the environment.

Fix:

- Add `.cargo` to the inputs.
- Refuse any `runner`, `[alias]`, `[build]` or `[env]` key in a cargo config found from the repository up to `/` and in `$CARGO_HOME`, and any `CARGO_TARGET_*_RUNNER` variable.
- Or run the gate from a fresh clone with `CARGO_HOME` set to an empty directory.

### Low

**B7. Ignored files are invisible to the tree check** (scripts/gate-g1.sh:43). `git status --porcelain` lists no ignored files. A family hidden by `.git/info/exclude` was counted in g2b ("57 alpha families") while the tree read "met". The committed parity fixture caught it until the fixture itself was regenerated and committed. Fix: add `--ignored`, or evaluate a clean `git worktree` of HEAD.

**B8. The distinctness row has no minimum** (scripts/gate-g1.sh:76; crates/vg-core/tests/families.rs:113-150). With every distinct family deleted, g3b reads "0 distinct families, 0 pairs: all different" and row 2 is met. Fix: require 16 or more families (or the agreed number) and assert it in the test.

### Gaps

- **G-1. Fuzz targets are run, not checked** (scripts/gate-g1.sh:96-116). An emptied codec target "runs an hour without a crash" (g4, coverage 14). A committed change of this kind needs a reviewer, but the gate could require a minimum coverage per target, recorded from a known-good run.
- **G-2. No fuzz target looks for collisions.** `canon` checks only that moves keep hashes. A target that mutates one generated program away from its class and asserts that the hash changes would cover the soundness side that docs/16 line 44 calls "the worse failure". The near-miss test `hashes_follow_unraveling` does this for 400 seeded bodies, but only over the generator's shapes.
- **G-3. Rows 1 and 2 test evidence files for existence.** This is safe today, because every evidence file is written by a test that must pass. But a test that writes its file before its last assertion would make the row pass on a partial run.
- **G-4. The fuzz toolchain is not pinned.** `+nightly` resolves to whatever nightly is installed (1.101.0-nightly 2026-10-29 here). toolchain.lock does not record it.

## 5. Observations (not G1 criteria)

- **O1.** ROADMAP.md line 95 leaves P1.5 (hashes) unticked, although every other P1 task is ticked and the hashes are implemented. PROGRESS.md line 38 asks the owner about the scc hash, which explains the box but should be resolved before approval.
- **O2.** The committed report names bf3935d, and the review ran on fe499c8. The gate inputs are the same, and the rerun's rows are the same except the fuzz counts.
- **O3. Dropping dead partial ops.** docs/13 says evaluation is strict ("every op is evaluated") and that partial ops trap. Step 5 drops a dead `DIV.checked(x, 0)` (t15), so the text traps under strict reading and the canonical node doesn't. docs/13 defines a node's meaning on its canonical graph and lists dead-op removal as a move, so this is consistent, but the sentence about strictness should say "of the canonical graph" where it first appears.
- **O4. Elapsed fuzz time exceeds FUZZ_SECONDS under load.** With FUZZ_SECONDS=60 and 15 fuzzers on the host, every copy reported "Done … in 926–981 second(s)". The full run reported 3613 s, and the committed report 4184 s. The gate's `secs >= 3600` check reads libFuzzer's elapsed time, which is what "an hour" asks for. I did not find out where the extra time went.
- **O5.** `export` and `inline` never reach a hash, as ADR 0001 line 147 decides (t16), although `export` changes the code vg-codegen emits (a checked entry point, ADR 0009). That is the owner's call, but it means two artifacts with one hash can differ at the trust boundary.

## 6. Not checked, and limits

- No Linux host was used; every result is from aarch64-apple-darwin.
- Formula argument counts are `u16` (canon.rs:239, `v.len() as u16`). Truncation needs a call with 65,536 arguments; I did not test it.
- Collisions through Tensor dimensions (B1) were argued from the code, not built.
- I did not review lexer and parser error paths beyond the Lark parity, the store (vg-store), `vg disasm` on many-node graphs, or the golden files' byte layouts against ADR 0001 field by field.
- The adversarial tests are not in the tree, as the task asks. Appendix A holds them, and the notes folder of this session group holds them with the pair files and their generator. Those are outside the repository and may not persist; appendix B is enough to rebuild the pairs.
- No code was fixed, nothing was committed or pushed in this checkout, and no CI run was triggered.

## Appendix A: crates/vg-core/tests/review_adversarial.rs

Run it with `cargo test -p vg-core --test review_adversarial` after copying it into `crates/vg-core/tests/`. At fe499c8, r01, r02, r03, r06 and r07 fail; r04, r05 and r08 pass. The code is as I ran it, after `cargo fmt`.

<details>
<summary>review_adversarial.rs</summary>

```rust
//! Gate G1 independent review: adversarial tests. Each test asserts what docs/13, docs/02 and
//! ADR 0001 require; a failure is a finding of reports/gate-g1-review.md.

mod common;

use std::fs;

use common::repo;
use vg_core::canon::{canonicalize, hashes};
use vg_core::codec::{decode, encode};
use vg_core::ir::{Contract, Node, Op, Operand};
use vg_core::types::{EffectMask, Handle, Type};
use vg_core::{asm_local, opcode};

fn node_hash(src: &str, label: &str) -> [u8; 32] {
    asm_local(src)
        .unwrap_or_else(|e| panic!("{label}: {e}"))
        .iter()
        .find(|a| a.label == label)
        .unwrap_or_else(|| panic!("no node @{label}"))
        .hashes
        .node
}

fn list(item: &str, n: usize) -> String {
    vec![item; n].join(", ")
}

fn identity_node(t: &str) -> String {
    format!(
        "module r;\n\nNODE @id {{\n  SIGNATURE: (x: {t}) -> ({t});\n  EFFECTS: [];\n  CONTRACT {{ }}\n  BODY {{\n    %0 = IDENTITY(x);\n    RETURN(%0);\n  }}\n}}\n"
    )
}

/// Inequivalent: two different signature types. The argc byte of a Tuple's structural bytes is
/// `len as u8`, so a 258-member tuple claims 2 members and the byte strings coincide.
#[test]
fn r01_tuple_member_count_is_not_truncated() {
    let p = format!("Tuple<Tuple<{}>, u8>", list("u8", 257));
    let q = format!("Tuple<Tuple<u8>, {}>", list("u8", 257));
    assert_ne!(
        node_hash(&identity_node(&p), "id"),
        node_hash(&identity_node(&q), "id"),
        "different types share a node hash"
    );
}

/// Inequivalent: @f returns slot 0 (the value y) in one text and slot 256 (the value x) in the other.
#[test]
fn r02_result_slot_256_is_not_slot_0() {
    let src = |k: usize| {
        let rets = std::iter::once("y")
            .chain(std::iter::repeat_n("x", 256))
            .collect::<Vec<_>>()
            .join(", ");
        let ports = (0..257)
            .map(|i| format!("%{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "module r;\n\nNODE @g {{\n  SIGNATURE: (x: u8, y: u8) -> ({});\n  EFFECTS: [];\n  CONTRACT {{ }}\n  BODY {{\n    RETURN({rets});\n  }}\n}}\n\nNODE @f {{\n  SIGNATURE: (x: u8, y: u8) -> (u8);\n  EFFECTS: [];\n  CONTRACT {{ }}\n  BODY {{\n    {ports} = CALL_HASH(@g, x, y);\n    RETURN(%{k});\n  }}\n}}\n",
            list("u8", 257)
        )
    };
    assert_ne!(
        node_hash(&src(0), "f"),
        node_hash(&src(256), "f"),
        "slot 256 is hashed as slot 0"
    );
}

/// A node the text accepts must also assemble, round-trip and decode (criterion 1).
#[test]
fn r03_wide_tuple_round_trips() {
    let src = identity_node(&format!("Tuple<{}>", list("u8", 256)));
    let a = asm_local(&src).expect("assembles");
    let n = decode(&a[0].bytes).expect("vg asm output decodes");
    assert_eq!(encode(&n).0, a[0].bytes);
}

fn golden(name: &str) -> Vec<u8> {
    fs::read(repo().join("tests/golden").join(name)).expect("golden file")
}

#[test]
fn r04_decoder_refuses_a_wrong_header_hash() {
    for (at, what) in [(16, "node"), (48, "body"), (80, "contract")] {
        let mut b = golden("minimal.inc.vgir");
        assert!(decode(&b).is_ok());
        b[at] ^= 1;
        assert!(
            decode(&b).is_err(),
            "a flipped {what} hash byte was accepted"
        );
    }
}

#[test]
fn r05_decoder_refuses_trailing_and_truncated_bytes() {
    let b = golden("sections.all.vgir");
    assert!(decode(&b).is_ok());
    let mut longer = b.clone();
    longer.extend([0u8; 16]);
    assert!(decode(&longer).is_err(), "16 trailing zero bytes accepted");
    assert!(
        decode(&b[..b.len() - 16]).is_err(),
        "truncated file accepted"
    );
}

/// A node built through the API: params, results, ops; no contract.
fn api_node(
    effects: u32,
    params: Vec<Type>,
    results: Vec<Type>,
    ops: Vec<Op>,
    outputs: Vec<Operand>,
) -> Node {
    Node {
        spec: false,
        effects: EffectMask(effects),
        params,
        results,
        learnable: vec![],
        consts: vec![],
        calls: vec![],
        ops,
        outputs,
        contract: Contract::default(),
        debug: None,
    }
}

fn clock_read(eff: u32) -> Op {
    Op {
        opcode: opcode::by_name("CLOCK_READ").unwrap().code,
        mode: 0,
        operands: vec![Operand::Input(eff), Operand::Input(1)],
        results: vec![Type::Eff, Type::UInt(64)],
    }
}

/// Two CLOCK_READs on one Eff token are two reads; one CLOCK_READ read twice is one. opcode.rs says
/// effect ops are never merged; canonicalization merges them anyway.
#[test]
fn r06_effect_ops_are_never_merged() {
    let clock = EffectMask::bit("CLOCK").unwrap();
    let sig = (
        vec![Type::Eff, Type::Handle(Handle::Clock)],
        vec![Type::UInt(64), Type::UInt(64)],
    );
    let two = api_node(
        clock,
        sig.0.clone(),
        sig.1.clone(),
        vec![clock_read(0), clock_read(0)],
        vec![Operand::Op(0, 1), Operand::Op(1, 1)],
    );
    let one = api_node(
        clock,
        sig.0,
        sig.1,
        vec![clock_read(0)],
        vec![Operand::Op(0, 1), Operand::Op(0, 1)],
    );
    assert_eq!(
        canonicalize(&two).expect("valid").node.ops.len(),
        2,
        "two effect ops merged into one"
    );
    assert_ne!(hashes(&two).unwrap().node, hashes(&one).unwrap().node);
}

/// Eff is linear (VG-E0102): .vgir in which one Eff token feeds two effect ops is not a program the
/// text can express. The decoder is the path every stored or Mesh node takes back in.
#[test]
fn r07_decoder_refuses_a_forked_eff_token() {
    let clock = EffectMask::bit("CLOCK").unwrap();
    let mut second = clock_read(0);
    second.operands[1] = Operand::Input(2);
    let n = api_node(
        clock,
        vec![
            Type::Eff,
            Type::Handle(Handle::Clock),
            Type::Handle(Handle::Clock),
        ],
        vec![Type::UInt(64), Type::UInt(64)],
        vec![clock_read(0), second],
        vec![Operand::Op(0, 1), Operand::Op(1, 1)],
    );
    let c = canonicalize(&n).expect("structurally valid").node;
    let (bytes, _) = encode(&c);
    assert!(
        decode(&bytes).is_err(),
        "decoder accepted an Eff token used twice"
    );
}

#[test]
fn r08_decoder_refuses_a_noncanonical_nan_constant() {
    let mut n = api_node(
        0,
        vec![Type::Float(32)],
        vec![Type::Float(32)],
        vec![],
        vec![Operand::Const(0)],
    );
    n.consts = vec![vg_core::ir::Const {
        ty: Type::Float(32),
        bytes: 0x7FC0_0001u32.to_le_bytes().to_vec(),
    }];
    let (bytes, _) = encode(&canonicalize(&n).expect("valid").node);
    assert!(decode(&bytes).is_err(), "NaN payload constant accepted");
}
```

</details>

## Appendix B: the hash pairs

Each pair is one module with a node @f, built from this template; `SIG`, `CONTRACT` and `BODY` are the cells of the table. t10 adds the kernel `@step` from tests/corpus/distinct/d16; t16 writes `NODE @f [export] {` in b.

```text
module p;

NODE @f {
  SIGNATURE: SIG;
  EFFECTS: [];
  CONTRACT {
    CONTRACT
  }
  BODY {
    BODY
  }
}
```

| ID | SIG | a | b | BODY |
| --- | --- | --- | --- | --- |
| t04 | `(xs: Array<i32, 4>, n: u64) -> (i32)` | `PRE: n < 128;` | `PRE: n < -128;` | `%0 = INDEX(xs, 0:u64); RETURN(%0);` |
| t05 | `(x: f16) -> (f16)` | `PRE: x == 1.00048828125;` | `PRE: x == 1.000488281250000000000001;` | `RETURN(x);` |
| t05b | `(x: bf16) -> (bf16)` | `PRE: x == 1.00390625;` | `PRE: x == 1.003906250000000000000001;` | `RETURN(x);` |
| t06 | `(x: f32) -> (f32)` | `PRE: x == 1.000000059604644775390625;` | `PRE: x == 1.0000000596046447753906250001;` | `RETURN(x);` |
| t06b | `(x: f16) -> (f16)` | body `FADD(x, 1.00048828125:f16)` | body `FADD(x, 1.000488281250000000000001:f16)` | `%0 = FADD(…); RETURN(%0);` |
| t07 | `(x: f32) -> (f32)` | `POST: same(result, 0.0);` | `POST: same(result, -0.0);` | `RETURN(x);` |
| t08a | as t04 | `PRE: forall(n in 0..4) { xs[n] > 0 };` | `PRE: forall(i in 0..4) { xs[i] > 0 };` | as t04 |
| t08b | as t04 | `PRE: forall(i in 0..4) { xs[i] > 0 };` | `PRE: forall(i in 0..4) { xs[n] > 0 };` | as t04 |
| t09 | as t04 | `PRE: forall(i in 0..4, j in 0..4) { xs[i] <= xs[j] };` | `… { xs[j] <= xs[i] };` | as t04 |
| t09b | as t04 | `PRE: forall(i in 0..4) { forall(j in 0..i) { xs[j] <= xs[i] } };` | `… forall(i in 0..i) { xs[i] <= xs[i] } };` | as t04 |
| t10 | `(arr: Array<u32, 4>) -> (u32)` | `INVARIANT @inv(k, acc): acc >= k;` | `INVARIANT @inv(acc, k): acc >= k;` | `%0 = FOLD_BOUNDED(@step, arr, init=0:u32, invariant=@inv); RETURN(%0);` |
| t11 | `(x: f32, lo: f32, hi: f32) -> (f32)` | body `FCLAMP(x, lo, hi)` | body `FCLAMP(x, hi, lo)` | `%0 = FCLAMP(…); RETURN(%0);` |
| t12 | as t04 | `PRE: (n > 1 && n > 2) \|\| n > 3;` | `PRE: n > 1 && (n > 2 \|\| n > 3);` | as t04 |
| t12b | as t04 | `PRE: n > 1; PRE: n > 2 && n < 9;` | `PRE: n > 1 && n > 2 && n < 9;` | as t04 |
| t13 | as t04 | `PRE: n > 1 ==> n > 2 ==> n > 3;` | `PRE: (n > 1 ==> n > 2) ==> n > 3;` | as t04 |
| t14 | as t04 | `PRE: n - 1 - 2 > 0;` | `PRE: n - (1 - 2) > 0;` | as t04 |
| t15 | `(x: i32) -> (i32)` | body `%0 = DIV.checked(x, 0:i32); RETURN(x);` | body `RETURN(x);` | — |
| t16 | `(x: f32) -> (f32)` | `NODE @f {` | `NODE @f [export] {` | `RETURN(x);` |
| t17 | a: `(x: {v: f32 \| v > 0.0}) -> (f32)`, b: `(x: f32) -> (f32)` | no clause | `PRE: x > 0.0;` | `RETURN(x);` |
| t17b | as t17 | no clause | no clause | `RETURN(x);` |
| t18 | `(x: u8, y: u8) -> (u8, u8)` | `POST: result.0 > result.1;` | `POST: result.1 > result.0;` | `RETURN(x, y);` |
| t19 | `(x: i8) -> (i8)` | body `ADD.wrap(x, -0:i8)` | body `ADD.wrap(x, 0:i8)` | `%0 = ADD.wrap(…); RETURN(%0);` |
| t20 | `(x: u8, y: u8) -> (Bool)` | body `LT.u(x, y)` | body `LT.u(y, x)` | `%0 = LT.u(…); RETURN(%0);` |
