# VeriGraph user guide

VeriGraph is a programming system in which a program is a typed dataflow graph that carries its own
contract. You write nodes in VG-Sym, a small text language. The `vg` toolchain checks them, proves each
contract with an SMT solver, runs them in a reference interpreter, and compiles them to native code.
A counterexample from the solver is replayed in the interpreter before it is reported, so a refutation
is always a real input that breaks the contract.

This guide covers version 0 of the toolchain, the first usable one. The chapters in `docs/00` to
`docs/16` are the specification, and they win wherever this guide simplifies them.

## Chapters

1. [Installation](install.md): the requirements, building `vg`, and checking that it works.
2. [Tutorial](tutorial.md): write a node, read a counterexample, repair it, run it, build it and call
   the binary.
3. [The VG-Sym language](language.md): modules, nodes, types, effects, contracts and the op set.
4. [Verification](verification.md): what `vg verify` proves, the obligations, the verdicts and what
   to do about each one.
5. [Command-line reference](cli.md): every `vg` command, with its options, JSON output and exit codes,
   and the input and output protocol of built executables.
6. [MCP server](mcp.md): the four tools that `vg mcp` gives an AI agent, and how to connect a client.
7. [Antigravity IDE](antigravity.md): the editor extension and the agent plugin, with installation,
   features, settings and troubleshooting.
8. [Diagnostics](diagnostics.md): every code `vg` reports, what it means and how to fix it.
9. [Troubleshooting and limits](troubleshooting.md): common problems, and what version 0 does not do
   yet.
10. [Example: a verified fractal renderer](examples.md): a Mandelbrot renderer whose loop bound,
    palette reads, colour conversions and opaque pixels are all proven, built for three targets.

The [quick start](../quickstart.md) is a one-page summary of chapters 2 and 5.

## Conventions

- Shell commands assume `vg` is on `PATH` (chapter 1) and run from the repository root.
- JSON results in this guide are real outputs of `vg`, sometimes shortened; `…` marks a cut.
- Every ```` ```vgsym ```` example in this guide is a complete module, checked by
  `crates/vg-cli/tests/guide_examples.rs` in CI. Its first line says what it must do:
  `// expect: verify`, `// expect: reject <obligation>`, `// expect: contract_gap <obligation>`, or
  `// expect: error <code>`. An example without that line must pass `vg check`.
