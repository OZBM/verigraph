#!/bin/sh
# Génère site/data/preuves.json à partir d'exécutions réelles de vg check et vg verify.
set -eu
VG=${VG:-./target/release/vg}
WORK=${WORK:-target/site/preuves}
rm -rf "$WORK/edit" "$WORK/release"
mkdir -p "$WORK/edit" "$WORK/release" site/data

"$VG" check examples/quickstart.vgs >"$WORK/quickstart_check.json"
"$VG" check examples/mandelbrot.vgs >"$WORK/mandelbrot_check.json"

"$VG" verify examples/quickstart.vgs --mode edit --detail 1 --root "$WORK/edit" >"$WORK/quickstart_edit.json"
"$VG" verify examples/quickstart.vgs --mode edit --detail 1 --root "$WORK/edit" >"$WORK/quickstart_cached.json"
"$VG" verify examples/mandelbrot.vgs --mode edit --detail 1 --root "$WORK/edit" >"$WORK/mandelbrot_edit.json"
"$VG" verify examples/quickstart.vgs --mode release --detail 1 --root "$WORK/release" >"$WORK/quickstart_release.json"

jq -e '.ok == true' "$WORK/quickstart_edit.json" >/dev/null
jq -e '.ok == true' "$WORK/mandelbrot_edit.json" >/dev/null
jq -e '.ok == true' "$WORK/quickstart_release.json" >/dev/null

jq -n -S \
  --slurpfile qc "$WORK/quickstart_check.json" \
  --slurpfile mc "$WORK/mandelbrot_check.json" \
  --slurpfile qe "$WORK/quickstart_edit.json" \
  --slurpfile qcache "$WORK/quickstart_cached.json" \
  --slurpfile me "$WORK/mandelbrot_edit.json" \
  --slurpfile qr "$WORK/quickstart_release.json" \
  --rawfile qe_txt "$WORK/quickstart_edit.json" \
  --rawfile qcache_txt "$WORK/quickstart_cached.json" \
  --rawfile qr_txt "$WORK/quickstart_release.json" \
  '
  def ob_kind: .id | split("/") | last | split(".") | first;
  def ob_node: .id | split("/") | first;
  def ob_port: (.id | split("/")) as $p | if ($p | length) == 3 then $p[1] else null end;
  def enrich: map(. + { node: ob_node, port: ob_port, kind: ob_kind });
  def join_nodes($chk; $ver):
    ($chk.nodes | map({ (.label): . }) | add) as $cmap
    | ($ver.results | group_by(ob_node) | map({ (.[0] | ob_node): length }) | add) as $counts
    | [$ver.nodes[] | . + ($cmap[.label] // {}) + { obligations: ($counts[.label] // 0) }];

  ($qe[0].results + $me[0].results) as $all
  | ([$all[] | ob_kind] | group_by(.) | map({ kind: .[0], n: length }) | sort_by(-.n, .kind)) as $kinds
  | {
      modules: [
        {
          file: "examples/quickstart.vgs",
          nodes: join_nodes($qc[0]; $qe[0]),
          results: ($qe[0].results | enrich)
        },
        {
          file: "examples/mandelbrot.vgs",
          nodes: join_nodes($mc[0]; $me[0]),
          results: ($me[0].results | enrich)
        }
      ],
      aggregates: {
        total: ($all | length),
        proven: ([$all[] | select(.verdict == "proven")] | length),
        nodes: (($qe[0].nodes + $me[0].nodes) | length),
        by_kind: $kinds,
        kinds8: ($kinds[:7] + (if ($kinds | length) > 7 then [{ kind: "other", n: ($kinds[7:] | map(.n) | add) }] else [] end)),
        by_solver: ([$all[] | .solver] | group_by(.) | map({ solver: .[0], n: length }) | sort_by(-.n, .solver)),
        by_node: ($all | group_by(ob_node) | map({
          label: (.[0] | ob_node),
          n: length,
          proven: ([.[] | select(.verdict == "proven")] | length),
          ms: (map(.time_ms) | add)
        }))
      },
      release: {
        ok: $qr[0].ok,
        total: ($qr[0].results | length),
        proven: ([$qr[0].results[] | select(.verdict == "proven")] | length),
        by_evidence: ([$qr[0].results[] | (.evidence // "none")] | group_by(.) | map({ evidence: .[0], n: length }) | sort_by(-.n, .evidence)),
        results: ($qr[0].results | enrich)
      },
      cache_demo: {
        run1_by_evidence: ([$qe[0].results[] | (.evidence // "none")] | group_by(.) | map({ evidence: .[0], n: length }) | sort_by(-.n, .evidence)),
        run2_by_evidence: ([$qcache[0].results[] | (.evidence // "none")] | group_by(.) | map({ evidence: .[0], n: length }) | sort_by(-.n, .evidence))
      },
      terminal_session: [
        { cmd: "vg verify examples/quickstart.vgs --mode edit --detail 1", out: $qe_txt },
        { cmd: "vg verify examples/quickstart.vgs --mode edit --detail 1  # second run (cache)", out: $qcache_txt },
        { cmd: "vg verify examples/quickstart.vgs --mode release --detail 1", out: $qr_txt }
      ]
    }
  ' >site/data/preuves.json
