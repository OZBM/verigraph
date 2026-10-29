# R-01 · SMT-guided constrained decoding

**Question.** Does Tier 2 and Tier 3 masking inside decoding beat the hosted generate-verify-repair loop on the same model?
**Why it is research.** Per-token SMT is far too slow, hosted APIs expose no logits, and masking can distort the model's distribution.
**Fallback that ships.** Hosted mode (docs/07).  **Time-box:** 4 weeks.  **Starts after:** P5.

## Proposed solution

Tier 1 compiles the VG-Sym grammar for llguidance or XGrammar. Tier 2 is an incremental vg-types state that lists the port references, opcodes and labels that can still type-check. Tier 3 checks only edge bindings, most likely candidate first, with a cache and asynchronous rollback; UNKNOWN never masks. Distortion is measured, and corrected if it hurts, either by grammar-aligned sampling or by sequential Monte Carlo, which carries several partial programs, weights each by how likely the model finds it under the constraints, and resamples (Loula et al., 2025, report small models beating models over 8× larger this way). VG-Sym's define-before-use rule (ADR 0005) makes Tier 2 exact, through the incremental checker of docs/15.

## Go if

At least 30% fewer repair rounds than the hosted loop with the same model; Tiers 1+2 under 1 ms per token at p99; throughput loss under 30%; zero unsound accepts.

## Starting points

- [XGrammar](https://arxiv.org/abs/2411.15100) and [llguidance](https://github.com/guidance-ai/llguidance)
- [Type-constrained code generation](https://arxiv.org/abs/2504.09246) (PLDI 2025)
- [Monitor-guided decoding](https://neurips.cc/virtual/2023/poster/70362) (NeurIPS 2023)
- [Grammar-aligned decoding](https://arxiv.org/abs/2405.21047) (NeurIPS 2024)
- [Syntactic and Semantic Control of Large Language Models via Sequential Monte Carlo](https://arxiv.org/abs/2504.13139) (Loula et al., 2025)
- [CRANE](https://arxiv.org/abs/2502.09061) (2025): leave room for unconstrained reasoning, here `//` comments, or constraints cost reasoning ability
- [Verifiers in the Loop](https://arxiv.org/abs/2605.17626) (2026): verification at structural boundaries with rollback, here VG-Sym statements

## Prompt

```text
Run research track R-01 (SMT-guided constrained decoding) following docs/research/PROTOCOL.md.
Question: does Tier 2 and Tier 3 masking inside decoding beat the hosted generate-verify-repair loop on the same model?
Build behind feature r-01: (1) export the VG-Sym grammar for llguidance and for XGrammar; (2) an incremental vg-types API, allowed_next(state) -> candidate ports, opcodes and labels; (3) a Tier 3 guard that checks edge bindings lazily, most likely candidate first, with a result cache and asynchronous rollback, where UNKNOWN never masks; (4) a logits processor for vLLM or llama.cpp that applies all three tiers.
Measure on benchmarks/synthesis (50 tasks) with one open-weight code model of 7-14B parameters: verified-success rate, repair rounds, tokens per second, p99 mask latency per tier, and distribution distortion on 5 tasks.
Baseline: hosted-mode loop with the same model.
Read first: XGrammar, llguidance, type-constrained code generation (PLDI 2025), monitor-guided decoding (NeurIPS 2023), grammar-aligned decoding (NeurIPS 2024), sequential Monte Carlo control of language models (Loula et al., 2025); if masking distorts the distribution, compare grammar-aligned sampling with SMC reweighting.
Go if: at least 30% fewer repair rounds, Tiers 1+2 under 1 ms per token at p99, throughput loss under 30%, zero unsound accepts. Otherwise keep hosted mode and explain why.
```
