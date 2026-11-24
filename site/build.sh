#!/bin/sh
# Usage: site/build.sh [--no-pages]. Builds everything the site computes or shows, with the toolchain only:
#   1. proves every module whose nodes the pages run (vg verify) and stops if one fails; the draft
#      soft-knee must be rejected exactly where examples/soft_knee_draft.vgs says;
#   2. checks that copied nodes have their originals' hashes (a node's identity is its content);
#   3. compiles each exported node the pages call to wasm32-wasip1 (vg build), unmodified;
#   4. writes wasm/proofs.json (every obligation's verdict, by module, node, kind and solver),
#      wasm/builds.json (each binary's blake3, size, guards and proof closure) and wasm/bench.json;
#   5. runs each chapter's data script, site/build/<page>.sh, which writes site/data/<page>.json.
# Every number the site shows comes from these files. Needs jq; wasm32 needs an LLVM clang.
set -eu
cd "$(dirname "$0")/.."
cargo build --release -p vg-cli --locked -q
VG=$PWD/target/release/vg
work=target/site
out=site/wasm
# A fresh work directory, so no proof cache or earlier run changes what the reports say.
rm -rf "$work"
mkdir -p "$work" "$out" site/data

# examples/audio_limiter.vgs takes its SHA-256 from a Mesh (docs/09): publish it to a fresh root and
# resolve the query there.
mesh=$work/mesh
rm -rf "$mesh"
mkdir -p "$mesh"
"$VG" mesh publish examples/sha256.vgs --root "$mesh" >"$work/mesh.publish.json"
"$VG" mesh resolve examples/audio_limiter.vgs --root "$mesh" >"$work/mesh.resolve.json"
jq -e .ok "$work/mesh.publish.json" "$work/mesh.resolve.json" >/dev/null

# zlib's crc32_z, lifted from C by OmniLift (docs/08).
"$VG" lift tests/lift/zlib/crc32.c crc32_z --lang c --root "$work" >"$work/lift.crc32.json"
jq -e .ok "$work/lift.crc32.json" >/dev/null
jq -r .vgsym "$work/lift.crc32.json" >"$work/crc32_lifted.vgs"

verify() { # FILE NAME [ROOT]
  if ! "$VG" verify "$1" --root "${3:-$work}" --timeout 60000 --detail 1 >"$work/$2.verify.json" ||
     ! jq -e '.ok' "$work/$2.verify.json" >/dev/null; then
    echo "build.sh: $1 does not verify (see $work/$2.verify.json)" >&2
    exit 1
  fi
}

# The call edges of a module, "caller callee" per line: every @label a node's body names, except
# FOLD invariants, which are clauses of the caller's contract, not nodes.
edges() { # FILE NAME
  awk '
    /^NODE @/ { cur = $2; inbody = 0; next }
    /^ *BODY *\{/ { inbody = 1; next }
    /^}/ { cur = ""; inbody = 0; next }
    cur != "" && inbody {
      line = $0; sub(/\/\/.*/, "", line); gsub(/invariant=@[A-Za-z0-9_]+/, "", line)
      while (match(line, /@[A-Za-z0-9_]+/)) { print cur, substr(line, RSTART, RLENGTH); line = substr(line, RSTART + RLENGTH) }
    }' "$1" | sort -u >"$work/$2.edges"
}

# The hash a file gives a label (vg hash: FILE LABEL node HASH body ... contract ...).
hash_of() { "$VG" hash "$1" | awk -v l="$2" '$2 == l { print $4 }'; }
same() { # FILE LABEL ORIGINAL
  a=$(hash_of "$1" "$2")
  b=$(hash_of "$3" "$2")
  if [ -z "$a" ] || [ "$a" != "$b" ]; then
    echo "build.sh: $2 in $1 is not the node of $3 ($a, $b)" >&2
    exit 1
  fi
}

build() { # FILE @ROOT NAME [ROOT] [--unverified]
  if ! "$VG" build "$1" "$2" --target wasm32-wasip1 --root "${4:-$work}" -o "$work/$3" ${5:-} >"$work/$3.build.json" ||
     ! jq -e '.ok' "$work/$3.build.json" >/dev/null; then
    echo "build.sh: $1 $2 does not build (see $work/$3.build.json)" >&2
    exit 1
  fi
  if [ -z "${5:-}" ] && ! jq -e '.verified' "$work/$3.build.json" >/dev/null; then
    echo "build.sh: $1 $2 built without its proofs" >&2
    exit 1
  fi
  cp "$work/$3/${2#@}-wasm32-wasip1" "$out/$3.wasm"
  printf '%s %s %s %s\n' "$3" "$2" "$1" "${5:+unverified}" >>"$work/binaries.txt"
}

# 1. Proofs. MODULES lists "file name root": every module a binary of the site comes from.
MODULES="site/vg/weblib.vgs weblib $work
examples/mandelbrot.vgs mandelbrot $work
site/vg/diagram.vgs diagram $work
site/vg/demos.vgs demos $work
examples/sha256.vgs sha256 $work
examples/audio_limiter.vgs audio_limiter $mesh
$work/crc32_lifted.vgs crc32_lifted $work"
echo "$MODULES" | while read -r file name root; do
  verify "$file" "$name" "$root"
  edges "$file" "$name"
done

# The draft must fail, and only at the three clauses of its POST, each with a counterexample.
"$VG" verify site/vg/draft.vgs --root "$work" --timeout 60000 --detail 1 >"$work/draft.verify.json" || true
jq -e '[.results[] | select(.verdict != "proven")] | (map(.id) == ["@soft_knee/POST.0", "@soft_knee/POST.1", "@soft_knee/POST.2"]) and all(.verdict == "counterexample")' \
  "$work/draft.verify.json" >/dev/null || { echo "build.sh: site/vg/draft.vgs is not refuted as expected" >&2; exit 1; }

# 2. Copies are the same nodes as their originals.
same site/vg/demos.vgs @soft_knee examples/quickstart.vgs
same site/vg/demos.vgs @fract site/vg/weblib.vgs
same site/vg/demos.vgs @sin_turns site/vg/weblib.vgs
same site/vg/draft.vgs @soft_knee examples/soft_knee_draft.vgs

# 3. Binaries.
: >"$work/binaries.txt"
for root in @aurora @motion @bar_chart @donut @zoom; do build site/vg/weblib.vgs "$root" "${root#@}"; done
build examples/mandelbrot.vgs @render render
build site/vg/diagram.vgs @layout layout
build site/vg/demos.vgs @grid grid
build site/vg/demos.vgs @knee_curve knee
build site/vg/demos.vgs @tone tone
build site/vg/demos.vgs @peak peak
build site/vg/draft.vgs @draft_curve knee_draft "$work" --unverified
build examples/sha256.vgs @sha256_impl sha256
build examples/audio_limiter.vgs @limit_frame limiter "$mesh"
build "$work/crc32_lifted.vgs" @crc32_z crc32

# 4. Reports. An obligation's id is @node/KIND… or @node/%port/KIND…; its kind is the first word of
# the last part.
echo "$MODULES" | while read -r file name root; do
  jq -c --arg file "$file" --arg name "$name" --rawfile edges "$work/$name.edges" '{
      file: $file, shown: (if $name == "crc32_lifted" then "tests/lift/zlib/crc32.c, relevé par vg lift" else $file end),
      name: $name, nodes: [.nodes[].label], results,
      edges: [$edges | split("\n")[] | select(length > 0) | split(" ")]
    }' "$work/$name.verify.json"
done >"$work/modules.jsonl"

jq -s '
  def kind: .id | split("/") | last | split(".") | first;
  . as $mods
  | [$mods[].results[]] as $all
  | ([$all[] | kind] | group_by(.) | map({kind: .[0], n: length}) | sort_by(-.n, .kind)) as $kinds
  | {
      modules: [$mods[] | {file: .shown, name, nodes: (.nodes | length), total: (.results | length),
                           proven: ([.results[] | select(.verdict == "proven")] | length),
                           solver_ms: ([.results[].time_ms] | add)}],
      total: ($all | length),
      proven: ([$all[] | select(.verdict == "proven")] | length),
      nodes: ([$mods[].nodes | length] | add),
      solver_ms: ([$all[].time_ms] | add),
      by_kind: $kinds,
      kinds8: ($kinds[:7] + (if ($kinds | length) > 7 then [{kind: "other", n: ($kinds[7:] | map(.n) | add)}] else [] end)),
      by_solver: ([$all[] | .solver] | group_by(.) | map({solver: .[0], n: length}) | sort_by(-.n, .solver)),
      by_evidence: ([$all[] | .evidence] | group_by(.) | map({evidence: .[0], n: length}) | sort_by(-.n, .evidence)),
      by_node: [$mods[] | .name as $m | .results | group_by(.id | split("/") | first)[] | {
        module: $m, label: (.[0].id | split("/") | first), n: length,
        proven: ([.[] | select(.verdict == "proven")] | length), ms: (map(.time_ms) | add)
      }]
    }' "$work/modules.jsonl" >"$out/proofs.json"

# Each binary: its build report, and the proof of everything its root calls (within its module).
while read -r name root file unverified; do
  if [ -n "$unverified" ]; then
    jq -c --arg name "$name" --arg node "$root" --arg file "$file" '
      {name: $name, node: $node, file: $file, verified: false}
      + (.artifacts[0] | {blake3, bytes, kb: ((.bytes / 102.4 | round) / 10), residual_guards, target})' "$work/$name.build.json"
  else
    jq -c --arg name "$name" --arg node "$root" --arg file "$file" --slurpfile mods "$work/modules.jsonl" '
      ($mods | map(select(.file == $file)) | first) as $m
      | ([$node] | until(. as $s | ([$m.edges[] | select(.[0] as $c | $s | index([$c])) | .[1]] + $s | unique) == $s;
                         . as $s | [$m.edges[] | select(.[0] as $c | $s | index([$c])) | .[1]] + $s | unique)) as $closure
      | [$m.results[] | select((.id | split("/") | first) as $l | $closure | index([$l]))] as $obl
      | {name: $name, node: $node, file: $m.shown, verified: true,
         closure_nodes: ($closure | length), obligations: ($obl | length), module: $m.name,
         proven: ([$obl[] | select(.verdict == "proven")] | length)}
      + (.artifacts[0] | {blake3, bytes, kb: ((.bytes / 102.4 | round) / 10), residual_guards, target})' "$work/$name.build.json"
  fi
done <"$work/binaries.txt" | jq -s '{binaries: ., residual_guards: (map(.residual_guards) | add), kb: ((map(.bytes) | add) / 1024 | round)}' >"$out/builds.json"

cp reports/bench/latest.json "$out/bench.json"

# 5. The chapters' own data (--no-pages skips it, while chapters are being written).
[ "${1:-}" = "--no-pages" ] && exit 0
for script in site/build/*.sh; do
  [ -e "$script" ] || continue
  page=$(basename "$script" .sh)
  if ! VG="$VG" WORK="$work/$page" sh "$script"; then
    echo "build.sh: $script failed" >&2
    exit 1
  fi
done

echo "site/build.sh: $(jq -r '"\(.proven) of \(.total) obligations proven over \(.nodes) nodes"' "$out/proofs.json"); $(jq -r '.binaries | length' "$out/builds.json") binaries in $out"
