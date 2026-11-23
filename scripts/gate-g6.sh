#!/bin/sh
# make gate-g6: the G6 criterion of docs/16 (telemetry); writes reports/gate-g6.md.
GATE=g6 TITLE="Telemetry"
. "$(dirname "$0")/gate-common.sh"
gate_start
c=met
run seeded cargo test -p vg-telemetry --locked --test seeded_g6 -- --nocapture || c="not met"
n=$(sed -n 's/^G6 small_slices=\([0-9]*\)\/60$/\1/p' "$work/seeded.log" | tail -n 1)
[ "${n:-0}" -ge 48 ] || c="not met"
row "On the 60 seeded bugs, the failing slice has 5 ops or fewer on at least 48" "$c" "${n:-no count} of 60 slices have 5 ops or fewer (seeded_g6)"
gate_finish
