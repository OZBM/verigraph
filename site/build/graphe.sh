#!/bin/sh
set -eu
VG=${VG:-./target/release/vg}
WORK=${WORK:-target/site/graphe}
mkdir -p "$WORK" site/data

# 1. Extract the 13 ops of @soft_knee from examples/audio_limiter.vgs
awk '
  /^NODE @soft_knee / { in_node = 1 }
  in_node && /BODY \{/ { in_body = 1; next }
  in_body && /\}/ { in_body = 0; in_node = 0 }
  in_body {
    sub(/\/\/.*$/, "")
    if (match($0, /%[0-9]+[ \t]*=[ \t]*[A-Za-z0-9_.]+[ \t]*\([^)]*\);/)) {
      line = substr($0, RSTART, RLENGTH)
      sub(/;$/, "", line)
      split(line, lr, /=/)
      port = lr[1]; gsub(/[ \t]/, "", port)
      rhs = lr[2]; sub(/^[ \t]*/, "", rhs)
      p = index(rhs, "(")
      op = substr(rhs, 1, p - 1)
      args = substr(rhs, p + 1, length(rhs) - p - 1)
      gsub(/[ \t]/, "", args)
      printf "%s\t%s\t%s\n", port, op, args
    }
  }
' examples/audio_limiter.vgs >"$WORK/soft_knee_ops.tsv"

jq -R -s '
  split("\n") | map(select(length > 0)) | map(
    split("\t") | {
      port: .[0],
      op: .[1],
      operands: (.[2] | split(","))
    }
  )
' "$WORK/soft_knee_ops.tsv" >"$WORK/soft_knee_ops.json"

# 2. Hash alpha-equivalence variants, renamed node, and distinct family
sed 's/NODE @calc_checked {/NODE @somme_et_diff [intent: "Autre nom et autre description"] {/' \
  tests/corpus/alpha/a01_add_sub_checked/01.vgs >"$WORK/renamed.vgs"

hash_variant() {
  vid=$1
  title=$2
  summary=$3
  file=$4
  line=$("$VG" hash "$file" --root "$WORK")
  label=$(printf '%s\n' "$line" | awk '{print $2}')
  nh=$(printf '%s\n' "$line" | awk '{print $4}')
  bh=$(printf '%s\n' "$line" | awk '{print $6}')
  ch=$(printf '%s\n' "$line" | awk '{print $8}')
  jq -n \
    --arg id "$vid" \
    --arg title "$title" \
    --arg summary "$summary" \
    --arg file "$file" \
    --rawfile code "$file" \
    --arg label "$label" \
    --arg node "$nh" \
    --arg body "$bh" \
    --arg contract "$ch" \
    '{id: $id, title: $title, summary: $summary, file: $file, code: $code, label: $label, node: $node, body: $body, contract: $contract}'
}

: >"$WORK/variants.ndjson"
hash_variant "ref" "Référence (01.vgs)" "Forme de départ de la famille a01_add_sub_checked." "tests/corpus/alpha/a01_add_sub_checked/01.vgs" >>"$WORK/variants.ndjson"
hash_variant "alpha_swap" "Renommage + commutativité (02.vgs)" "Paramètres (a, b), ports (%10..%13), opérandes inversés dans ADD.checked, PRE scindée et attribut intent." "tests/corpus/alpha/a01_add_sub_checked/02.vgs" >>"$WORK/variants.ndjson"
hash_variant "alpha_dead" "Réordonnancement + IDENTITY + op morte (03.vgs)" "Ordre des instructions indépendantes modifié, opération pure dupliquée, IDENTITY et opération morte éliminée." "tests/corpus/alpha/a01_add_sub_checked/03.vgs" >>"$WORK/variants.ndjson"
hash_variant "renamed" "Étiquette et intent changés (@somme_et_diff)" "Copie de 01.vgs dont le label @calc_checked et l'intent ont été remplacés." "$WORK/renamed.vgs" >>"$WORK/variants.ndjson"
hash_variant "distinct" "Autre famille : mode .wrap (a02/01.vgs)" "Opérations ADD.wrap/SUB.wrap et contrat différent : empreinte distincte." "tests/corpus/alpha/a02_add_sub_wrap/01.vgs" >>"$WORK/variants.ndjson"

jq -s '
  .[0].node as $ref |
  map(. + {same_as_ref: (.node == $ref)})
' "$WORK/variants.ndjson" >"$WORK/variants.json"

# 3. Hash examples/mandelbrot.vgs call graph
"$VG" hash examples/mandelbrot.vgs --root "$WORK" >"$WORK/mandelbrot_hash.txt"
jq -R -s '
  split("\n") | map(select(length > 0)) | map(
    split(" ") | {
      label: .[1],
      node: .[3],
      short12: (.[3] | .[0:12]),
      body: .[5],
      contract: .[7]
    }
  )
' "$WORK/mandelbrot_hash.txt" >"$WORK/mandelbrot_hashes.json"

# 4. Assemble examples/quickstart.vgs into a clean store under $WORK/store
rm -rf "$WORK/store"
mkdir -p "$WORK/store"
"$VG" asm examples/quickstart.vgs --root "$WORK/store" >"$WORK/asm.out"

: >"$WORK/objects.ndjson"
find "$WORK/store/.vg/objects" -type f -name '*.vgir' | sort | while read -r f; do
  rel=${f#"$WORK/store/"}
  sz=$(wc -c <"$f" | tr -d ' ')
  jq -cn --arg path "$rel" --argjson bytes "$sz" '{path: $path, bytes: $bytes}' >>"$WORK/objects.ndjson"
done
jq -s '.' "$WORK/objects.ndjson" >"$WORK/objects.json"

# 5. Assemble deterministic graphe.json
jq -S -n \
  --slurpfile ops "$WORK/soft_knee_ops.json" \
  --slurpfile variants "$WORK/variants.json" \
  --slurpfile mandel "$WORK/mandelbrot_hashes.json" \
  --slurpfile objects "$WORK/objects.json" \
  --rawfile asm_out "$WORK/asm.out" \
  --rawfile vg_lock "$WORK/store/vg.lock" \
  '{
    soft_knee_ops: $ops[0],
    variants: $variants[0],
    mandelbrot_hashes: $mandel[0],
    store: {
      asm_out: $asm_out,
      vg_lock: $vg_lock,
      objects: $objects[0]
    }
  }' >site/data/graphe.json
