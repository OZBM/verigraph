# 07 · AI generation loop

Any AI can write VeriGraph from day one through tools: it drafts VG-Sym, the toolchain answers with structured errors and counterexamples, and the AI repairs until every obligation is proven. In-loop logit masking is a second mode, for local open-weight models.

| Aspect | Hosted mode [MVP] | Local mode [R-01] |
| --- | --- | --- |
| Models | Claude, Gemini, GPT or any API model | Open-weight models in vLLM, SGLang or llama.cpp |
| Mechanism | Generate, check, repair through MCP tools | A logits processor masks invalid tokens during decoding |
| Tier 1: structure | Grammar-constrained output where the API offers it, else `vg_check` | Grammar mask on every token, via llguidance or XGrammar |
| Tier 2: types and effects | `vg_check` after each draft | Masks port references, opcodes and labels that cannot type-check |
| Tier 3: proofs | `vg_verify` after each draft | SMT checks at edge-binding events, most likely candidates first |
| Guarantee | Nothing ships unverified | The same, with fewer repair rounds |

## Hosted loop

1. **Contract card.** The agent turns the request into effects, PRE, POST, COST and BUDGET. The human confirms the toolchain's rendering of the contract (`contract_text` from `vg_check`, docs/15), not the agent's own summary, so what is confirmed is what will be proven.
2. **Mesh first.** `vg_mesh_query` for each sub-problem with a known spec, such as hashing, codecs or linear algebra.
3. **Draft** the remaining nodes in VG-Sym.
4. **Check.** `vg_check` returns hashes, or structural, type and effect errors tied to op ids.
5. **Verify.** `vg_verify` returns a verdict per obligation; failures carry a counterexample and a failing slice.
6. **Repair** and repeat, by verdict (docs/05): a counterexample means fix the body, a contract gap means strengthen a callee's POST, unknown means split the obligation or guard the op. After 5 rounds without progress, the agent changes approach or asks the human.
7. **Ship.** `vg_build` and `vg_run` against tests and benchmarks, then `vg_mesh_publish`.

## MCP tools (`vg mcp`, stdio)

docs/15 gives the exact schemas, error codes, obligation IDs and JSON value encoding; this table is the overview.

| Tool | Input | Returns |
| --- | --- | --- |
| vg_check | `{source}` or `{path}` | `{ok, nodes: [{label, node_hash, contract_text, …}], diagnostics: [{code, node, op, span, message, fix?}]}` |
| vg_verify | `{path, obligations?, timeout_ms?, detail?}` | `{results: [{id, hash, verdict, code?, time_ms, report?}]}`; verdict is proven, counterexample, contract_gap, vacuous, unknown or error |
| vg_build | `{path, targets}` | `{artifacts: [{target, path, bytes}], manifest, residual_guards}` |
| vg_run | `{artifact or node, inputs}` | `{outputs, telemetry}` |
| vg_mesh_query | a MeshContractQuery (docs/09) | `{candidates: [{node_hash, spec_hash, cost, evidence, benchmarks}]}` |
| vg_mesh_publish | `{path}` | `{accepted: [hash], rejected: [{node, check, reason}]}` |
| vg_lift | `{path, function, language}` | `{node_hash, report: {bounded_check, unroll, fuzz_cases, ub_found}}` |
| vg_explain | `{obligation}` | a FailureReport, in JSON and in prose |

## Local mode, in detail

- **Tier 1** compiles the VG-Sym grammar for a grammar engine built for per-token masking.
- **Tier 2** asks vg-types which candidates for each port reference, opcode and label type-check, and masks the rest. Type-constrained decoding (PLDI 2025) and monitor-guided decoding (NeurIPS 2023) show this works for mainstream languages; VG-Sym makes it exact, because every prefix can be checked (ADR 0005) through the incremental API of docs/15.
- **Tier 3** runs only when a token binds an edge, never on every token. It checks the most likely candidates first and stops at the first one proven safe.
- **UNKNOWN never masks**, because a timeout is not a refutation; the final verifier still decides. Checks may run asynchronously while decoding continues, rolling back to the binding point if one is refuted.
- **Budgets:** Tiers 1 and 2 must add under 1 ms per token at p99. Tier 3 is judged end to end, with a target of under 30% throughput loss.
- **Known risk:** masking distorts the model's distribution and can trap it in dead ends (grammar-aligned decoding, NeurIPS 2024). R-01 measures both, and tests sequential Monte Carlo, which reweights partial programs instead of only masking them (Loula et al., 2025), as the correction.

## Counterfactual telemetry

A failure, at verify time or at a runtime guard, produces one report, whose schema is docs/15's FailureReport. The first draft of `@soft_knee` (examples/soft_knee_draft.vgs) gives, in short:

```json
{
  "node": "@soft_knee", "obligation": "@soft_knee/POST.0", "clause": "result >= -1.0",
  "verdict": "counterexample", "inputs": { "knee": "0.5", "x": "3e38" },
  "slice": [
    { "port": "%4", "op": "FDIV(%2, %3)", "value": "inf" },
    { "port": "%5", "op": "FADD(1.0:f32, %4)", "value": "inf" },
    { "port": "%6", "op": "FDIV(%4, %5)", "value": "nan" },
    { "port": "%12", "op": "FCLAMP(%11, -1.0:f32, 1.0:f32)", "value": "nan" }
  ],
  "diff": "for |x| > knee, the obligation holds iff %4 is finite",
  "reproduced": "concrete"
}
```

- **slice** is the backward cone of the failed predicate, pruned to the ops whose values matter.
- **diff** is the weakest condition on the slice that would make the obligation hold, derived from the solver. The fix itself stays the AI's job.
- **Runtime reports** add `"guard": "<guard id>"` and `"target"`. Release builds keep only residual guards; each maps to its obligation through the guard map, so a field failure yields the same report. An `ASSERT_GUARD.branch` taking its else branch is normal control flow and reports nothing. Captured inputs are opt-in and size-capped.
