# Gate G0 · Independent review

Reviewer: an independent Claude Code session (Claude Opus 5.5), started from the W0 session on 2026-10-29 and run as the "Gate review" prompt in PROMPTS.md asks. `reports/gate-g0.md` was not used as evidence. Every claim below comes from the repository, a rerun of `make gate-g0`, or the GitHub API. No code was fixed, nothing was committed or pushed, and no CI run was triggered.

Reviewed: commit `e4dc6e3c909c10b2d53d8582a94f249d2d7dc782`, HEAD of `main` and equal to `origin/main`, with a clean working tree. Criteria: the G0 row of ROADMAP.md (line 67), the P0 tasks (ROADMAP.md from line 81) and AGENTS.md.

## Verdict

| # | G0 criterion (ROADMAP.md) | Verdict | Evidence |
| --- | --- | --- | --- |
| 1 | The workspace builds on macOS ARM64 and Linux x86_64 | **met** | Section 2.1 |
| 2 | CI is green | **met** | Section 2.2 |
| 3 | The spec docs are in the repo | **met** | Section 2.3 |

`make gate-g0` exits 0 on the real tree, and its verdict is correct for this commit. The script itself is weaker than its report suggests. I ran 26 adversarial checks:

- 12 broken trees were caught, as they should be.
- 3 checks confirmed claims the script makes.
- 8 broken trees were reported as "every criterion holds". One of them (c18) is outside today's G0 list.
- 2 times the script exited 0 without writing its report.
- 1 good tree was failed.

Together they show 7 bugs and 5 gaps (section 4). None of them changes the verdict above, because the evidence in section 2 does not depend on the script.

Recommendation for the owner: G0 can be approved for commit e4dc6e3 on this evidence. Bugs B1 to B6 should be fixed before gate-g0.sh becomes the template for gate-g1 to gate-g8, because the owner approves gates based on what these scripts write.

## 1. What I ran

Host: macOS 27.2 (26B118), arm64. Tools: rustc 1.90.0 (1159e78c4 2025-09-14), cargo 1.90.0, clippy 0.1.90, rustfmt 1.8.0, rustup 1.28.2, GNU Make 3.81, `/bin/sh` (bash 3.2.57), gh 2.83.1 (logged in).

1. `make gate-g0` on the real tree at e4dc6e3: **exit 0**, `wrote reports/gate-g0.md: every criterion holds; awaiting the owner's approval`. The regenerated report differs from the committed one in a single line: the CI row names e4dc6e3 instead of 29d25ea (see O3). Afterwards I restored the committed bytes of `reports/gate-g0.md`, so the tree is as I found it apart from this file.
2. Read-only GitHub queries: `gh run list --workflow ci.yml`, `gh run view <run> --json jobs`, and `gh run view --job <id> --log` for all four jobs.
3. git plumbing on HEAD (`git cat-file -s HEAD:<path>`, `git ls-tree`, `git status --porcelain --ignored`) for the docs, the research tracks and the crates.
4. A control plus 26 adversarial checks. Each ran on its own copy of the repository, in a scratch directory outside the repo: a `cp -R` copy with the push URL disabled, or a fresh `git clone` for c23. The harness is in the appendix. I ran the full set twice, and every exit code matched between the two runs.

## 2. Evidence per criterion

### 2.1 The workspace builds on macOS ARM64 and Linux x86_64: met

- **macOS ARM64, locally.** My rerun on `aarch64-apple-darwin` ran three commands with `RUSTFLAGS=-D warnings`, and all exited 0:
  - `cargo fmt --all --check`
  - `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - `cargo test --workspace --locked`

  A fresh clone of HEAD also passes (c23), so nothing depends on untracked or ignored files.
- **Both targets, in CI.** Run 40837987092 built e4dc6e3 (event `push`, branch `main`). From the job logs:

  | Job (id) | Runner image | `rustc -vV` host | Toolchain | Crates checked by clippy / compiled by test |
  | --- | --- | --- | --- | --- |
  | linux-x86_64 (123327216141) | ubuntu-24.04, 20261025.353.1 | `x86_64-unknown-linux-gnu` | 1.90.0 (1159e78c4 2025-09-14) | 12 / 12 |
  | macos-arm64 (123327216577) | macos-15-arm64, 20260907.0337.1 | `aarch64-apple-darwin` | 1.90.0 (1159e78c4 2025-09-14) | 12 / 12 |

  Run 40837848645 for 29d25ea shows the same lines (jobs 123326765326 and 123326765591).
- **The 12 crates.** HEAD holds exactly the crates listed in AGENTS.md section 3 and docs/01-architecture.md: vg-cli, vg-codegen, vg-core, vg-decode, vg-interp, vg-mesh, vg-omnilift, vg-rt, vg-smt, vg-store, vg-telemetry and vg-types. The names come from each Cargo.toml in HEAD.
- **Caveat: the workspace has no tests yet.** In every CI job, `cargo test` ran 23 harnesses (12 unit, 11 doc), and each reported "0 passed; 0 failed". That is expected for skeletons. The test step shows only that the test profile builds.

### 2.2 CI is green: met

`gh run list --workflow ci.yml` returns two runs, both triggered by `push` to `main`, and both `completed` with `success`:

- 40837848645, for 29d25ea;
- 40837987092, for e4dc6e3.

In both runs, both jobs concluded `success`, and so did every step: checkout, toolchain install, host print, fmt, clippy and test.

### 2.3 The spec docs are in the repo: met

I checked HEAD itself, not the disk, with `git cat-file -s HEAD:<path>`. All of these exist and are non-empty:

- the 19 files the gate lists: AGENTS.md, ROADMAP.md, PROGRESS.md, docs/00 to docs/12, docs/source/original-spec.md (42,904 bytes), docs/research/PROTOCOL.md and docs/research/README.md;
- all 14 research tracks, R-01 to R-14, matching the table in docs/research/README.md (2,023 to 3,673 bytes each);
- outside the gate's list: docs/13 to docs/16 (13,548, 17,255, 18,533 and 10,183 bytes), ADRs 0001 to 0010, skills/verigraph-synthesis/SKILL.md and tools/vgsym.lark.

`git status --porcelain --ignored` shows nothing untracked or ignored except `target/`. I did not review whether the docs are correct; that is outside G0.

## 3. Adversarial checks

Each check copies the repository, breaks one thing, runs `make gate-g0` and reads the report. `make` exits 2 whenever the gate script exits 1. A correct gate fails every check except four:

- c00 is the control;
- c09, c21 and c23 should pass;
- c20 tests the other gate targets.

Six checks need a CI result for a commit that was never pushed: c06, c07, c08, c09, c11 and c17. For these, a stub `gh` sits first on PATH and prints what `gh run list --json conclusion --jq '.[].conclusion'` would print. The stub also logged the gate's query: `run list --workflow ci.yml --commit <HEAD> --json conclusion --jq .[].conclusion`.

Where the stub says `success` (c06, c11, c17), that is what GitHub would report: ci.yml runs only fmt, clippy and tests, and each of those trees passes them. Every other check uses the real GitHub API.

| ID | What the check does | `make gate-g0` | Outcome |
| --- | --- | --- | --- |
| c00 | Control: unmodified copy | exit 0 | Baseline; same report as the real tree |
| c01 | A `clippy::needless_return` in vg-mesh (fmt still clean) | exit 2, criterion 1 not met | Caught, but the evidence says the failing clippy command exited 0 (B4) |
| c02 | A rustfmt violation in vg-types | exit 2 | Caught, with the same false "exit 0" (B4) |
| c03 | A failing unit test in vg-core | exit 2 | Caught, with the same false "exit 0" (B4) |
| c04 | 11 crates: vg-omnilift removed and Cargo.lock regenerated, so cargo itself passes | exit 2, "11 crates" | Caught |
| c05 | 13 crates: vg-extra added and the lock regenerated | exit 2, "13 crates" | Caught |
| c06 | vg-mesh replaced by a crate named vg-impostor, still 12 crates; committed; CI `success` | **exit 0** | **Not caught** (G-1) |
| c06b | vg-omnilift emptied to a bare directory | exit 2: cargo "failed to load manifest for workspace member" | Caught by cargo; the count still read 12 |
| c07 | CI conclusion `failure` | exit 2 | Caught |
| c08 | Two CI runs for HEAD, one `success` and one `failure` | exit 2 | Caught, but the two-line value breaks the report's table (B7) |
| c09 | Two CI runs for HEAD, both `success` (push plus pull_request) | **exit 2** | **False negative** (B7) |
| c10 | HEAD is a local commit that was never pushed (real API) | exit 2, "no run found" | Caught |
| c11 | macOS job deleted from ci.yml; committed; CI `success` | **exit 0**; the report still says "(jobs linux-x86_64 and macos-arm64)" | **Not caught** (B1) |
| c12 | docs/05-contracts-verification.md deleted | exit 2, listed as missing | Caught |
| c13 | docs/11-errata.md emptied to 0 bytes | exit 2, listed as missing | Caught |
| c14 | docs/03-vg-sym.md replaced by one byte | **exit 0** | **Not caught** (G-2) |
| c15 | Research track R-07 deleted, leaving 13 tracks | **exit 0**, "13 research tracks" | **Not caught** (B3) |
| c15b | R-07 and R-13 deleted | exit 2, "12 of at least 13" | Caught |
| c16 | All 14 research track files emptied to 0 bytes | **exit 0** | **Not caught** (B3) |
| c17 | docs/10-bootstrap.md removed from the repository with `git rm --cached` and committed, but left on disk; CI `success` | **exit 0** | **Not caught** (B2) |
| c18 | docs/13 to docs/16 and ADRs 0002 to 0010 deleted | exit 0 | Not caught; outside the G0 list today (G-3) |
| c19 | reports/gate-g0.md made read-only | **exit 0**. It prints "wrote reports/gate-g0.md: every criterion holds", stderr shows "Permission denied", and the stale report for 29d25ea stays | **Bug** (B6) |
| c19b | reports/ deleted | **exit 0**, with the same message. Stderr shows "No such file or directory", and no report exists | **Bug** (B6) |
| c20 | `make gate-g1` to `make gate-g8` | Each exits 2 with "gate-gN: NOT IMPLEMENTED …" and writes no report | As P0.4 requires |
| c21 | `make gate-g0` twice on one tree | Both exit 0, and the reports are byte-identical | The determinism claim holds |
| c22 | The c01 lint, plus a repository `.cargo/config.toml` that aliases `clippy` to `test --no-run` | **exit 0**. Cargo warns that the alias shadows cargo-clippy, then runs it anyway | **Not caught** by the gate or by CI (G-4) |
| c23 | Fresh `git clone` of HEAD (tracked files only) | exit 0 | Passes, as it should |

The review task named five example breaks:

| Example break | Check | Result |
| --- | --- | --- |
| A required doc is missing | c12 | Caught |
| A crate fails clippy | c01 | Caught |
| The CI conclusion is not `success` | c07, c08, c10 | Caught |
| There are 11 crates | c04 | Caught |
| A research track is deleted | c15 | **Missed** |

## 4. Bugs and gaps

Severity levels:

- **High:** the gate reports that every criterion holds while a G0 criterion is false.
- **Medium:** the report's evidence is false, or describes a different tree from the one evaluated.
- **Low:** a false negative or a robustness problem.

No fix was made.

### High

**B1. The CI half of criterion 1 is asserted, not checked** (scripts/gate-g0.sh:49-52; .github/workflows/ci.yml:28-29).

- The gate reads only the run's overall `conclusion`. It then writes "(jobs linux-x86_64 and macos-arm64)" as fixed text.
- In c11 the macOS job was deleted, so nothing was built on macOS ARM64. The gate still passes and still claims both jobs.
- In CI, the step "Confirm the host architecture" only prints `rustc -vV | grep '^host:'` and never compares it with anything. A runner of another architecture behind the same job name would stay green.
- For e4dc6e3 the jobs and hosts are right (section 2.1); the gate just doesn't check them.

Fix: read `gh run view <id> --json jobs` and require both job names with conclusion `success`. Make the CI step assert the triple, for example `rustc -vV | grep -qx "host: <expected triple>"`, with the triple in the matrix.

**B2. Criterion 3 checks the working tree, not the repository** (scripts/gate-g0.sh:61-71). `[ -s "$f" ]` and `ls` look at the disk. In c17, a doc removed from git but left on disk passes. CI can't catch this, because ci.yml never looks at docs. Fix: test the committed tree, for example with `git cat-file -s "HEAD:$f"`, or refuse to run on a dirty tree (see B5).

**B3. Research tracks are counted, not named, and their size is never checked** (scripts/gate-g0.sh:69-71). The threshold is "at least 13" while 14 tracks exist, so any one track can disappear (c15). All 14 can also be emptied (c16). Fix: check R-01 to R-14 by name with `-s`, as the docs loop does, taking the list from the table in docs/research/README.md.

### Medium

**B4. A failing command is recorded as "exit 0"** (scripts/gate-g0.sh:18-23).

- After `if cmd; then …; fi`, when the condition is false and there is no else branch, `$?` is 0. POSIX gives an `if` with no branch taken status 0, and I confirmed this with this host's /bin/sh.
- So `code=$?` on line 23 is always 0.
- c01 to c03 produced "`cargo clippy --workspace --all-targets --locked -- -D warnings`: exit 0. Last lines: … error: could not compile `vg-mesh` (lib) due to 1 previous error".
- The verdict stays right, but the evidence is false, which AGENTS.md line 22 forbids.

Fix: run `"$@" >"$log" 2>&1; code=$?`, then branch on `$code`.

**B5. Local results and CI results may describe different trees** (scripts/gate-g0.sh:32-56). Fmt, clippy, tests, the crate count and the docs are read from the working tree, while the CI row is for HEAD. Nothing checks that the two agree. c14, c15, c16 and c18 ran on uncommitted edits, and each report names commit e4dc6e3 as if that commit had been evaluated. Fix: fail, or mark the criteria not met, when `git status --porcelain` is non-empty.

**B6. The gate exits 0 when it cannot write its report** (scripts/gate-g0.sh:88-116).

- A failed `cat >"$report"` is ignored. The script still prints "wrote reports/gate-g0.md: every criterion holds" and exits 0.
- c19 leaves the stale report for 29d25ea in place; c19b leaves no report at all.
- AGENTS.md lines 85-86 say that make gate-gN "exits 0 only when every criterion holds and writes reports/gate-gN.md with the evidence".

Fix: exit 1 when the write fails.

### Low

**B7. Several CI runs for one commit read as "not met" and break the report's table** (scripts/gate-g0.sh:49-54; ci.yml:3-5).

- ci.yml triggers on both `push` and `pull_request`. A pull request from a branch of this repository therefore produces two runs with the same head SHA.
- The gate compares the joined output with `success`, so two green runs fail (c09).
- The two-line value also splits the report's table row (c08, c09).
- This fails safe, but it will block gates once workstreams merge through pull requests.

Fix: use `--jq 'length > 0 and all(.[]; .conclusion == "success")'` and test for `true`.

### Gaps

The gate never claims to check these, but a reader of "every criterion holds" may assume it does.

- **G-1. Crate identities** (scripts/gate-g0.sh:38-39). Only the number of directories under crates/ is counted, so c06 passes with vg-impostor in place of vg-mesh. Glob membership does make every directory a crate (c06b). Fix: compare the package names from `cargo metadata --no-deps --format-version 1` with the 12 names in docs/01.
- **G-2. Doc content** (scripts/gate-g0.sh:67). `-s` accepts a one-byte doc (c14). A cheap guard: check that each doc's first line is its expected heading, such as `# 03 · …`.
- **G-3. Doc list** (scripts/gate-g0.sh:61-66). docs/13 to docs/16 and ADRs 0002 to 0010 are not checked (c18), although README.md's map lists docs/13 to 16 as part of the specification. W0 says they await the owner's approval and are outside G0, so this becomes a gap only once they are approved; the list should grow then.
- **G-4. A repository cargo alias can replace clippy** (scripts/gate-g0.sh:36; ci.yml:31).
  - With `[alias] clippy = ["test", "--no-run"]` in `.cargo/config.toml`, cargo 1.90.0 warns that the alias shadows cargo-clippy, then runs `cargo test --no-run` instead. It exits 0 with a lint present (c22). Without the alias, the same command exits 101.
  - CI runs the same command and would be fooled the same way.
  - Cargo says this behavior is "being phased out", but the pinned 1.90.0 accepts it.

  Fix: fail when a repository `.cargo/config.toml` or `.cargo/config` defines a `clippy` or `fmt` alias.
- **G-5. P0 deliverables are checked for existence only** (scripts/gate-g0.sh:74-81). `toolchain.lock` is reported "present" although it pins nothing but rustc (see O1).

## 5. Observations on P0 (not G0 criteria)

- **P0.1, met.**
  - 12 crates, edition 2024, rust-version 1.90.
  - rust-toolchain.toml pins `1.90.0` with rustfmt and clippy, and both CI jobs installed and used exactly 1.90.0.
  - Warnings are denied through `RUSTFLAGS=-D warnings` in the Makefile, CI and the gate. There is no `[workspace.lints]`, so a bare `cargo build` doesn't deny them. That matches AGENTS.md section 4 ("warnings denied in CI").
- **P0.2, met** (section 2.1).
- **O1. P0.3 is only partly done.**
  - toolchain.lock pins rustc 1.90.0 and records `none` for clang, z3, cvc5, bitwuzla and wasi-sdk.
  - `scripts/check-toolchain.sh` exits 1 on this host: UNPINNED clang `apple-21.0.0`; MISSING z3, cvc5, bitwuzla and wasi-sdk.
  - The comment on toolchain.lock line 6 is stale. It says Apple clang 17.0.0 is installed, but this host now has Apple clang 21.0.0 (clang-2100.3.34.2).
  - This is not a G0 criterion, but P3 needs the solvers, and P4 needs clang 19 or newer plus wasi-sdk.
- **P0.4, met** (c20).
- **P0.5, met.** Every folder is tracked through a `.gitkeep`.
- **P0.6, met.** ADR 0001 is a stub that hands the decisions to P1.6.
- **P0.7.** The report exists and is reviewed here.
- **O2.** None of P0.1 to P0.7 is ticked in ROADMAP.md, although line 79 says to tick a task "in the same commit that completes it".
- **O3. The committed report can't name its own commit.** It describes 29d25ea; committing it created e4dc6e3, so a rerun now names e4dc6e3. CI for that commit also passed (run 40837987092). The approval should name the commit it covers; this review checked e4dc6e3.
- **O4.** The workspace has no tests (section 2.1); G1 brings the first.

## 6. Not checked, and limits

- No Linux host was available. The Linux x86_64 half of criterion 1 rests on CI job logs read through the API.
- The CI-dependent checks c06, c07, c08, c09, c11 and c17 used the stub `gh` described in section 3. Nothing was pushed, as the task requires, so GitHub never ran those trees. c10 and every other check used the real API.
- The harness uses BSD `sed -i ''` (macOS). c19 relies on file permissions, so it doesn't work when run as root.
- tools/check_examples.py was not run, and the docs' content was not reviewed; neither is a G0 criterion.
- No code was fixed, nothing was committed or pushed, and no CI run was triggered.

## Appendix: the harness

To reproduce section 3, run `ADV_DIR=<empty scratch dir> REPO=<path to this checkout> bash gate-g0-adversarial.sh all`. The harness only reads `$REPO`: every check runs `make gate-g0` on its own copy, with pushing disabled. Per-check outputs and reports go to `$ADV_DIR/results`.

<details>
<summary>gate-g0-adversarial.sh</summary>

```bash
#!/bin/bash
# Adversarial checks against scripts/gate-g0.sh (VeriGraph G0 review, reports/gate-g0-review.md).
# Every check runs `make gate-g0` on its own copy of the repository under $ADV_DIR/work/<name>;
# the checkout in $REPO is only read. macOS (BSD sed). Pushing is disabled in every copy.
# Usage: ADV_DIR=<empty scratch dir> REPO=<verigraph checkout> gate-g0-adversarial.sh all|<check>...
set -u
S=${ADV_DIR:?set ADV_DIR to an empty scratch directory}
REAL=${REPO:?set REPO to the verigraph checkout}
BASE=$S/base
STUB=$S/stub
OUT=$S/results
mkdir -p "$OUT" "$STUB" "$S/work"

# Base copy, made once, with pushing disabled so no check can ever push to GitHub.
if [ ! -d "$BASE" ]; then
  cp -R "$REAL" "$BASE"
  git -C "$BASE" remote set-url --push origin file:///dev/null/push-disabled
fi

# Stub gh, for CI conclusions of commits that cannot be pushed: prints $GH_STUB_OUTPUT the
# way `gh run list --jq '.[].conclusion'` prints conclusions, and logs the gate's arguments.
cat >"$STUB/gh" <<'EOF'
#!/bin/sh
echo "$*" >>"${GH_STUB_LOG:-/dev/null}"
printf '%b\n' "$GH_STUB_OUTPUT"
EOF
chmod +x "$STUB/gh"

fresh() { rm -rf "$S/work/$1"; cp -R "$BASE" "$S/work/$1"; cd "$S/work/$1" || exit 1; }
commit() { git -c user.name=reviewer -c user.email=reviewer@invalid commit -q "$@"; }

# gate <name> [stub-output]: run make gate-g0 (with the gh stub when an output is given),
# then print the exit code and the three criterion rows of the report it wrote.
gate() {
  name=$1; shift
  if [ $# -gt 0 ]; then
    GH_STUB_OUTPUT=$1 GH_STUB_LOG="$OUT/$name.gh-args" PATH="$STUB:$PATH" make gate-g0 >"$OUT/$name.out" 2>&1
  else
    make gate-g0 >"$OUT/$name.out" 2>&1
  fi
  code=$?
  echo "$name $code" >>"$OUT/summary.txt"
  cp reports/gate-g0.md "$OUT/$name.report.md" 2>/dev/null
  echo "### $name: make gate-g0 exit $code"
  sed -n 's/^wrote /stdout: wrote /p' "$OUT/$name.out"
  grep -E '^\| [123] \|' reports/gate-g0.md 2>/dev/null | cut -c1-260
}

c00_control() { fresh c00-control; gate c00-control; }

c01_clippy() {
  fresh c01-clippy
  printf '\n/// Adversarial: trips clippy::needless_return.\npub fn needless(x: i32) -> i32 {\n    return x;\n}\n' >>crates/vg-mesh/src/lib.rs
  cargo fmt --all --check && echo "fmt still clean, so only clippy can fail"
  gate c01-clippy
  grep -A3 'cargo clippy' "$OUT/c01-clippy.report.md" | head -4
}

c02_fmt() {
  fresh c02-fmt
  printf '\npub fn ugly( ) {}\n' >>crates/vg-types/src/lib.rs
  gate c02-fmt
  grep 'cargo fmt' "$OUT/c02-fmt.report.md"
}

c03_test() {
  fresh c03-test
  cat >>crates/vg-core/src/lib.rs <<'EOF'

#[cfg(test)]
mod adversarial {
    #[test]
    fn fails() {
        panic!("adversarial failing test");
    }
}
EOF
  cargo fmt --all --check && echo "fmt still clean"
  gate c03-test
  grep 'cargo test' "$OUT/c03-test.report.md"
}

c04_11_crates() {
  fresh c04-11-crates
  rm -rf crates/vg-omnilift
  sed -i '' '/^vg-omnilift = /d' crates/vg-cli/Cargo.toml
  cargo generate-lockfile --offline -q
  ls -d crates/*/ | wc -l
  gate c04-11-crates
}

c05_13_crates() {
  fresh c05-13-crates
  mkdir -p crates/vg-extra/src
  printf '[package]\nname = "vg-extra"\nversion.workspace = true\nedition.workspace = true\nrust-version.workspace = true\npublish.workspace = true\n' >crates/vg-extra/Cargo.toml
  echo '//! Adversarial thirteenth crate.' >crates/vg-extra/src/lib.rs
  cargo generate-lockfile --offline -q
  ls -d crates/*/ | wc -l
  gate c05-13-crates
}

c06_renamed_crate() {
  fresh c06-renamed-crate
  git mv crates/vg-mesh crates/vg-impostor
  sed -i '' 's/^name = "vg-mesh"/name = "vg-impostor"/' crates/vg-impostor/Cargo.toml
  sed -i '' 's|^vg-mesh = { path = "../vg-mesh" }|vg-impostor = { path = "../vg-impostor" }|' crates/vg-cli/Cargo.toml
  cargo generate-lockfile --offline -q
  git add -A && commit -m "adversarial: vg-mesh replaced by vg-impostor"
  ls crates | tr '\n' ' '; echo
  gate c06-renamed-crate success
}

c06b_empty_crate_dir() {
  fresh c06b-empty-crate-dir
  rm -rf crates/vg-omnilift && mkdir crates/vg-omnilift
  sed -i '' '/^vg-omnilift = /d' crates/vg-cli/Cargo.toml
  gate c06b-empty-crate-dir
  grep -h 'failed to load manifest' "$OUT/c06b-empty-crate-dir.report.md" | head -1
}

c07_ci_failure() { fresh c07-ci-failure; gate c07-ci-failure failure; cat "$OUT/c07-ci-failure.gh-args"; }
c08_ci_mixed() { fresh c08-ci-mixed; gate c08-ci-mixed 'success\nfailure'; }
c09_ci_two_green() { fresh c09-ci-two-green; gate c09-ci-two-green 'success\nsuccess'; }

c10_ci_not_run() {
  fresh c10-ci-not-run
  echo >>README.md
  commit -am "adversarial: local commit, never pushed"
  git rev-parse HEAD
  gate c10-ci-not-run
}

c11_ci_no_macos() {
  fresh c11-ci-no-macos
  sed -i '' '/name: macos-arm64/,/os: macos-15/d' .github/workflows/ci.yml
  commit -am "adversarial: CI matrix without the macOS job"
  grep -c macos .github/workflows/ci.yml
  sed -n '/matrix:/,/name: \${{/p' .github/workflows/ci.yml
  gate c11-ci-no-macos success
}

c12_doc_missing() { fresh c12-doc-missing; rm docs/05-contracts-verification.md; gate c12-doc-missing; }
c13_doc_empty() { fresh c13-doc-empty; : >docs/11-errata.md; gate c13-doc-empty; }
c14_doc_one_byte() { fresh c14-doc-one-byte; printf x >docs/03-vg-sym.md; wc -c <docs/03-vg-sym.md; gate c14-doc-one-byte; }

c15_track_deleted() {
  fresh c15-track-deleted
  rm docs/research/R-07-learnable-parameters.md
  ls docs/research/R-*.md | wc -l
  gate c15-track-deleted
}

c15b_two_tracks_deleted() {
  fresh c15b-two-tracks-deleted
  rm docs/research/R-07-learnable-parameters.md docs/research/R-13-dynamic-language-lifting.md
  gate c15b-two-tracks-deleted
}

c16_tracks_emptied() {
  fresh c16-tracks-emptied
  for f in docs/research/R-[0-9][0-9]-*.md; do : >"$f"; done
  wc -c docs/research/R-*.md | tail -1
  gate c16-tracks-emptied
}

c17_doc_not_in_repo() {
  fresh c17-doc-not-in-repo
  git rm -q --cached docs/10-bootstrap.md
  commit -m "adversarial: docs/10 removed from the repository, left on disk"
  test -s docs/10-bootstrap.md && echo "docs/10-bootstrap.md is on disk"
  git cat-file -e HEAD:docs/10-bootstrap.md 2>/dev/null || echo "docs/10-bootstrap.md is not in HEAD"
  gate c17-doc-not-in-repo success
}

c18_new_spec_docs_deleted() {
  fresh c18-new-spec-docs-deleted
  rm docs/13-semantics.md docs/14-opcode-reference.md docs/15-agent-interface.md docs/16-conformance-and-gates.md
  rm docs/adr/000[2-9]-*.md docs/adr/0010-*.md
  ls docs/1[3-6]-*.md docs/adr 2>&1 | tr '\n' ' '; echo
  gate c18-new-spec-docs-deleted
}

c19_report_readonly() {
  fresh c19-report-readonly
  chmod a-w reports/gate-g0.md
  gate c19-report-readonly
  grep -i 'denied' "$OUT/c19-report-readonly.out"
  cmp -s reports/gate-g0.md "$REAL/reports/gate-g0.md" && echo "report on disk is unchanged: the committed one, for commit $(sed -n 's/.*for commit \([0-9a-f]\{7\}\).*/\1/p' reports/gate-g0.md)"
}

c19b_reports_dir_missing() {
  fresh c19b-reports-dir-missing
  rm -rf reports
  gate c19b-reports-dir-missing
  grep -i 'no such' "$OUT/c19b-reports-dir-missing.out"
  ls reports 2>&1
}

c20_unimplemented_gates() {
  fresh c20-unimplemented-gates
  for g in 1 2 3 4 5 6 7 8; do
    make gate-g$g >"$OUT/c20-g$g.out" 2>&1; code=$?
    echo "c20-gate-g$g $code" >>"$OUT/summary.txt"
    echo "make gate-g$g exit $code: $(head -n1 "$OUT/c20-g$g.out")"
  done
  ls reports
}

c21_determinism() {
  fresh c21-determinism
  make gate-g0 >/dev/null 2>&1; echo "run 1 exit $?"; cp reports/gate-g0.md "$OUT/c21-run1.md"
  make gate-g0 >/dev/null 2>&1; echo "run 2 exit $?"; cp reports/gate-g0.md "$OUT/c21-run2.md"
  if cmp "$OUT/c21-run1.md" "$OUT/c21-run2.md"; then echo "byte-identical reports"; echo "c21-determinism identical" >>"$OUT/summary.txt"; fi
}

c22_clippy_alias() {
  fresh c22-clippy-alias
  printf '\n/// Adversarial: trips clippy::needless_return.\npub fn needless(x: i32) -> i32 {\n    return x;\n}\n' >>crates/vg-mesh/src/lib.rs
  mkdir -p .cargo
  printf '[alias]\nclippy = ["test", "--no-run"]\n' >.cargo/config.toml
  gate c22-clippy-alias
  RUSTFLAGS="-D warnings" cargo clippy --workspace --all-targets --locked -- -D warnings 2>&1 | grep 'user-defined alias'
}

c23_fresh_clone() {
  rm -rf "$S/work/c23-fresh-clone"
  git clone -q "$REAL" "$S/work/c23-fresh-clone"
  cd "$S/work/c23-fresh-clone" || exit 1
  git remote set-url origin "$(git -C "$REAL" remote get-url origin)"
  git remote set-url --push origin file:///dev/null/push-disabled
  git rev-parse HEAD; git status --porcelain | wc -l
  gate c23-fresh-clone
}

checks="c00_control c01_clippy c02_fmt c03_test c04_11_crates c05_13_crates c06_renamed_crate
c06b_empty_crate_dir c07_ci_failure c08_ci_mixed c09_ci_two_green c10_ci_not_run c11_ci_no_macos
c12_doc_missing c13_doc_empty c14_doc_one_byte c15_track_deleted c15b_two_tracks_deleted
c16_tracks_emptied c17_doc_not_in_repo c18_new_spec_docs_deleted c19_report_readonly
c19b_reports_dir_missing c20_unimplemented_gates c21_determinism c22_clippy_alias c23_fresh_clone"
[ "${1:-}" = all ] && set -- $checks
for c in "$@"; do echo; echo "==== $c"; ( "$c" ) 2>&1; done
```

</details>
