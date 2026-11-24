#!/bin/sh
# Usage: scripts/bench.sh [KERNEL...]. Runs the benchmark kernels of docs/16 ("Benchmarks"): each benchmarks/kernels/<name>/ names a
# verified VG-Sym node (bench.conf) and holds a C harness with the equivalent C (bench.c). The
# node is proven, built with proof facts and linked into the harness, which calls both sides
# in-process and prints one JSON line (benchmarks/kernels/bench.h). Native host target always;
# wasm32-wasip1 under wasmtime when its toolchain is installed (WASM ratios are reported, not
# gated). Writes reports/bench/latest.json and latest.md. Exits non-zero when a kernel fails to
# verify or build, or its two sides disagree on the bytes; never because of a ratio.
set -eu
cd "$(dirname "$0")/.."
cargo build --release -p vg-cli --locked -q
VG=target/release/vg
work=target/bench
rm -rf "$work"
mkdir -p "$work" reports/bench

host=$(rustc -vV | sed -n 's/^host: //p')
# The C compiler and CPU baseline vg-codegen uses for each target (docs/06), so both sides of a
# benchmark are compiled alike: clang -O2 -ffp-contract=off, no -ffast-math.
case "$host" in
  aarch64-apple-darwin) targets="aarch64-apple-darwin" ;;
  x86_64-unknown-linux-gnu) targets="x86_64-unknown-linux-gnu" ;;
  *) echo "bench.sh: no native target for host $host" >&2; exit 1 ;;
esac
llvm_clang=""
for c in /opt/homebrew/opt/llvm/bin/clang /usr/local/opt/llvm/bin/clang; do
  [ -x "$c" ] && llvm_clang=$c && break
done
if [ -n "$llvm_clang" ] && command -v wasmtime >/dev/null 2>&1; then
  targets="$targets wasm32-wasip1"
fi

cc_for() {
  case "$1" in
    aarch64-apple-darwin) echo "/usr/bin/clang -mcpu=apple-m1" ;;
    x86_64-unknown-linux-gnu) echo "clang -march=x86-64-v3" ;;
    wasm32-wasip1) echo "$llvm_clang -msimd128 -mno-relaxed-simd" ;;
  esac
}

lines="$work/lines.jsonl"
: >"$lines"
status=0
# With arguments, only the kernels they name.
kernels="$*"
for dir in benchmarks/kernels/*/; do
  k=$(basename "$dir")
  if [ -n "$kernels" ] && ! echo " $kernels " | grep -q " $k "; then continue; fi
  [ -f "$dir/bench.conf" ] || continue
  SRC="" ROOT="" DEFINES=""
  . "$dir/bench.conf"
  kw="$work/$k"
  mkdir -p "$kw"
  # Proven first, with a generous limit; the build then reuses those proof records.
  if ! "$VG" verify "$SRC" --root "$kw" --timeout 120000 --detail 0 >"$kw/verify.json" ||
     ! grep -q '"ok": true' "$kw/verify.json"; then
    echo "bench.sh: $k: $SRC does not verify (see $kw/verify.json)" >&2
    status=1
    continue
  fi
  for t in $targets; do
    out="$kw/$t"
    mkdir -p "$out"
    if ! "$VG" build "$SRC" "$ROOT" --target "$t" --root "$kw" --emit-llvm -o "$out" >"$out/build.json" ||
       ! grep -q '"verified": true' "$out/build.json"; then
      echo "bench.sh: $k: build for $t failed (see $out/build.json)" >&2
      status=1
      continue
    fi
    label=$(echo "$ROOT" | sed 's/^@//')
    ll="$out/$label-$t.ll"
    # The module's own entry point gives way to the harness's main; the root's checked entry is
    # the vg_<12 hex> function that main calls.
    sed -e 's/^define i32 @main(/define i32 @vg_module_main(/' \
        -e 's/^define i32 @__main_argc_argv(/define i32 @vg_module_main(/' "$ll" >"$out/module.ll"
    entry=$(grep -o 'call i32 @vg_[0-9a-f]\{12,\}(' "$out/module.ll" | head -n 1 | sed 's/call i32 @//; s/($//')
    cc=$(cc_for "$t")
    extra=""
    [ "$t" = wasm32-wasip1 ] && extra="-Wl,-z,stack-size=8388608"
    # shellcheck disable=SC2086
    if ! $cc -O2 -ffp-contract=off -Wno-override-module --target="$t" -c "$out/module.ll" -o "$out/module.o" ||
       ! eval "$cc -O2 -ffp-contract=off --target=$t -I runtime -DVG_TARGET='\"$t\"' -DVG_ENTRY=$entry $DEFINES \
           $dir/bench.c runtime/vg_rt.c $out/module.o -lm $extra -o $out/bench"; then
      echo "bench.sh: $k: the harness for $t does not build" >&2
      status=1
      continue
    fi
    if [ "$t" = wasm32-wasip1 ]; then
      run="wasmtime run $out/bench"
    else
      run="$out/bench"
    fi
    if ! $run >"$out/line.json" 2>"$out/stderr.txt"; then
      echo "bench.sh: $k: the harness for $t failed: $(cat "$out/stderr.txt")" >&2
      status=1
      continue
    fi
    cat "$out/line.json" >>"$lines"
    grep -q '"identical_output":true' "$out/line.json" || { echo "bench.sh: $k on $t: outputs differ: $(cat "$out/stderr.txt")" >&2; status=1; }
    echo "$(cat "$out/line.json")"
  done
done

cpu=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || sed -n 's/^model name[[:space:]]*: //p' /proc/cpuinfo 2>/dev/null | head -n 1)
os=$(uname -sr)
clang_v=$(/usr/bin/env clang --version 2>/dev/null | head -n 1)
python3 - "$lines" "$cpu" "$os" "$clang_v" <<'PY'
import json, sys
lines, cpu, os_, clang = sys.argv[1:5]
kernels = [json.loads(l) for l in open(lines) if l.strip()]
host = {"cpu": cpu, "os": os_, "clang": clang, "flags": "-O2 -ffp-contract=off, the target's CPU baseline (docs/06)"}
json.dump({"host": host, "kernels": kernels}, open("reports/bench/latest.json", "w"), indent=2)
open("reports/bench/latest.json", "a").write("\n")
rows = "\n".join(
    f"| {k['name']} | `{k['target']}` | {k['median_vg_ns']/1e3:,.1f} | {k['median_c_ns']/1e3:,.1f} | "
    f"{k['ratio']:.3f} | [{k['ci_low']:.3f}, {k['ci_high']:.3f}] | {'yes' if k['identical_output'] else 'NO'} |"
    for k in kernels)
open("reports/bench/latest.md", "w").write(f"""# Benchmarks

Generated by `scripts/bench.sh` (docs/16, "Benchmarks"). Host: {cpu}, {os_}; {clang}.
Each kernel is called in-process, alternating with its C equivalent for 10 rounds of 100 warm-up
calls and then up to 1,000 calls or 1 second; the ratio is median(VeriGraph) / median(C), with a
seeded bootstrap 95% interval. G4 asks for a ratio of 1.20 or less on both native targets; WASM is
reported only. Timings vary from run to run and machine to machine; the outputs do not.

| Kernel | Target | VeriGraph (µs) | C (µs) | Ratio | 95% interval | Same bytes |
| --- | --- | --- | --- | --- | --- | --- |
{rows}
""")
PY
echo "wrote reports/bench/latest.json and reports/bench/latest.md"
exit $status
