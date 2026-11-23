#!/bin/sh
# make gate-g2: the G2 criteria of docs/16 (types and interpreter); writes reports/gate-g2.md.
GATE=g2 TITLE="Types and interpreter"
. "$(dirname "$0")/gate-common.sh"
gate_start

c=met
run errors cargo test -p vg-types --locked --test corpus_negative || c="not met"
test_ran errors types_bad_corpus_rejected_with_expected_codes || c="not met"
n=$(ls tests/corpus/errors/*.vgs | wc -l | tr -d ' ')
row "Every file in tests/corpus/errors ($n) is rejected with exactly its code and location" "$c" \
  "corpus_negative matches each file's \`// expect: error <code> <node> <port>\` line against vg-types' diagnostics"

c=met
run conformance cargo test -p vg-interp --locked --test conformance || c="not met"
test_ran conformance conformance_vectors_hold || c="not met"
row "Every conformance vector is bit-exact in vg-interp" "$c" "vg-interp conformance_vectors_hold"

# docs/16 names five capability escalations; each has a file in the errors corpus, checked above.
c=met
cases=""
for f in e0101_clock_in_pure e0101_call_effect_escalation c0108_fold_effectful_kernel e0102_eff_used_twice e0104_clock_without_bit; do
  if [ -f "tests/corpus/errors/$f.vgs" ]; then cases="$cases \`$f\`"; else c="not met"; cases="$cases \`$f\` (missing)"; fi
done
[ "$(test_ran errors types_bad_corpus_rejected_with_expected_codes && echo y)" = y ] || c="not met"
row "The capability escalations are rejected: an effect op in a pure node, a callee mask not contained, an effect in a kernel, an Eff token used twice, a handle without its bit" "$c" "$cases"

gate_finish
