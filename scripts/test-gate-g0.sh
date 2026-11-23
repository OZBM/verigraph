#!/bin/sh
# Regression test for scripts/gate-g0.sh: the adversarial checks of the independent G0 review
# (2026-10-29) and checks for its fixes. Each check copies a clone of HEAD, breaks one thing, runs
# `make gate-g0` with a stub gh in place of GitHub, then checks make's exit code (2 when the gate
# fails) and the evidence. Nothing is pushed, and GitHub is never called.
# Usage: scripts/test-gate-g0.sh [check...]; TEST_DIR=<dir> keeps the copies and outputs. Needs jq.
set -u
repo=$(cd "$(dirname "$0")/.." && pwd)
dir=${TEST_DIR:-$(mktemp -d)}
base=$dir/base
out=$dir/results
green="push success linux-x86_64=success macos-arm64=success"
red="push failure linux-x86_64=failure macos-arm64=failure"
checks="c00 c01 c02 c03 c04 c05 c06 c06b c07 c08 c09 c10 c11 c11b c12 c13 c14 c15 c15b c16 c17
c18 c19 c19b c20 c21 c22 c22b c23 c24 c24b c25 c25b"
command -v jq >/dev/null || { echo "test-gate-g0: the stub gh needs jq" >&2; exit 1; }
unset CARGO_TARGET_DIR
mkdir -p "$out" "$dir/stub" "$dir/work" || exit 1
: >"$dir/failures"

# The commit under test, cloned once, with pushing disabled.
if [ ! -d "$base" ]; then
  git clone -q --no-hardlinks "$repo" "$base" &&
    git -C "$base" remote set-url --push origin file:///dev/null/push-disabled || exit 1
fi

# Stub gh: answers `run list` and `run view` from $GH_STUB_RUNS, a JSON array of the CI runs GitHub
# would report, each with its jobs. --json keeps the named fields and --jq filters them, as in gh;
# any other query fails. Every call is logged to $GH_STUB_LOG.
cat >"$dir/stub/gh" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >>"$GH_STUB_LOG"
query="$1 $2"; shift 2
id= commit= fields= filter= workflow=
while [ $# -gt 0 ]; do
  case $1 in
    --workflow) workflow=$2; shift ;;
    --commit) commit=$2; shift ;;
    --json) fields=$2; shift ;;
    --jq) filter=$2; shift ;;
    -*) echo "stub gh: unsupported flag $1" >&2; exit 1 ;;
    *) id=$1 ;;
  esac
  shift
done
case "$query/$workflow" in
  "run list/ci.yml") select='[.[] | select(.headSha == $commit) | del(.jobs) | keep]' ;;
  "run view/") select='first(.[] | select(.databaseId == ($id | tonumber)) | keep) // error("no run \($id)")' ;;
  *) echo "stub gh: unsupported query: $query $workflow" >&2; exit 1 ;;
esac
[ -n "$fields" ] && [ -n "$filter" ] || { echo "stub gh: needs --json and --jq" >&2; exit 1; }
keep='def keep: ($fields | split(",")) as $f
  | if ($f - keys) == [] then with_entries(select(.key | IN($f[]))) else error("unknown JSON field in \($f)") end;'
exec jq -r --arg commit "$commit" --arg id "$id" --arg fields "$fields" "$keep $select | ($filter)" "$GH_STUB_RUNS"
EOF
chmod +x "$dir/stub/gh"

# ci <sha> <run>...: the CI runs GitHub reports for commit <sha>, for the stub gh. A run is
# "<event> <conclusion> <job>=<conclusion>..."; run ids count up from the real run 40837987092.
ci() {
  sha=$1; shift
  printf '%s\n' "$@" | jq -R -n --arg sha "$sha" '[inputs | split(" ")] | to_entries | map({
    databaseId: (40837987092 + .key), event: .value[0], conclusion: .value[1], status: "completed",
    headSha: $sha, workflowName: "ci", jobs: [.value[2:][] | split("=") | {name: .[0], conclusion: .[1]}]})' >"$runs"
}

# fresh <check> <what>: start a check in a new copy of the clone.
fresh() {
  check=$1 runs=$out/$1.runs.json
  echo "== $1: $2"
  rm -rf "$dir/work/$1" "$runs" && cp -R "$base" "$dir/work/$1" && cd "$dir/work/$1" || exit 1
}
fail() { echo "   FAIL: $*"; echo "$check: $*" >>"$dir/failures"; }
commit() { git -c user.name=test -c user.email=test@invalid -c commit.gpgsign=false commit -q --no-verify "$@" || fail "git commit failed"; }
edit() { sed "$1" "$2" >"$2.new" && mv "$2.new" "$2"; }
lint() { printf '\n/// Trips clippy::needless_return.\npub fn needless(x: i32) -> i32 {\n    return x;\n}\n'; }

# gate <exit>: run make gate-g0 here with the stub gh, CI green on HEAD unless the check set other
# runs; make must exit with <exit>.
gate() {
  [ -f "$runs" ] || ci "$(git rev-parse HEAD)" "$green"
  GH_STUB_RUNS=$runs GH_STUB_LOG=$out/$check.gh-args PATH="$dir/stub:$PATH" make gate-g0 >"$out/$check.out" 2>&1
  code=$?
  echo "   make gate-g0: exit $code"
  cp reports/gate-g0.md "$out/$check.report.md" 2>/dev/null || : >"$out/$check.report.md"
  [ "$code" = "$1" ] || fail "make gate-g0 exited $code, expected $1"
}
# row <n> <text>: row <n> of the report's criteria table holds <text>.
row() { grep "^| $1 |" "$out/$check.report.md" | grep -qF -- "$2" || fail "report row $1 lacks: $2"; }
has() { grep -qF -- "$1" "$out/$check.report.md" || fail "report lacks: $1"; }
says() { grep -qF -- "$1" "$out/$check.out" || fail "output lacks: $1"; }
lacks() { ! grep -qF -- "$1" "$out/$check.out" || fail "output has: $1"; }
# oneline: the CI row is a single line of the table (B7).
oneline() { grep -A1 '^| 2 |' "$out/$check.report.md" | tail -n 1 | grep -q '^| 3 |' || fail "row 2 spans several lines"; }

c00() {
  fresh c00 "control: the commit as it is"
  gate 0; says "every criterion holds"; row 1 "local: met; CI: met"; row 1 "the working tree is HEAD"
  row 1 "the 12 crates of docs/01-architecture.md"; row 3 "| met |"
  row 2 "run 40837987092 (push) success, jobs ok (linux-x86_64 success, macos-arm64 success)."
}
c01() {
  fresh c01 "B4: a clippy lint in vg-mesh, fmt still clean, committed"
  lint >>crates/vg-mesh/src/lib.rs && commit -am "test: clippy::needless_return"
  ci "$(git rev-parse HEAD)" "$red"
  gate 2; row 1 "local: not met"; has '`cargo clippy --workspace --all-targets --locked -- -D warnings`: exit 101'
}
c02() {
  fresh c02 "B4: a rustfmt violation in vg-types, committed"
  printf '\npub fn ugly( ) {}\n' >>crates/vg-types/src/lib.rs && commit -am "test: unformatted"
  ci "$(git rev-parse HEAD)" "$red"
  gate 2; row 1 "local: not met"; has '`cargo fmt --all --check`: exit 1'
}
c03() {
  fresh c03 "B4: a failing unit test in vg-core, committed"
  printf '\n#[cfg(test)]\nmod failing {\n    #[test]\n    fn fails() {\n        panic!("failing test");\n    }\n}\n' >>crates/vg-core/src/lib.rs
  commit -am "test: a failing test"
  ci "$(git rev-parse HEAD)" "$red"
  gate 2; row 1 "local: not met"; has '`cargo test --workspace --locked`: exit 101'
}
c04() {
  fresh c04 "G-1: 11 crates, vg-omnilift removed and the lock regenerated, committed"
  rm -rf crates/vg-omnilift && edit '/^vg-omnilift = /d' crates/vg-cli/Cargo.toml && cargo generate-lockfile --offline -q
  git add -A && commit -m "test: no vg-omnilift"
  gate 2; row 1 "local: not met"; row 1 "not the 12 of docs/01-architecture.md"
}
c05() {
  fresh c05 "G-1: 13 crates, vg-extra added, committed"
  mkdir -p crates/vg-extra/src && echo '//! A thirteenth crate.' >crates/vg-extra/src/lib.rs
  printf '[package]\nname = "vg-extra"\nversion.workspace = true\nedition.workspace = true\nrust-version.workspace = true\npublish.workspace = true\n' >crates/vg-extra/Cargo.toml
  cargo generate-lockfile --offline -q && git add -A && commit -m "test: vg-extra"
  gate 2; row 1 "local: not met"; row 1 "vg-extra"
}
c06() {
  fresh c06 "G-1: vg-mesh replaced by vg-impostor, still 12 crates, committed"
  git mv crates/vg-mesh crates/vg-impostor
  edit 's/^name = "vg-mesh"/name = "vg-impostor"/' crates/vg-impostor/Cargo.toml
  edit 's|^vg-mesh = { path = "../vg-mesh" }|vg-impostor = { path = "../vg-impostor" }|' crates/vg-cli/Cargo.toml
  cargo generate-lockfile --offline -q && git add -A && commit -m "test: vg-impostor"
  gate 2; row 1 "local: not met"; row 1 "vg-impostor"
}
c06b() {
  fresh c06b "G-1: vg-omnilift left as an empty directory, which git does not keep"
  rm -rf crates/vg-omnilift && edit '/^vg-omnilift = /d' crates/vg-cli/Cargo.toml && cargo generate-lockfile --offline -q
  git add -A && commit -m "test: no vg-omnilift" && mkdir crates/vg-omnilift
  gate 2; row 1 "local: not met"; has "failed to load manifest"
}
c07() {
  fresh c07 "B1: CI failed on macOS"
  ci "$(git rev-parse HEAD)" "push failure linux-x86_64=success macos-arm64=failure"
  gate 2; row 2 "| not met |"; row 2 "macos-arm64 failure"
}
c08() {
  fresh c08 "B7: two runs for HEAD, push green and pull_request red"
  ci "$(git rev-parse HEAD)" "$green" "pull_request failure linux-x86_64=success macos-arm64=failure"
  gate 2; row 2 "| not met |"; row 2 "(pull_request) failure"; oneline
}
c09() {
  fresh c09 "B7: two green runs for HEAD, push and pull_request"
  ci "$(git rev-parse HEAD)" "$green" "pull_request success linux-x86_64=success macos-arm64=success"
  gate 0; row 2 "(push) success"; row 2 "run 40837987093 (pull_request) success, jobs ok"; oneline
}
c10() {
  fresh c10 "a local commit that was never pushed: CI ran only on its parent"
  ci "$(git rev-parse HEAD)" "$green"
  echo >>README.md && commit -am "test: never pushed"
  gate 2; row 2 "no run found"
}
c11() {
  fresh c11 "B1: the macOS job deleted from ci.yml, committed; CI green on Linux alone"
  edit '/- name: macos-arm64/,/triple: aarch64-apple-darwin/d' .github/workflows/ci.yml && commit -am "test: no macOS job"
  ci "$(git rev-parse HEAD)" "push success linux-x86_64=success"
  gate 2; row 2 "| not met |"; row 2 "jobs NOT OK (linux-x86_64 success)"
}
c11b() {
  fresh c11b "B1: the run succeeded but the macOS job was skipped"
  ci "$(git rev-parse HEAD)" "push success linux-x86_64=success macos-arm64=skipped"
  gate 2; row 2 "| not met |"; row 2 "macos-arm64 skipped"
}
c12() {
  fresh c12 "B2: docs/05 deleted, committed"
  git rm -q docs/05-contracts-verification.md && commit -m "test: no docs/05"
  gate 2; row 1 "local: met"; row 3 "docs/05-contracts-verification.md"
}
c13() {
  fresh c13 "B2: docs/11 emptied, committed"
  : >docs/11-errata.md && commit -am "test: empty docs/11"
  gate 2; row 1 "local: met"; row 3 "docs/11-errata.md"
}
c14() {
  fresh c14 "G-2: docs/03 replaced by one byte, committed"
  printf x >docs/03-vg-sym.md && commit -am "test: one-byte docs/03"
  gate 2; row 1 "local: met"; row 3 "docs/03-vg-sym.md"
}
c15() {
  fresh c15 "B3: research track R-07 deleted, 13 left, committed"
  git rm -q docs/research/R-07-learnable-parameters.md && commit -m "test: no R-07"
  gate 2; row 1 "local: met"; row 3 "docs/research/R-07-learnable-parameters.md"
}
c15b() {
  fresh c15b "B3: R-07 and R-13 deleted, committed"
  git rm -q docs/research/R-07-learnable-parameters.md docs/research/R-13-dynamic-language-lifting.md && commit -m "test: no R-07, R-13"
  gate 2; row 3 "docs/research/R-07-learnable-parameters.md"; row 3 "docs/research/R-13-dynamic-language-lifting.md"
}
c16() {
  fresh c16 "B3: all 14 research tracks emptied, committed"
  for f in docs/research/R-[0-9][0-9]-*.md; do : >"$f"; done && commit -am "test: empty tracks"
  gate 2; row 3 "docs/research/R-01-smt-guided-decoding.md"; row 3 "docs/research/R-14-intent-validation.md"
}
c17() {
  fresh c17 "B2: docs/10 removed from the repository but kept on disk, and ignored"
  git rm -q --cached docs/10-bootstrap.md && commit -m "test: docs/10 only on disk"
  mkdir -p .git/info && echo docs/10-bootstrap.md >>.git/info/exclude
  gate 2; row 1 "local: met"; row 3 "docs/10-bootstrap.md"
}
c18() {
  fresh c18 "G-3, out of scope: docs/13-16 and ADRs 0002-0010 deleted, committed"
  git rm -q docs/1[3-6]-*.md docs/adr/000[2-9]-*.md docs/adr/0010-*.md && commit -m "test: no docs/13-16, ADRs 0002-0010"
  gate 0
}
c19() {
  fresh c19 "B6: the report is read-only"
  [ "$(id -u)" != 0 ] || { echo "   skipped: root ignores file permissions"; return; }
  chmod a-w reports/gate-g0.md
  gate 2; says "could not write reports/gate-g0.md"; lacks "wrote reports/gate-g0.md"
}
c19b() {
  fresh c19b "B6: the reports directory is missing"
  rm -rf reports
  gate 2; says "could not write reports/gate-g0.md"; lacks "wrote reports/gate-g0.md"
}
c20() {
  fresh c20 "make gate-g1 to gate-g8 fail loudly and write no report"
  for g in 1 2 3 4 5 6 7 8; do
    make gate-g$g >"$out/c20-g$g.out" 2>&1; code=$?
    [ $code = 2 ] || fail "make gate-g$g exited $code, expected 2"
    grep -q "^gate-g$g: NOT IMPLEMENTED" "$out/c20-g$g.out" || fail "make gate-g$g does not say NOT IMPLEMENTED"
    [ ! -e reports/gate-g$g.md ] || fail "make gate-g$g wrote a report"
  done
}
c21() {
  fresh c21 "determinism: two runs on one tree; the second sees the report the first wrote"
  gate 0; cp reports/gate-g0.md "$out/c21.first.md"
  gate 0; cmp -s reports/gate-g0.md "$out/c21.first.md" || fail "the two reports differ"
}
c22() {
  fresh c22 "G-4: a clippy lint, and a clippy alias in .cargo/config.toml, committed; CI fooled too"
  lint >>crates/vg-mesh/src/lib.rs && mkdir .cargo && printf '[alias]\nclippy = ["test", "--no-run"]\n' >.cargo/config.toml
  git add -A && commit -m "test: clippy aliased away"
  gate 2; row 1 "local: not met"; has 'user-defined alias `clippy`'
}
c22b() {
  fresh c22b "G-4: a rustfmt violation, and an fmt alias in .cargo/config, committed; CI fooled too"
  printf '\npub fn ugly( ) {}\n' >>crates/vg-types/src/lib.rs && mkdir .cargo && printf '[alias]\nfmt = ["test", "--no-run", "--"]\n' >.cargo/config
  git add -A && commit -m "test: fmt aliased away"
  gate 2; row 1 "local: not met"; has 'user-defined alias `fmt`'
}
c23() {
  check=c23 runs=$out/c23.runs.json
  echo "== c23: a fresh clone of HEAD, tracked files only"
  rm -rf "$dir/work/c23" "$runs" && git clone -q --no-hardlinks "$repo" "$dir/work/c23" && cd "$dir/work/c23" &&
    git remote set-url --push origin file:///dev/null/push-disabled || exit 1
  gate 0
}
c24() {
  fresh c24 "B5: a source edit hidden from git status by assume-unchanged"
  echo '// local edit' >>crates/vg-core/src/lib.rs && git update-index --assume-unchanged crates/vg-core/src/lib.rs
  [ -z "$(git status --porcelain)" ] || fail "git status shows the edit"
  gate 2; row 1 "local: not met"; row 1 '`h crates/vg-core/src/lib.rs`'
}
c24b() {
  fresh c24b "B5: a source edit hidden from git status by skip-worktree"
  echo '// local edit' >>crates/vg-core/src/lib.rs && git update-index --skip-worktree crates/vg-core/src/lib.rs
  [ -z "$(git status --porcelain)" ] || fail "git status shows the edit"
  gate 2; row 1 "local: not met"; row 1 '`S crates/vg-core/src/lib.rs`'
}
c25() {
  fresh c25 "B5: an uncommitted source edit that passes fmt, clippy and tests"
  echo '// local edit' >>crates/vg-core/src/lib.rs
  gate 2; row 1 "local: not met"; row 1 '` M crates/vg-core/src/lib.rs`'
}
c25b() {
  fresh c25b "B5: an untracked file"
  echo note >notes.txt
  gate 2; row 1 "local: not met"; row 1 '`?? notes.txt`'
}

[ $# -gt 0 ] || set -- $checks
for c in "$@"; do
  case " $(echo $checks) " in *" $c "*) ;; *) echo "test-gate-g0: unknown check $c" >&2; exit 2 ;; esac
done
for c in "$@"; do ( "$c" ); rm -rf "$dir"/work/*/target; done
failed=$(wc -l <"$dir/failures" | tr -d ' ')
echo "$# checks, $failed failed expectations; copies and outputs in $dir"
[ "$failed" = 0 ]
