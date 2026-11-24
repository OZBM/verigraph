#!/bin/sh
set -eu
VG=${VG:-./target/release/vg}
WORK=${WORK:-target/site/vg-sym}
mkdir -p "$WORK" site/data

"$VG" check examples/quickstart.vgs --root "$WORK" >"$WORK/quickstart_check.json"
"$VG" check examples/mandelbrot.vgs --root "$WORK" >"$WORK/mandelbrot_check.json"

"$VG" run examples/quickstart.vgs @add_sat '{"a": 200, "b": 100}' --root "$WORK" >"$WORK/run_add_sat.json"
"$VG" run examples/quickstart.vgs @midpoint '{"a": 4294967295, "b": 4294967293}' --root "$WORK" >"$WORK/run_midpoint.json"
"$VG" run examples/quickstart.vgs @sum4 '{"xs": [1, 2, 3, 994]}' --root "$WORK" >"$WORK/run_sum4.json"

lark_lines=$(wc -l <tools/vgsym.lark | tr -d ' ')
lark_nonterminals=$(awk '/^[?]?[a-z_]+:/ { c++ } END { print c + 0 }' tools/vgsym.lark)
lark_terminals=$(awk '/^[A-Z_]+:/ { c++ } END { print c + 0 }' tools/vgsym.lark)

jq -S -n \
  --slurpfile qs_check "$WORK/quickstart_check.json" \
  --slurpfile md_check "$WORK/mandelbrot_check.json" \
  --slurpfile r_add "$WORK/run_add_sat.json" \
  --slurpfile r_mid "$WORK/run_midpoint.json" \
  --slurpfile r_sum "$WORK/run_sum4.json" \
  --argjson lark_lines "$lark_lines" \
  --argjson lark_nonterminals "$lark_nonterminals" \
  --argjson lark_terminals "$lark_terminals" \
  '{
    quickstart_check: $qs_check[0],
    mandelbrot_check: $md_check[0],
    lark: {
      lines: $lark_lines,
      nonterminals: $lark_nonterminals,
      terminals: $lark_terminals
    },
    runs: [
      {
        cmd: "vg run examples/quickstart.vgs @add_sat '\''{\"a\": 200, \"b\": 100}'\''",
        out: ($r_add[0] | tojson)
      },
      {
        cmd: "vg run examples/quickstart.vgs @midpoint '\''{\"a\": 4294967295, \"b\": 4294967293}'\''",
        out: ($r_mid[0] | tojson)
      },
      {
        cmd: "vg run examples/quickstart.vgs @sum4 '\''{\"xs\": [1, 2, 3, 994]}'\''",
        out: ($r_sum[0] | tojson)
      }
    ]
  }' >site/data/vg-sym.json
