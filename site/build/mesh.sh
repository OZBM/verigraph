#!/bin/sh
set -eu
VG=${VG:-./target/release/vg}
WORK=${WORK:-target/site/mesh}
rm -rf "$WORK"
mkdir -p "$WORK" site/data

"$VG" mesh publish examples/sha256.vgs --root "$WORK" >"$WORK/publish.json"
jq -e '.ok == true' "$WORK/publish.json" >/dev/null

cat >"$WORK/sha256_query.json" <<'EOF'
{
  "input_types": ["Slice<u8>"],
  "output_types": ["Array<u8, 32>"],
  "max_effects": [],
  "spec": "sha256",
  "intent": "SHA-256 digest of a byte slice"
}
EOF

"$VG" mesh query "$WORK/sha256_query.json" --root "$WORK" >"$WORK/query.json"
jq -e '.ok == true and (.candidates | length >= 1)' "$WORK/query.json" >/dev/null

"$VG" mesh resolve examples/audio_limiter.vgs --root "$WORK" >"$WORK/resolve.json"
jq -e '.ok == true' "$WORK/resolve.json" >/dev/null

"$VG" verify examples/audio_limiter.vgs --mode edit --root "$WORK" --detail 0 >"$WORK/limiter_verify.json"
jq -e '.ok == true' "$WORK/limiter_verify.json" >/dev/null

"$VG" run examples/sha256.vgs @sha256_impl '{"m": [97, 98, 99]}' >"$WORK/sha256_abc.json"
jq -e '.ok == true' "$WORK/sha256_abc.json" >/dev/null

jq -n -S \
  --slurpfile pub "$WORK/publish.json" \
  --slurpfile qin "$WORK/sha256_query.json" \
  --slurpfile qout "$WORK/query.json" \
  --slurpfile res "$WORK/resolve.json" \
  --rawfile vglock "$WORK/vg.lock" \
  --slurpfile lim "$WORK/limiter_verify.json" \
  --slurpfile abc "$WORK/sha256_abc.json" \
  '{
    publish: $pub[0],
    query_input: $qin[0],
    query: $qout[0],
    resolve: $res[0],
    vg_lock: $vglock,
    limiter_verify: {
      ok: $lim[0].ok,
      nodes: $lim[0].nodes,
      total: ($lim[0].results | length),
      proven: ([$lim[0].results[] | select(.verdict == "proven")] | length)
    },
    sha256_abc: $abc[0],
    counts: {
      accepted: ($pub[0].accepted | length),
      rejected: ($pub[0].rejected | length),
      candidates: ($qout[0].candidates | length),
      bound: ($res[0].bound | length),
      limiter_obligations: ($lim[0].results | length),
      limiter_proven: ([$lim[0].results[] | select(.verdict == "proven")] | length)
    }
  }' >site/data/mesh.json
