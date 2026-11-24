#!/bin/sh
set -eu
VG=${VG:-./target/release/vg}
WORK=${WORK:-target/site/omnilift}
rm -rf "$WORK"
mkdir -p "$WORK" site/data

"$VG" lift tests/lift/zlib/crc32.c crc32_z --lang c --root "$WORK" >"$WORK/crc32.json"
jq -e '.ok == true' "$WORK/crc32.json" >/dev/null

"$VG" lift tests/lift/zlib/adler32.c adler32_z --lang c --root "$WORK" >"$WORK/adler32.json"
jq -e '.ok == true' "$WORK/adler32.json" >/dev/null

jq -r '.vgsym' "$WORK/crc32.json" >"$WORK/crc32_lifted.vgs"
jq -r '.vgsym' "$WORK/adler32.json" >"$WORK/adler32_lifted.vgs"

rc_crc_ver=0
"$VG" verify "$WORK/crc32_lifted.vgs" --mode edit --root "$WORK" --detail 1 >"$WORK/crc32_verify.json" || rc_crc_ver=$?

rc_adl_ver=0
"$VG" verify "$WORK/adler32_lifted.vgs" --mode edit --root "$WORK" --detail 1 >"$WORK/adler32_verify.json" || rc_adl_ver=$?

jq -n -S \
  --slurpfile crc "$WORK/crc32.json" \
  --slurpfile adl "$WORK/adler32.json" \
  --slurpfile crc_ver "$WORK/crc32_verify.json" \
  --slurpfile adl_ver "$WORK/adler32_verify.json" \
  --argjson rc_crc_ver "$rc_crc_ver" \
  --argjson rc_adl_ver "$rc_adl_ver" \
  '
  ($crc[0].vgsym | split("\n")) as $crc_lines
  | {
    crc32: {
      node_hash: $crc[0].node_hash,
      root_label: $crc[0].root_label,
      report: $crc[0].report,
      provenance: $crc[0].provenance,
      vgsym_line_count: ($crc_lines | length),
      vgsym_tail: ($crc_lines[-45:] | join("\n"))
    },
    crc32_verify: {
      exit_code: $rc_crc_ver,
      ok: $crc_ver[0].ok,
      nodes: $crc_ver[0].nodes,
      total: ($crc_ver[0].results | length),
      proven: ([$crc_ver[0].results[] | select(.verdict == "proven")] | length),
      sample: $crc_ver[0].results[:5]
    },
    adler32: {
      node_hash: $adl[0].node_hash,
      root_label: $adl[0].root_label,
      report: $adl[0].report,
      provenance: $adl[0].provenance,
      vgsym_line_count: ($adl[0].vgsym | split("\n") | length)
    },
    adler32_verify: {
      exit_code: $rc_adl_ver,
      ok: $adl_ver[0].ok,
      nodes: $adl_ver[0].nodes,
      total: ($adl_ver[0].results | length),
      proven: ([$adl_ver[0].results[] | select(.verdict == "proven")] | length)
    }
  }' >site/data/omnilift.json
