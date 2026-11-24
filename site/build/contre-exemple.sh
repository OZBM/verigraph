#!/bin/sh
# Génère site/data/contre-exemple.json à partir d'exécutions réelles de vg.
set -eu
VG=${VG:-./target/release/vg}
WORK=${WORK:-target/site/contre-exemple}
mkdir -p "$WORK" "$WORK/mesh" site/data

"$VG" verify examples/soft_knee_draft.vgs --mode edit --detail 2 --root "$WORK" >"$WORK/draft_verify.json" || true
jq -e '.ok == false' "$WORK/draft_verify.json" >/dev/null

"$VG" explain examples/soft_knee_draft.vgs @soft_knee/POST.0 >"$WORK/explain.json"
"$VG" verify examples/quickstart.vgs --mode edit --detail 1 --root "$WORK" >"$WORK/quickstart_verify.json"
jq -e '.ok == true' "$WORK/quickstart_verify.json" >/dev/null

"$VG" hash examples/soft_knee_draft.vgs >"$WORK/draft_hash.txt"
"$VG" hash examples/quickstart.vgs >"$WORK/quickstart_hash.txt"
"$VG" mesh publish examples/sha256.vgs --root "$WORK/mesh" >"$WORK/mesh_publish.json"
"$VG" mesh resolve examples/audio_limiter.vgs --root "$WORK/mesh" >"$WORK/mesh_resolve.json"
"$VG" hash examples/audio_limiter.vgs --root "$WORK/mesh" >"$WORK/limiter_hash.txt"

"$VG" run examples/soft_knee_draft.vgs @soft_knee '{"x":"0.3","knee":"0.5"}' >"$WORK/run_draft_03.json"
"$VG" run examples/quickstart.vgs @soft_knee '{"x":"0.3","knee":"0.5"}' >"$WORK/run_rep_03.json"

"$VG" run examples/soft_knee_draft.vgs @soft_knee '{"x":"0.8","knee":"0.5"}' >"$WORK/run_draft_08.json"
"$VG" run examples/quickstart.vgs @soft_knee '{"x":"0.8","knee":"0.5"}' >"$WORK/run_rep_08.json"

"$VG" run examples/soft_knee_draft.vgs @soft_knee '{"x":"3e38","knee":"0.5"}' >"$WORK/run_draft_3e38.json"
"$VG" run examples/quickstart.vgs @soft_knee '{"x":"3e38","knee":"0.5"}' >"$WORK/run_rep_3e38.json"

"$VG" run examples/soft_knee_draft.vgs @soft_knee '{"x":"-3.4028235e38","knee":"0.5"}' >"$WORK/run_draft_ce.json"
"$VG" run examples/quickstart.vgs @soft_knee '{"x":"-3.4028235e38","knee":"0.5"}' >"$WORK/run_rep_ce.json"

parse_hash() {
  # Extrait les empreintes node/body/contract du nœud @soft_knee depuis la sortie de vg hash.
  awk '$2 == "@soft_knee" { printf "{\"node\":\"%s\",\"body\":\"%s\",\"contract\":\"%s\"}\n", $4, $6, $8 }' "$1"
}

h_draft=$(parse_hash "$WORK/draft_hash.txt")
h_quick=$(parse_hash "$WORK/quickstart_hash.txt")
h_limit=$(parse_hash "$WORK/limiter_hash.txt")

jq -n -S \
  --slurpfile draft_verify "$WORK/draft_verify.json" \
  --slurpfile explain "$WORK/explain.json" \
  --slurpfile quickstart_verify "$WORK/quickstart_verify.json" \
  --argjson h_draft "$h_draft" \
  --argjson h_quick "$h_quick" \
  --argjson h_limit "$h_limit" \
  --slurpfile rd03 "$WORK/run_draft_03.json" \
  --slurpfile rr03 "$WORK/run_rep_03.json" \
  --slurpfile rd08 "$WORK/run_draft_08.json" \
  --slurpfile rr08 "$WORK/run_rep_08.json" \
  --slurpfile rd3e38 "$WORK/run_draft_3e38.json" \
  --slurpfile rr3e38 "$WORK/run_rep_3e38.json" \
  --slurpfile rdce "$WORK/run_draft_ce.json" \
  --slurpfile rrce "$WORK/run_rep_ce.json" \
  --rawfile explain_txt "$WORK/explain.json" \
  --rawfile rdce_txt "$WORK/run_draft_ce.json" \
  --rawfile rrce_txt "$WORK/run_rep_ce.json" \
  '{
    draft_verify: $draft_verify[0],
    explain: $explain[0],
    repaired_verify: {
      ok: ($quickstart_verify[0].nodes[] | select(.label == "@soft_knee") | .verified),
      results: [$quickstart_verify[0].results[] | select(.id | startswith("@soft_knee/"))],
      total: ([$quickstart_verify[0].results[] | select(.id | startswith("@soft_knee/"))] | length),
      proven: ([$quickstart_verify[0].results[] | select(.id | startswith("@soft_knee/")) | select(.verdict == "proven")] | length)
    },
    hashes: {
      draft: $h_draft,
      quickstart: $h_quick,
      audio_limiter: $h_limit,
      same_node_as_audio_limiter: ($h_quick.node == $h_limit.node),
      same_contract_as_draft: ($h_draft.contract == $h_quick.contract)
    },
    runs: [
      { label: "Sous le seuil (x = 0.3)", inputs: { x: "0.3", knee: "0.5" }, draft: $rd03[0], repaired: $rr03[0] },
      { label: "Dans le coude (x = 0.8)", inputs: { x: "0.8", knee: "0.5" }, draft: $rd08[0], repaired: $rr08[0] },
      { label: "Grand positif (x = 3e38)", inputs: { x: "3e38", knee: "0.5" }, draft: $rd3e38[0], repaired: $rr3e38[0] },
      { label: "Contre-exemple (x = -3.4028235e38)", inputs: { x: "-3.4028235e38", knee: "0.5" }, draft: $rdce[0], repaired: $rrce[0] }
    ],
    terminal_session: [
      { cmd: "vg explain examples/soft_knee_draft.vgs @soft_knee/POST.0", out: $explain_txt },
      { cmd: "vg run examples/soft_knee_draft.vgs @soft_knee '\''{\"x\":\"-3.4028235e38\",\"knee\":\"0.5\"}'\''", out: $rdce_txt },
      { cmd: "vg run examples/quickstart.vgs @soft_knee '\''{\"x\":\"-3.4028235e38\",\"knee\":\"0.5\"}'\''", out: $rrce_txt }
    ]
  }' >site/data/contre-exemple.json
