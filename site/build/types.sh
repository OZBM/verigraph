#!/bin/sh
set -eu
VG=${VG:-./target/release/vg}
WORK=${WORK:-target/site/types}
mkdir -p "$WORK" site/data

check_err() {
  eid=$1
  file=$2
  expect_line=$(head -n 1 "$file")
  tail -n +2 "$file" >"$WORK/$eid.noexpect.vgs"
  "$VG" check "$file" --root "$WORK" >"$WORK/$eid.check.json" || true
  jq -e '.diagnostics | length > 0' "$WORK/$eid.check.json" >/dev/null
  jq -n \
    --arg id "$eid" \
    --arg file "$file" \
    --arg expect_line "$expect_line" \
    --rawfile source "$WORK/$eid.noexpect.vgs" \
    --slurpfile check "$WORK/$eid.check.json" \
    '{id: $id, file: $file, expect_line: $expect_line, source_without_expect: $source, check: $check[0]}'
}

: >"$WORK/exercises.ndjson"
check_err "t0103" "tests/corpus/errors/t0103_lt_u_on_i32.vgs" >>"$WORK/exercises.ndjson"
check_err "t0115" "tests/corpus/errors/t0115_ok_no_annotation.vgs" >>"$WORK/exercises.ndjson"
check_err "e0101" "tests/corpus/errors/e0101_clock_in_pure.vgs" >>"$WORK/exercises.ndjson"
check_err "e0102" "tests/corpus/errors/e0102_eff_used_twice.vgs" >>"$WORK/exercises.ndjson"
check_err "e0104" "tests/corpus/errors/e0104_file_in_without_bit.vgs" >>"$WORK/exercises.ndjson"
check_err "t0120" "tests/corpus/errors/t0120_return_slice_of_local_array.vgs" >>"$WORK/exercises.ndjson"
jq -s '.' "$WORK/exercises.ndjson" >"$WORK/exercises.json"

"$VG" check tests/corpus/alpha/a45_file_read_write_chain/01.vgs --root "$WORK" >"$WORK/effect_chain_check.json"
err_count=$(find tests/corpus/errors -maxdepth 1 -name '*.vgs' | wc -l | tr -d ' ')

jq -S -n \
  --slurpfile exercises "$WORK/exercises.json" \
  --slurpfile eff_check "$WORK/effect_chain_check.json" \
  --rawfile eff_src "tests/corpus/alpha/a45_file_read_write_chain/01.vgs" \
  --argjson err_count "$err_count" \
  '{
    errors_corpus_count: $err_count,
    exercises: $exercises[0],
    effect_chain_check: $eff_check[0],
    effect_chain_source: $eff_src
  }' >site/data/types.json
