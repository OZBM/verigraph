# R-06 · Resource and latency contracts

**Question.** How tight can static cost bounds be, and how can they become honest latency claims per target?
**Why it is research.** Wall-clock time on out-of-order CPUs with caches cannot be proven by SMT; sound WCET analysis is hard.
**Fallback that ships.** COST for memory and op counts; BUDGET checked by benchmark gates.  **Time-box:** 3 weeks.  **Starts after:** P4.

## Proposed solution

Give every op a cost in ops and bytes and prove symbolic bounds over the graph, as resource-aware type systems do. Calibrate cycles per op on each target with microbenchmarks to predict latency, then confirm by measurement. Real-time paths add a deadline guard with a verified fallback, which is the source spec's GracefulDegrade idea made honest.

## Go if

Static bounds within 2× of measured ops and bytes on 20 kernels; predicted latency within 25% of measured p50 on two targets; deadline-guard overhead under 2%.

## Starting points

- Resource Aware ML (automatic amortized resource analysis)
- WCET analysis surveys and tools for in-order cores

## Prompt

```text
Run research track R-06 (resource and latency contracts) following docs/research/PROTOCOL.md.
Question: how tight can static cost bounds be, and how can they become honest latency claims per target?
Build behind feature r-06: (1) a cost semantics per op and combinator with symbolic bounds in input sizes; (2) per-target calibration from microbenchmarks; (3) a latency predictor; (4) a deadline guard combinator with a verified fallback for real-time paths.
Corpus: 20 kernels, two targets (macOS ARM64 and Linux x86_64).
Measure: static bound versus measured ops and bytes, predicted versus measured p50 latency, deadline-guard overhead.
Read first: Resource Aware ML (automatic amortized resource analysis), WCET analysis surveys, worst-case execution time tools for in-order cores.
Go if: bounds within 2x of measured, predictions within 25% of measured p50, guard overhead under 2%. Otherwise COST covers memory only and BUDGET stays benchmark-gated.
```
