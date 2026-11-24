#!/bin/sh
set -eu
VG=${VG:-./target/release/vg}
WORK=${WORK:-target/site/agent}
rm -rf "$WORK"
mkdir -p "$WORK" site/data

REPO_ROOT=$(pwd)
VG_ABS=$(cd "$(dirname "$VG")" && pwd)/$(basename "$VG")

# 1. Record real MCP handshake, tools/list, and one sample vg_run call over stdio
printf '%s\n%s\n%s\n%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"vg-site","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"vg_run","arguments":{"path":"'"$REPO_ROOT"'/examples/quickstart.vgs","node":"@soft_knee","inputs":{"x":"3e38","knee":"0.5"}}}}' \
  | (cd "$WORK" && "$VG_ABS" mcp) >"$WORK/mcp_out.jsonl"

jq -s '{init: .[0], list: .[1], sample_call: .[2]}' "$WORK/mcp_out.jsonl" >"$WORK/mcp.json"
jq -e '.list.result.tools | length == 8' "$WORK/mcp.json" >/dev/null

# 2. Run vg loop --replay on the three midpoint tasks from crates/vg-cli/tests/hosted_loop.rs
for t in a_solved b_weak c_wrong; do
  mkdir -p "$WORK/tasks/$t"
  cp benchmarks/synthesis/03_midpoint_u32/request.md "$WORK/tasks/$t/request.md"
  cp benchmarks/synthesis/03_midpoint_u32/contract.vgs "$WORK/tasks/$t/contract.vgs"
  cp benchmarks/synthesis/03_midpoint_u32/tests.json "$WORK/tasks/$t/tests.json"
done

OVERFLOWS='module m;
NODE @midpoint_u32 {
  SIGNATURE: (a: u32, b: u32) -> (u32);
  EFFECTS: [];
  CONTRACT { POST: result == (a + b) / 2; }
  BODY {
    %0 = ADD.checked(a, b);
    %1 = SHR.checked(%0, 1:u32);
    RETURN(%1);
  }
}'

WEAK='module m;
NODE @midpoint_u32 {
  SIGNATURE: (a: u32, b: u32) -> (u32);
  EFFECTS: [];
  CONTRACT { POST: true; }
  BODY {
    %0 = AND(a, b);
    %1 = XOR(a, b);
    %2 = SHR.checked(%1, 1:u32);
    %3 = ADD.checked(%0, %2);
    RETURN(%3);
  }
}'

WRONG='module m;
NODE @midpoint_u32 {
  SIGNATURE: (a: u32, b: u32) -> (u32);
  EFFECTS: [];
  CONTRACT { POST: result == a; }
  BODY {
    RETURN(a);
  }
}'

jq -n \
  --arg overflows "$OVERFLOWS" \
  --rawfile ref benchmarks/synthesis/03_midpoint_u32/reference.vgs \
  --arg weak "$WEAK" \
  --arg wrong "$WRONG" \
  '{
    a_solved: [
      {content: [{type: "text", text: "Checking."}, {type: "tool_use", id: "t1", name: "vg_verify", input: {source: $overflows}}], stop_reason: "tool_use", usage: {input_tokens: 1000, output_tokens: 200}},
      {content: [{type: "text", text: "Checking."}, {type: "tool_use", id: "t2", name: "vg_verify", input: {source: $ref}}], stop_reason: "tool_use", usage: {input_tokens: 1500, output_tokens: 300}},
      {content: [{type: "text", text: "Checking."}, {type: "tool_use", id: "t3", name: "submit", input: {source: $ref}}], stop_reason: "tool_use", usage: {input_tokens: 2000, output_tokens: 50}}
    ],
    b_weak: [
      {content: [{type: "text", text: "Checking."}, {type: "tool_use", id: "w1", name: "submit", input: {source: $weak}}], stop_reason: "tool_use", usage: {input_tokens: 900, output_tokens: 100}}
    ],
    c_wrong: [
      {content: [{type: "text", text: "Checking."}, {type: "tool_use", id: "r1", name: "submit", input: {source: $wrong}}], stop_reason: "tool_use", usage: {input_tokens: 900, output_tokens: 100}}
    ]
  }' >"$WORK/script.json"

"$VG" loop "$WORK/tasks" --replay "$WORK/script.json" -o "$WORK/loop_out" >"$WORK/loop_stdout.txt"
jq -e '.summary.success == 1 and .summary.unsound == 0' "$WORK/loop_out/results.json" >/dev/null

# 3. M1 story evidence: draft counterexample + audio_limiter verification
rc_explain=0
"$VG" explain examples/soft_knee_draft.vgs @soft_knee/POST.0 --root "$WORK" >"$WORK/draft_explain.json" || rc_explain=$?

mkdir -p "$WORK/m1"
"$VG" mesh publish examples/sha256.vgs --root "$WORK/m1" >"$WORK/m1_publish.json"
"$VG" mesh resolve examples/audio_limiter.vgs --root "$WORK/m1" >"$WORK/m1_resolve.json"
"$VG" verify examples/audio_limiter.vgs --mode edit --root "$WORK/m1" --detail 0 >"$WORK/m1_verify.json"
jq -e '.ok == true' "$WORK/m1_verify.json" >/dev/null

jq -n -S \
  --slurpfile mcp "$WORK/mcp.json" \
  --slurpfile loop_res "$WORK/loop_out/results.json" \
  --slurpfile loop_a "$WORK/loop_out/transcripts/a_solved.json" \
  --slurpfile draft_explain "$WORK/draft_explain.json" \
  --slurpfile m1_verify "$WORK/m1_verify.json" \
  --slurpfile m1_resolve "$WORK/m1_resolve.json" \
  '{
    mcp: {
      protocolVersion: $mcp[0].init.result.protocolVersion,
      tools: $mcp[0].list.result.tools,
      sample_call: $mcp[0].sample_call.result.structuredContent
    },
    loop_results: $loop_res[0],
    loop_first_verify: ($loop_a[0].messages[2].content[0].content | fromjson),
    draft_explain: $draft_explain[0],
    m1: {
      resolve: $m1_resolve[0],
      verify: {
        ok: $m1_verify[0].ok,
        nodes: $m1_verify[0].nodes,
        total_obligations: ($m1_verify[0].results | length),
        proven_obligations: ([$m1_verify[0].results[] | select(.verdict == "proven")] | length)
      }
    }
  }' >site/data/agent.json
