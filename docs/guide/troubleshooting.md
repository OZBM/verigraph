# Troubleshooting and limits

## Common problems

**`vg verify` answers `error` or `unknown` for every obligation.** z3 is not found or does not run.
`vg` looks for `/opt/homebrew/bin/z3`, then `/usr/local/bin/z3`, then `z3` on `PATH`. Install z3 5.4.1
(`brew install z3` on macOS) and check `z3 --version`.

**An obligation is `unknown` with VG-V0004.** The solver timed out. Nonlinear integer arithmetic
(products and divisions of two variables) is the usual cause. State a weaker fact that still says what
callers need, split the conjunct, or raise `--timeout`. [Verification](verification.md#when-the-solver-says-unknown)
has the details.

**A counterexample has inputs such as `3e38` or `nan`.** Floats include huge values, infinities and
NaN, and the solver tries them all. Add what you assume to PRE (`is_finite(x)`, a range), or make the
body correct at the extremes.

**`vg check` reports VG-P0001 at a `%`.** `x%2` lexes as `x` followed by the port `%2`; write `x % 2`.
Likewise `t.0.1` lexes as `t`, `.`, `0.1`: write `(t.0).1`.

**VG-S0102 on an arithmetic op.** Every integer op names its mode: `ADD.checked`, `ADD.wrap` or
`ADD.sat`; every ordered comparison its kind: `LT.s`, `LT.u` or `LT.f`.

**VG-S0004.** A label is used before its definition. Move kernels and callees above their callers.

**`vg build` says "not every obligation is proven".** Fix the obligations first. To build anyway, with
a runtime check on every partial op, add `--unverified`.

**`vg build` for `x86_64-unknown-linux-gnu` or `wasm32-wasip1` gives `"linked": false`.** Version 0
compiles those targets to object files only; linking needs a Linux sysroot and wasi-sdk.

**VG-S0009.** A `@MESH_QUERY` label has no pin in `vg.lock`. Version 0 cannot query VG-Mesh yet, so
`examples/audio_limiter.vgs`'s `@limit_frame`, which calls a SHA-256 from Mesh, does not check;
its `@soft_knee` does, and `examples/quickstart.vgs` holds a copy.

**An obligation ID names `%3`, but the source has no op at port `%3`.** Version 0 numbers ops in
canonical order, not as written. Canonicalization sorts and renumbers ops, and drops the ones nothing
uses (VG-W0001). Use `vg disasm` on the node's hash to see the canonical form.

**The Antigravity extension or plugin does not work.** See the troubleshooting table in
[Antigravity IDE](antigravity.md#troubleshooting).

## What version 0 does not do yet

| Missing | Arrives with |
| --- | --- |
| COST obligations (reported as `unknown`) | P3 follow-up |
| cvc5 and Bitwuzla, small-model search, the proof cache | P3 follow-up |
| Plain-language `contract_text`, `fix` edits in diagnostics, failing slices (`vg explain`) | P5 |
| Telemetry and runtime guard reports | P6 |
| VG-Mesh queries and publishing, beyond `vg.lock` pins | P7 |
| Lifting existing code (`vg lift`) | P8 |
| Linked binaries for Linux x86_64 and WASM, sandbox manifests | P4 follow-up |
| Obligation IDs that use the ports written in the source | P3 follow-up |
| An LSP server (hover, completion, go to definition) in the editor | not planned yet |

ROADMAP.md tracks every phase, and PROGRESS.md what is done.
