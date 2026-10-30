#!/bin/sh
# make gate-g1: check the G1 criteria of ROADMAP.md, as docs/16 makes them executable, and write
# reports/gate-g1.md. Exits 0 only when every criterion holds and the report is written.
# FUZZ_SECONDS (default 3600) sets each fuzz target's run; below 3600 the fuzz criterion is not met.
# Written to avoid the bugs that reports/gate-g0-review.md found in gate-g0.sh: exit codes are
# captured, not assumed (B4); the gate's inputs must be committed (B2, B5); a failed report write
# fails the gate (B6); a repository cargo alias cannot replace clippy, fmt or test (G-4).
set -u
cd "$(dirname "$0")/.."
export RUSTFLAGS="-D warnings" RUSTDOCFLAGS="-D warnings" CARGO_TERM_COLOR=never
FUZZ_SECONDS=${FUZZ_SECONDS:-3600}

report=reports/gate-g1.md
work=target/gate-g1-run
rm -rf "$work" target/gate-g1
mkdir -p "$work"
evidence=""

# run <log name> <command...>: run it, keep its whole output in $work/<name>.log, record the real
# exit code, and on failure the last lines. Returns the command's exit code.
run() {
  name=$1; shift
  "$@" >"$work/$name.log" 2>&1
  code=$?
  if [ "$code" -eq 0 ]; then
    evidence="$evidence
- \`$*\`: exit 0"
  else
    evidence="$evidence
- \`$*\`: exit $code. Last lines:
\`\`\`text
$(tail -n 15 "$work/$name.log")
\`\`\`"
  fi
  return "$code"
}

summary() { [ -f "target/gate-g1/$1" ] && cat "target/gate-g1/$1" || echo "no evidence: target/gate-g1/$1 was not written"; }

head=$(git rev-parse HEAD 2>/dev/null || echo "not a git repository")
inputs="crates fuzz tests tools examples scripts Cargo.toml Cargo.lock Makefile rust-toolchain.toml"
# shellcheck disable=SC2086
dirty=$(git status --porcelain -- $inputs 2>&1)
tree=met
if [ -n "$dirty" ]; then
  tree="not met"
  evidence="$evidence
- The gate's inputs differ from HEAD, so this run would not describe commit $head:
\`\`\`text
$dirty
\`\`\`"
fi
for cfg in .cargo/config .cargo/config.toml; do
  if [ -f "$cfg" ] && grep -Eq '^[[:space:]]*(clippy|fmt|test)[[:space:]]*=' "$cfg"; then
    tree="not met"
    evidence="$evidence
- \`$cfg\` defines a cargo alias for clippy, fmt or test, which could replace the real command."
  fi
done

# The workspace is green: fmt, clippy, every test. The tests write the law evidence to target/gate-g1.
green=met
run fmt cargo fmt --all --check || green="not met"
run clippy cargo clippy --workspace --all-targets --locked -- -D warnings || green="not met"
run test cargo test --workspace --locked || green="not met"

# Criterion 1: .vgs and .vgir round-trip byte-exact, idempotence, golden files.
c1=met
for f in print.txt asm.txt golden.txt generated.txt unravel.txt; do [ -f "target/gate-g1/$f" ] || c1="not met"; done
[ "$green" = met ] || c1="not met"

# Criterion 2: at least 50 alpha families hash identically under random renaming and reordering.
c2=met
families=$(sed -n 's/^\([0-9]*\) alpha families.*/\1/p' target/gate-g1/alpha.txt 2>/dev/null)
[ "${families:-0}" -ge 50 ] || c2="not met"
[ -f target/gate-g1/distinct.txt ] || c2="not met"
[ "$green" = met ] || c2="not met"

# Criterion 3: vg-core's parser agrees with tools/vgsym.lark on every file and lexical case.
c3=met
venv=target/venv
if [ ! -x "$venv/bin/python" ]; then
  python3 -m venv "$venv" >/dev/null 2>&1 && "$venv/bin/pip" install -q lark==1.3.1 >/dev/null 2>&1
fi
lark=$("$venv/bin/python" -c 'import lark; print(lark.__version__)' 2>&1)
run lark-parity "$venv/bin/python" tools/lark_parity.py --check || c3="not met"
[ -f target/gate-g1/parity.txt ] || c3="not met"

# Criterion 4: each fuzz target runs FUZZ_SECONDS without a crash, from seeds of every corpus file.
c4=met
fuzz_rows=""
# The gate's own seed directories, so no other fuzzing session can write into them mid-run.
if ! run seeds ./scripts/fuzz-seeds.sh "$work/seeds"; then
  c4="not met"
fi
for t in parse codec canon; do
  (cd fuzz && cargo +nightly fuzz run "$t" "../$work/seeds/$t" -- -max_total_time="$FUZZ_SECONDS" -print_final_stats=1 \
    >"../$work/fuzz-$t.log" 2>&1; echo $? >"../$work/fuzz-$t.code") &
done
wait
for t in parse codec canon; do
  code=$(cat "$work/fuzz-$t.code" 2>/dev/null || echo "missing")
  done_line=$(grep -E '^Done [0-9]+ runs in [0-9]+ second' "$work/fuzz-$t.log" | tail -n 1)
  secs=$(echo "$done_line" | sed -n 's/^Done [0-9]* runs in \([0-9]*\) second.*/\1/p')
  cov=$(grep -E '^#[0-9]+' "$work/fuzz-$t.log" | tail -n 1 | sed -n 's/.*cov: \([0-9]*\).*/\1/p')
  if [ "$code" != 0 ] || [ "${secs:-0}" -lt 3600 ]; then c4="not met"; fi
  fuzz_rows="$fuzz_rows
| \`$t\` | $code | ${done_line:-no final line} | ${cov:-?} |"
  if [ "$code" != 0 ]; then
    evidence="$evidence
- fuzz target \`$t\` exited $code. Last lines:
\`\`\`text
$(tail -n 20 "$work/fuzz-$t.log")
\`\`\`"
  fi
done

fail=0
for c in "$tree" "$green" "$c1" "$c2" "$c3" "$c4"; do [ "$c" = met ] || fail=1; done
if [ $fail = 0 ]; then verdict="every criterion holds; awaiting the owner's approval"; else verdict="FAILED: at least one criterion is not met"; fi

if ! cat >"$report" <<EOF
# Gate G1 · Core IR

Generated by \`make gate-g1\` (scripts/gate-g1.sh) for commit \`$head\`. Result: **$verdict**.
A gate passes only when this script exits 0 and the owner approves this report.

| # | Criterion (ROADMAP.md, docs/16) | Status | Evidence |
| --- | --- | --- | --- |
| 0 | The gate's inputs are committed, and the workspace is green (fmt, clippy, tests) | tree: $tree; green: $green | Commands below |
| 1 | \`.vgs\` and \`.vgir\` round-trip byte-exact; canonicalization is idempotent; golden files hold | $c1 | $(summary print.txt) $(summary asm.txt | tr '\n' ' ') $(summary golden.txt) |
| 2 | 50 or more alpha families hash identically under random renaming and reordering; near-misses hash differently | $c2 | $(summary alpha.txt) $(summary distinct.txt) $(summary unravel.txt) |
| 3 | vg-core's parser agrees with tools/vgsym.lark on every corpus file and lexical case | $c3 | $(summary parity.txt) Fixture checked against lark $lark. |
| 4 | Each fuzz target runs an hour without a crash | $c4 | $FUZZ_SECONDS s per target, table below |

The distinctness families (row 2, docs/16) wait on the owner's decision to add them to G1
(PROGRESS.md, questions); this gate already requires them.

## Fuzzing

Seeds: every VG-Sym file for \`parse\`, the .vgir of every file that assembles for \`codec\`, 32 fixed
seeds for \`canon\` (scripts/fuzz-seeds.sh). Run counts differ from run to run; crashes would not.

| Target | Exit | Final line | Coverage |
| --- | --- | --- | --- |$fuzz_rows

## Generated programs

$(summary generated.txt)

## Commands run on this host

Host \`$(rustc -vV | sed -n 's/^host: //p')\`, $(rustc --version); fuzzing with $(rustc +nightly --version 2>&1) and $(cargo fuzz --version 2>&1 | head -n 1).
$evidence
EOF
then
  echo "gate-g1: could not write $report" >&2
  exit 1
fi
echo "wrote $report: $verdict"
exit $fail
