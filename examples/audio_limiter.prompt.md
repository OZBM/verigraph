# Audio limiter — the M1 request

Paste this into the hosted loop (the "Build an app in VeriGraph" prompt in PROMPTS.md):

> Write a verified real-time audio limiter and packet integrity module that takes a 512-sample stereo FP32 audio buffer, clamps peaks smoothly using a learnable knee threshold so no sample ever exceeds [-1.0, 1.0], computes a SHA-256 integrity digest of the output frame, and runs in under 500 microseconds with zero heap allocation.

This is the source spec's request, with one honest change: "guarantees execution in under 500 microseconds" became "runs in under 500 microseconds", because latency is measured (BUDGET), not proven (docs/05).

The expected contract card:

| Field | Value |
| --- | --- |
| Inputs | `frame: Tensor<f32, [512, 2]>` |
| Outputs | the limited frame, and `Array<u8, 32>` |
| Effects | `[]` |
| PRE | every sample finite |
| POST | every output sample in [−1, 1], sign preserved per sample; digest equals SHA-256 of the output bytes |
| COST | `heap_bytes == 0 && arena_bytes == 0` |
| BUDGET | `latency_p99_us <= 500` on each target |
| LEARNABLE | `knee: f32 in [0.5, 0.99]`, frozen in release builds; no loss until tuning lands (P9, R-07) |
| Mesh queries | SHA-256 |

The reference solution is examples/audio_limiter.vgs. M1 also requires the counterexample-and-repair story: the loop must reject examples/soft_knee_draft.vgs with a reproducing counterexample before reaching the repaired kernel.
