# 10 · Self-hosting bootstrap

The compiler is self-hosting when vgc2 and vgc3 are bit-identical: two binaries built from the same VeriGraph source by successive compiler generations. The source spec compared stage 1 with stage 2, which can legitimately differ.

| Stage | Built by | From | Output | Passes when |
| --- | --- | --- | --- | --- |
| 0 | cargo | The Rust workspace | vgc0 | Every gate through the alpha passes |
| 1 | vgc0 | vgc.vgir, the compiler written in VeriGraph | vgc1 | The full test suite passes |
| 2 | vgc1 | vgc.vgir | vgc2 | vgc1 and vgc2 emit identical LLVM IR on every test input |
| 3 | vgc2 | vgc.vgir | vgc3 | BLAKE3(vgc2) == BLAKE3(vgc3) |

Why stage 3: vgc1 is produced by Rust code, so its bytes may differ from vgc2's even when both are correct. Only two compilers built from the same source, by compilers that themselves share a source, must agree byte for byte.

- **Retired:** the Rust compiler logic. vgc0 is archived as the reference for diverse double-compiling: build vgc0 with two independent Rust toolchains, run both chains two stages, and require identical vgc2 bytes. This defends against a compromised bootstrap.
- **Kept as host services**, behind the HOST_CALL and SPAWN effects: the SMT solvers, LLVM, clang, the linker and the OS. Rewriting Z3 or LLVM is not part of the vision and would add years.
- **Reproducibility:** deterministic codegen, one pinned clang version and flag set, `-ffile-prefix-map`, no timestamps, one linker (lld).

The compiler needs language features the MVP lacks: byte buffers, persistent maps, SCC recursion with DECREASES, and file and process effects (R-11). Once written in VeriGraph, it verifies its own core invariants, starting with canonicalization being idempotent.
