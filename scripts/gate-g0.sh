#!/bin/sh
# make gate-g0: check the G0 criteria of ROADMAP.md for commit HEAD and write reports/gate-g0.md.
# Exits 0 only when every criterion holds and the report is written. Output has no timestamps,
# so reruns on the same tree and host give the same report.
set -u
cd "$(dirname "$0")/.."
export RUSTFLAGS="-D warnings" RUSTDOCFLAGS="-D warnings" CARGO_TERM_COLOR=never

report=reports/gate-g0.md
log=$(mktemp)
trap 'rm -f "$log"' EXIT
fail=0
evidence=""
head=$(git rev-parse --verify -q HEAD 2>/dev/null)

# run <label> <command...>: run a command, record its exit code and, on failure, its last lines.
# A cargo alias named clippy or fmt (in .cargo/config.toml or .cargo/config, say) runs something
# else in place of the tool; cargo warns when it does, and the run then counts as failed.
run() {
  label=$1; shift
  "$@" >"$log" 2>&1
  code=$?
  alias=$(grep -m 1 'user-defined alias' "$log")
  if [ $code = 0 ] && [ -z "$alias" ]; then
    evidence="$evidence
- \`$*\`: exit 0"
    return 0
  fi
  evidence="$evidence
- \`$*\`: exit $code${alias:+; cargo: $alias}. Last lines:
\`\`\`text
$(tail -n 15 "$log")
\`\`\`"
  return 1
}

# Criterion 1, local half: this tree is HEAD, holds the 12 crates of docs/01-architecture.md, and
# builds and passes fmt, clippy and tests on this host.
host=$(rustc -vV | sed -n 's/^host: //p')
c1=met
# Local results stand for HEAD only when nothing differs from it but this gate's own report.
# git status misses edits to files marked assume-unchanged or skip-worktree; ls-files -v shows them.
changed=$({ git status --porcelain --untracked-files=all; git ls-files -v | grep -v '^H '; } 2>&1 |
  grep -v '^.. reports/gate-g0\.md$')
if [ -z "$changed" ]; then
  tree="the working tree is HEAD"
else
  c1="not met"
  n=$(printf '%s\n' "$changed" | wc -l | tr -d ' ')
  tree="the working tree is not HEAD: \`git status\` and \`git ls-files -v\` list $(printf '%s\n' "$changed" | head -n 5 | sed 's/.*/`&`/' | paste -sd ' ' -)"
  [ "$n" -le 5 ] || tree="$tree and $((n - 5)) more"
fi
run fmt cargo fmt --all --check || c1="not met"
run clippy cargo clippy --workspace --all-targets --locked -- -D warnings || c1="not met"
run test cargo test --workspace --locked || c1="not met"
# In cargo's JSON a package object starts with its name, then its version; a dependency object
# has its source second.
# shortcut: a text match, so name and version keys in a [package.metadata] table could stand in for a missing crate; parse the JSON once the toolchain has a parser.
expected="vg-cli vg-codegen vg-core vg-decode vg-interp vg-mesh vg-omnilift vg-rt vg-smt vg-store vg-telemetry vg-types"
crates=$(cargo metadata --no-deps --format-version 1 --locked 2>/dev/null |
  grep -o '{"name":"[^"]*","version":"' | cut -d'"' -f4 | LC_ALL=C sort | paste -sd ' ' -)
if [ "$crates" = "$expected" ]; then
  crates="the 12 crates of docs/01-architecture.md"
else
  c1="not met"
  crates="crates \`${crates:-none}\`, not the 12 of docs/01-architecture.md (\`$expected\`)"
fi

# Criterion 1, remote half, and criterion 2: CI ran on this commit, and every run succeeded with
# both jobs, linux-x86_64 and macos-arm64. Each job checks its host triple (ci.yml).
c2="not met"
if [ -z "$head" ]; then
  ci="not run: this directory is not a git repository, so there is no commit for CI to build."
elif ! git remote get-url origin >/dev/null 2>&1; then
  ci="not run: the repository has no \`origin\` remote, so CI has never run."
elif ! runs=$(gh run list --workflow ci.yml --commit "$head" --json databaseId,event,conclusion \
  --jq '.[] | "\(.databaseId) \(.event) \(.conclusion)"' 2>&1) || [ -z "$runs" ]; then
  ci="workflow \`ci\` for commit $head: \`$(printf '%s' "${runs:-no run found}" | tr '\n' ' ')\`."
else
  c2=met
  ci="every run of workflow \`ci\` for commit $head must succeed, with jobs linux-x86_64 and macos-arm64:"
  while read -r id event conclusion; do
    jobs=$(gh run view "$id" --json jobs --jq '(any(.jobs[]; .name == "linux-x86_64" and .conclusion == "success")
      and any(.jobs[]; .name == "macos-arm64" and .conclusion == "success")) as $ok
      | "\(if $ok then "ok" else "NOT OK" end) (\([.jobs[] | "\(.name) \(.conclusion)"] | join(", ")))"' </dev/null 2>&1)
    case "$conclusion $jobs" in "success ok "*) ;; *) c2="not met" ;; esac
    ci="$ci run $id ($event) $conclusion, jobs $(printf '%s' "$jobs" | tr '\n' ' ');"
  done <<EOF
$runs
EOF
  ci="${ci%;}."
fi

# Criterion 3: the spec docs are in the repo: each is committed in HEAD and starts with the first
# line below. The research tracks are R-01 to R-14, from the table in docs/research/README.md.
c3=met
missing=""
docs=0
while IFS='|' read -r f first; do
  docs=$((docs + 1))
  [ "$(git cat-file blob "HEAD:$f" 2>/dev/null | head -n 1)" = "$first" ] || { c3="not met"; missing="$missing $f"; }
done <<'EOF'
AGENTS.md|# AGENTS.md — Build VeriGraph (VG-IR)
ROADMAP.md|# ROADMAP
PROGRESS.md|# PROGRESS
docs/00-vision.md|# 00 · Vision
docs/01-architecture.md|# 01 · Architecture
docs/02-vg-ir-format.md|# 02 · VG-IR format
docs/03-vg-sym.md|# 03 · VG-Sym, the text projection
docs/04-types-effects.md|# 04 · Types, effects and capabilities
docs/05-contracts-verification.md|# 05 · Contracts and verification
docs/06-codegen-runtime.md|# 06 · Codegen and runtime
docs/07-generation-loop.md|# 07 · AI generation loop
docs/08-omnilift.md|# 08 · OmniLift
docs/09-vg-mesh.md|# 09 · VG-Mesh
docs/10-bootstrap.md|# 10 · Self-hosting bootstrap
docs/11-errata.md|# 11 · Errata of the source spec
docs/12-risks-and-open-questions.md|# 12 · Risks, budget and open questions
docs/source/original-spec.md|> **Historical source document.** This is the original VeriGraph spec as first written. It contains 27 technical errors, listed with their fixes in docs/11-errata.md. Where it disagrees with docs/, docs/ wins. Its "built-in" hashes are placeholders, not real hashes.
docs/research/PROTOCOL.md|# Research protocol
docs/research/README.md|# Research tracks
docs/research/R-01-smt-guided-decoding.md|# R-01 · SMT-guided constrained decoding
docs/research/R-02-semantic-convergence.md|# R-02 · Semantic convergence
docs/research/R-03-lifting-legacy-code.md|# R-03 · Lifting legacy code
docs/research/R-04-contract-invariant-synthesis.md|# R-04 · Contract and invariant synthesis
docs/research/R-05-proof-certificates.md|# R-05 · Proof certificates
docs/research/R-06-resource-latency-contracts.md|# R-06 · Resource and latency contracts
docs/research/R-07-learnable-parameters.md|# R-07 · Learnable parameters under contracts
docs/research/R-08-counterfactual-telemetry.md|# R-08 · Counterfactual telemetry
docs/research/R-09-token-efficiency.md|# R-09 · Token efficiency and model competence
docs/research/R-10-floating-point-verification.md|# R-10 · Floating-point verification
docs/research/R-11-self-hosting-feasibility.md|# R-11 · Self-hosting feasibility
docs/research/R-12-mesh-at-scale.md|# R-12 · VG-Mesh at global scale
docs/research/R-13-dynamic-language-lifting.md|# R-13 · Dynamic-language lifting
docs/research/R-14-intent-validation.md|# R-14 · Intent validation
EOF

# P0 deliverables (ROADMAP.md P0.1-P0.6); not gate criteria, listed for the reviewer.
deliverables=""
for p in rust-toolchain.toml toolchain.lock scripts/check-toolchain.sh Makefile .github/workflows/ci.yml \
  tests/corpus/good tests/corpus/bad tests/corpus/alpha benchmarks/kernels benchmarks/synthesis \
  reports docs/adr docs/interface-requests docs/adr/0001-format-encodings.md; do
  if [ -e "$p" ]; then deliverables="$deliverables
- present: \`$p\`"; else deliverables="$deliverables
- MISSING: \`$p\`"; fi
done

toolchain=$(./scripts/check-toolchain.sh 2>&1)

for c in "$c1" "$c2" "$c3"; do [ "$c" = met ] || fail=1; done
if [ $fail = 0 ]; then verdict="every criterion holds; awaiting the owner's approval"; else verdict="FAILED: at least one criterion is not met"; fi

cat >"$report" <<EOF || { echo "gate-g0: could not write $report" >&2; exit 1; }
# Gate G0 · Scaffold

Generated by \`make gate-g0\` (scripts/gate-g0.sh). Result: **$verdict**.
A gate passes only when this script exits 0 and the owner approves this report.

| # | Criterion (ROADMAP.md) | Status | Evidence |
| --- | --- | --- | --- |
| 1 | The workspace builds on macOS ARM64 and Linux x86_64 | local: $c1; CI: $c2 | This host is \`$host\`: $tree, with $crates; fmt, clippy and tests below. Both targets are built only in CI (row 2). |
| 2 | CI is green | $c2 | $ci |
| 3 | The spec docs are in the repo | $c3 | $docs docs checked in commit $head by their first line: AGENTS.md, ROADMAP.md, PROGRESS.md, docs 00-12, the source spec, the research protocol and README, and research tracks R-01 to R-14.${missing:+ Missing or wrong:$missing} |

## Commands run on this host

Host \`$host\`, $(rustc --version), DEVELOPER_DIR=\`${DEVELOPER_DIR:-unset}\`.
$evidence

## Toolchain on this host (scripts/check-toolchain.sh, informational for G0)

\`\`\`text
$toolchain
\`\`\`

## P0 deliverables
$deliverables
EOF

echo "wrote $report: $verdict"
exit $fail
