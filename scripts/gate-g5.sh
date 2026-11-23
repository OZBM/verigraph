#!/bin/sh
# make gate-g5: the G5 criteria of docs/16 (agent loop) from reports/g5/results.json, which the
# hosted-loop harness writes; running it spends API credits, so this gate never starts it.
GATE=g5 TITLE="Agent loop"
. "$(dirname "$0")/gate-common.sh"
gate_start
results=reports/g5/results.json
if [ -f "$results" ]; then
  verdict=$(python3 - "$results" <<'PY'
import json, sys
j = json.load(open(sys.argv[1]))
ts = j["tasks"]
ok = [t for t in ts if t["success"] and t["verified"] and t["reference_contract"] and t["tests_passed"] and t["rounds"] <= 5]
unsound = [t["id"] for t in ts if t["unsound_accept"]]
claimed = [t["id"] for t in ts if t["success"] and t not in ok]
print("met" if len(ts) >= 30 and len(ok) >= 18 and not claimed else "not met")
print("met" if not unsound else "not met")
print(f"model {j['model']}: {len(ok)} of {len(ts)} tasks succeeded" + (f"; claimed but not shown: {claimed}" if claimed else ""))
print(f"unsound accepts: {unsound or 'none'}")
PY
)
  c1=$(echo "$verdict" | sed -n 1p); c2=$(echo "$verdict" | sed -n 2p)
  e1=$(echo "$verdict" | sed -n 3p); e2=$(echo "$verdict" | sed -n 4p)
else
  c1="not met"; c2="not met"; e1="no $results: the hosted loop has not been run"; e2="$e1"
fi
row "A hosted model verifies at least 18 of 30 tasks within 5 repair rounds, each against the task's reference contract and tests" "$c1" "$e1"
row "Zero unsound accepts" "$c2" "$e2"
gate_finish
