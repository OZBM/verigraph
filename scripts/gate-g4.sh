#!/bin/sh
# make gate-g4: the G4 criteria of docs/16 (codegen and runtime); writes reports/gate-g4.md.
# x86_64-unknown-linux-gnu executables run only on Linux: this gate takes that target's evidence
# from this host when it is Linux, and reports it not met otherwise.
GATE=g4 TITLE="Codegen and runtime"
. "$(dirname "$0")/gate-common.sh"
gate_start
host=$(rustc -vV | sed -n 's/^host: //p')

c=met
run differential env VG_DIFF_PROGRAMS=1000 VG_DIFF_INPUTS=100 cargo test -p vg-cli --locked --test differential -- --nocapture || c="not met"
summary=$(grep -E '^[0-9]+ programs \(' "$work/differential.log" | tail -n 1)
case "$summary" in 1000\ programs*) ;; *) c="not met" ;; esac
targets=$(echo "$summary" | grep -o 'runs per build: .*')
for t in aarch64-apple-darwin wasm32-wasip1 x86_64-unknown-linux-gnu; do
  echo "$summary" | grep -q "$t" || { c="not met"; targets="$targets; no run on $t on this host ($host)"; }
done
row "1,000 generated programs agree with vg-interp on three targets, with and without proof facts" "$c" "${summary:-no summary line}. $targets"

c=met
bench=reports/bench/latest.json
if [ -f "$bench" ]; then
  table=$(python3 - "$bench" <<'PY'
import json, sys
j = json.load(open(sys.argv[1]))
native = {"aarch64-apple-darwin", "x86_64-unknown-linux-gnu"}
ok = True
out = []
for t in sorted(native):
    ks = [k for k in j["kernels"] if k["target"] == t]
    if len(ks) < 5:
        ok = False
        out.append(f"{t}: {len(ks)} kernels measured")
    for k in ks:
        good = k["identical_output"] and k["ratio"] <= 1.20
        ok &= good
        out.append(f"{t} {k['name']}: ratio {k['ratio']:.3f} [{k['ci_low']:.3f}, {k['ci_high']:.3f}]{'' if good else ' FAILS'}")
print("met" if ok else "not met")
print("; ".join(out))
PY
)
  [ "$(echo "$table" | head -n 1)" = met ] || c="not met"
  detail=$(echo "$table" | tail -n +2)
else
  c="not met"; detail="no $bench: run scripts/bench.sh"
fi
row "Within 20% of clang -O2 C on five kernels, on both native targets" "$c" "$detail"

c=met
run sandbox env VG_REQUIRE_WASMTIME=1 cargo test -p vg-cli --locked --test sandbox || c="not met"
found=""
for t in macos linux wasm; do
  if test_ran sandbox "${t}_sandbox_refuses_an_ungranted_file"; then found="$found $t"; fi
done
for t in macos wasm; do echo "$found" | grep -q "$t" || c="not met"; done
echo "$found" | grep -q linux || { c="not met"; found="$found (linux: runs only on a Linux host)"; }
row "A root that opens an ungranted file is refused by the OS sandbox, on each target" "$c" "refusal tests passed here:${found:- none}"

gate_finish
