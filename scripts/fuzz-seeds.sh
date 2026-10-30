#!/bin/sh
# Seed corpora for the fuzz targets (docs/16): every VG-Sym file for `parse`, the .vgir of every
# file that assembles for `codec`, and fixed generator seeds for `canon`. Written to $1.
set -eu
cd "$(dirname "$0")/.."
out=${1:?usage: scripts/fuzz-seeds.sh DIR}
rm -rf "$out"
mkdir -p "$out/parse" "$out/codec" "$out/canon" "$out/store"
find examples tests/corpus tests/golden tests/lexical -name '*.vgs' | sort | while read -r f; do
  cp "$f" "$out/parse/$(echo "$f" | tr '/' '_')"
  ./target/debug/vg asm "$f" --root "$out/store" >/dev/null 2>&1 || true
done
find "$out/store/.vg/objects" -name '*.vgir' -exec cp {} "$out/codec/" \;
# Every crash ever found stays a seed (docs/16: crashes are added to the corpus).
for t in parse codec canon; do
  [ -d "fuzz/regressions/$t" ] && cp fuzz/regressions/$t/* "$out/$t/" 2>/dev/null
done
i=0
while [ $i -lt 32 ]; do
  printf '%08x%08x%08x%08x%02x' $i $((i * 7919)) $((i * 104729)) $((i ^ 0x5eed)) $((i * 37 % 256)) | xxd -r -p > "$out/canon/seed$i"
  i=$((i + 1))
done
rm -rf "$out/store"
