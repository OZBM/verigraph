#!/bin/sh
# make gate-g7: the G7 criteria of docs/16 (VG-Mesh lite); writes reports/gate-g7.md.
GATE=g7 TITLE="VG-Mesh lite"
. "$(dirname "$0")/gate-common.sh"
gate_start
run g7 cargo test -p vg-mesh --release --locked --test g7 -- --include-ignored --nocapture
c=met
p99=$(sed -n 's/^G7 p99_us=\([0-9]*\) .*/\1/p' "$work/g7.log" | tail -n 1)
test_ran g7 g7_bench_100k || c="not met"
[ -n "$p99" ] && [ "$p99" -lt 5000 ] || c="not met"
row "Indexed queries under 5 ms at p99 on 100,000 nodes" "$c" "$(grep -E '^G7 p99_us=' "$work/g7.log" | tail -n 1)"
c=met
cases=""
for t in tamper_flipped_vgir_byte tamper_wrong_claimed_hash tamper_forged_signature tamper_missing_signature \
  tamper_understated_effect_mask tamper_reordered_log tamper_truncated_log tamper_replayed_submission; do
  if test_ran g7 "$t"; then cases="$cases $t"; else c="not met"; cases="$cases $t(FAILED)"; fi
done
row "Every tampered submission is rejected" "$c" "$cases"
gate_finish
