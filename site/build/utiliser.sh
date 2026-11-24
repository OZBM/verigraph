#!/bin/sh
set -eu
VG=${VG:-./target/release/vg}
WORK=${WORK:-target/site/utiliser}
rm -rf "$WORK"
mkdir -p "$WORK" site/data

"$VG" 2>"$WORK/usage.txt" || true
test -s "$WORK/usage.txt"

"$VG" check examples/quickstart.vgs >"$WORK/check_quick.json"
jq -e '.ok == true' "$WORK/check_quick.json" >/dev/null

"$VG" verify examples/quickstart.vgs --mode edit --root "$WORK" --detail 1 >"$WORK/verify_quick.json"
jq -e '.ok == true' "$WORK/verify_quick.json" >/dev/null

"$VG" run examples/quickstart.vgs @add_sat '{"a": 200, "b": 100}' >"$WORK/run_add_sat.json"
jq -e '.ok == true' "$WORK/run_add_sat.json" >/dev/null

"$VG" run examples/quickstart.vgs @soft_knee '{"x": "3e38", "knee": "0.5"}' >"$WORK/run_soft_knee.json"
jq -e '.ok == true' "$WORK/run_soft_knee.json" >/dev/null

"$VG" build examples/quickstart.vgs @soft_knee --root "$WORK" -o "$WORK/bin" >"$WORK/build_soft_knee.json"
jq -e '.ok == true and .verified == true' "$WORK/build_soft_knee.json" >/dev/null

rc_verify_draft=0
"$VG" verify examples/soft_knee_draft.vgs --mode edit --root "$WORK" --detail 2 >"$WORK/verify_draft.json" || rc_verify_draft=$?
jq -e '.ok == false' "$WORK/verify_draft.json" >/dev/null

rc_explain_draft=0
"$VG" explain examples/soft_knee_draft.vgs @soft_knee/POST.0 --root "$WORK" >"$WORK/explain_draft.json" || rc_explain_draft=$?

rc_run_draft=0
"$VG" run examples/soft_knee_draft.vgs @soft_knee '{"x": "3e38", "knee": "0.5"}' >"$WORK/run_draft.json" || rc_run_draft=$?

jq -n -S \
  --rawfile usage "$WORK/usage.txt" \
  --slurpfile check_quick "$WORK/check_quick.json" \
  --slurpfile verify_quick "$WORK/verify_quick.json" \
  --slurpfile run_add_sat "$WORK/run_add_sat.json" \
  --slurpfile run_soft_knee "$WORK/run_soft_knee.json" \
  --slurpfile build_soft_knee "$WORK/build_soft_knee.json" \
  --slurpfile verify_draft "$WORK/verify_draft.json" \
  --slurpfile explain_draft "$WORK/explain_draft.json" \
  --slurpfile run_draft "$WORK/run_draft.json" \
  --argjson rc_verify_draft "$rc_verify_draft" \
  --argjson rc_explain_draft "$rc_explain_draft" \
  --argjson rc_run_draft "$rc_run_draft" \
  '
  ($usage | split("\n") | reduce .[] as $line (
    [];
    if ($line | startswith("  vg ")) then
      ($line[2:] |
        if test("  +") then
          capture("^(?<cmd>.*?)  +(?<summary>.*)$")
        elif test("\\[--timeout MS\\] ") then
          capture("^(?<cmd>.*?\\[--timeout MS\\]) (?<summary>.*)$")
        elif test("\\[--addr HOST:PORT\\] ") then
          capture("^(?<cmd>.*?\\[--addr HOST:PORT\\]) (?<summary>.*)$")
        else
          {cmd: ., summary: ""}
        end
      ) as $entry
      | . + [$entry]
    elif ($line | startswith("    ")) and (length > 0) then
      .[-1].summary = (
        if (.[-1].summary | length) > 0 then
          .[-1].summary + " " + ($line | gsub("^ +"; ""))
        else
          ($line | gsub("^ +"; ""))
        end
      )
    else
      .
    end
  )) as $cmds
  | {
      usage: $usage,
      commands: $cmds,
      check_quick: $check_quick[0],
      verify_quick: $verify_quick[0],
      run_add_sat: $run_add_sat[0],
      run_soft_knee: $run_soft_knee[0],
      build_soft_knee: $build_soft_knee[0],
      verify_draft: $verify_draft[0],
      explain_draft: $explain_draft[0],
      run_draft: $run_draft[0],
      exit_codes: {
        verify_draft: $rc_verify_draft,
        explain_draft: $rc_explain_draft,
        run_draft: $rc_run_draft
      },
      counts: {
        quickstart_nodes: ($check_quick[0].nodes | length),
        quickstart_obligations: ($verify_quick[0].results | length),
        quickstart_proven: ([$verify_quick[0].results[] | select(.verdict == "proven")] | length),
        draft_obligations: ($verify_draft[0].results | length),
        draft_refuted: ([$verify_draft[0].results[] | select(.verdict == "counterexample")] | length),
        build_bytes: $build_soft_knee[0].artifacts[0].bytes,
        build_guards: $build_soft_knee[0].artifacts[0].residual_guards,
        build_blake3: $build_soft_knee[0].artifacts[0].blake3,
        build_target: $build_soft_knee[0].artifacts[0].target,
        build_path: $build_soft_knee[0].artifacts[0].path
      }
    }
  ' >site/data/utiliser.json
