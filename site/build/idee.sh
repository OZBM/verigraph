#!/bin/sh
set -eu
VG=${VG:-./target/release/vg}
WORK=${WORK:-target/site/idee}
mkdir -p "$WORK" site/data

# 1. Six ordinary test inputs on examples/soft_knee_draft.vgs @soft_knee
: >"$WORK/draft_tests.ndjson"
for x in -2.0 -0.7 0.0 0.3 0.9 1.5; do
  in_json=$(jq -cn --arg x "$x" --arg k "0.5" '{x: $x, knee: $k}')
  out_json=$("$VG" run examples/soft_knee_draft.vgs @soft_knee "$in_json" --root "$WORK")
  printf '%s\n' "$out_json" | jq -c --arg x "$x" --arg k "0.5" \
    '{x: $x, knee: $k, ok: .ok, output: .outputs[0], pre: .pre, post: .post}' >>"$WORK/draft_tests.ndjson"
done
jq -s '.' "$WORK/draft_tests.ndjson" >"$WORK/draft_tests.json"

# 2. Verify examples/soft_knee_draft.vgs (expected to fail with a counterexample)
"$VG" verify examples/soft_knee_draft.vgs --root "$WORK" --mode edit --detail 2 >"$WORK/draft_verify.json" || true
jq -e '.results | length > 0' "$WORK/draft_verify.json" >/dev/null

ce_x=$(jq -r '[.results[] | select(.verdict == "counterexample")][0].report.inputs.x' "$WORK/draft_verify.json")
ce_k=$(jq -r '[.results[] | select(.verdict == "counterexample")][0].report.inputs.knee' "$WORK/draft_verify.json")
ce_in=$(jq -cn --arg x "$ce_x" --arg k "$ce_k" '{x: $x, knee: $k}')

# 3. Run the draft on the verifier's counterexample
"$VG" run examples/soft_knee_draft.vgs @soft_knee "$ce_in" --root "$WORK" >"$WORK/draft_ce_run.json" || true

# 4. Verify the repaired @soft_knee in examples/quickstart.vgs and run it on the same counterexample
"$VG" verify examples/quickstart.vgs --root "$WORK" --mode edit --detail 1 >"$WORK/quickstart_verify.json"
"$VG" run examples/quickstart.vgs @soft_knee "$ce_in" --root "$WORK" >"$WORK/repaired_ce_run.json"

# 5. Assemble deterministic JSON output
jq -S -n \
  --slurpfile draft_tests "$WORK/draft_tests.json" \
  --slurpfile draft_verify "$WORK/draft_verify.json" \
  --slurpfile draft_ce_run "$WORK/draft_ce_run.json" \
  --slurpfile qs_verify "$WORK/quickstart_verify.json" \
  --slurpfile repaired_ce_run "$WORK/repaired_ce_run.json" \
  --arg ce_x "$ce_x" \
  --arg ce_k "$ce_k" \
  '
  ($draft_verify[0]) as $dv |
  ($qs_verify[0]) as $qv |
  ([$dv.results[] | select(.verdict == "counterexample")][0]) as $ce |
  ([$qv.results[] | select(.id | startswith("@soft_knee/"))]) as $qs_sk |
  {
    draft_tests: $draft_tests[0],
    draft_verify: {
      ok: $dv.ok,
      results: [$dv.results[] | {
        id,
        verdict,
        code: (.code // null),
        solver: (.solver // null),
        evidence: (.evidence // null),
        time_ms: (.time_ms // null),
        reproduced: (.report.reproduced // null),
        inputs: (.report.inputs // null)
      }]
    },
    counterexample: {
      id: $ce.id,
      code: $ce.code,
      solver: $ce.solver,
      evidence: ($ce.evidence // null),
      reproduced: $ce.report.reproduced,
      inputs: $ce.report.inputs
    },
    draft_counterexample_run: {
      x: $ce_x,
      knee: $ce_k,
      ok: $draft_ce_run[0].ok,
      output: $draft_ce_run[0].outputs[0],
      pre: $draft_ce_run[0].pre,
      post: $draft_ce_run[0].post
    },
    repaired_obligations: {
      total: ($qs_sk | length),
      proven: ([$qs_sk[] | select(.verdict == "proven")] | length),
      results: [$qs_sk[] | {id, verdict, solver: (.solver // null), evidence: (.evidence // null), time_ms: (.time_ms // null)}]
    },
    repaired_counterexample_run: {
      x: $ce_x,
      knee: $ce_k,
      ok: $repaired_ce_run[0].ok,
      output: $repaired_ce_run[0].outputs[0],
      pre: $repaired_ce_run[0].pre,
      post: $repaired_ce_run[0].post
    },
    terminal_steps: [
      {
        cmd: "vg run examples/soft_knee_draft.vgs @soft_knee '\''{\"x\": \"1.5\", \"knee\": \"0.5\"}'\''",
        out: ($draft_tests[0] | map(select(.x == "1.5"))[0] | {ok, outputs: [.output], pre, post} | tojson)
      },
      {
        cmd: "vg verify examples/soft_knee_draft.vgs --mode edit --detail 2",
        out: ({
          ok: $dv.ok,
          counterexample: {
            id: $ce.id,
            code: $ce.code,
            solver: $ce.solver,
            evidence: ($ce.evidence // null),
            inputs: $ce.report.inputs,
            reproduced: $ce.report.reproduced
          }
        } | tojson)
      },
      {
        cmd: ("vg run examples/soft_knee_draft.vgs @soft_knee '\''{\"x\": \"" + $ce_x + "\", \"knee\": \"" + $ce_k + "\"}'\''"),
        out: ($draft_ce_run[0] | tojson)
      },
      {
        cmd: ("vg run examples/quickstart.vgs @soft_knee '\''{\"x\": \"" + $ce_x + "\", \"knee\": \"" + $ce_k + "\"}'\''"),
        out: ($repaired_ce_run[0] | tojson)
      }
    ]
  }' >site/data/idee.json
