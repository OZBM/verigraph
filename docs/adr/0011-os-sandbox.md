# 0011 · The OS sandbox of a built program

Status: proposed on 2026-11-24 for the owner's approval; applied to runtime/vg_rt.c, vg-codegen's
entry point and `vg build`, and documented in docs/guide/cli.md. docs/04 ("Sandbox manifests") left
the macOS mechanism to P4.6; this records the choice and its evidence.

## Context

docs/04 says vg-rt opens a root's handles first and then drops all other authority, so the process
keeps exactly the descriptors its manifest names. Before P4.6, vg-rt took handle bindings from
undocumented `VG_HANDLE_<i>` environment variables, fell back to stdin and stdout, and applied no
sandbox; no test built a root with handles.

On macOS two mechanisms exist. App Sandbox entitlements take effect at launch and grant only fixed
paths or the app container, which cannot express "the file named on the command line".
`sandbox_init` with a profile string is deprecated but still works on the pinned macOS: probed on
2026-11-24 (macOS 27.3, Xcode 27.2), a process that opened a file, then called
`sandbox_init("(version 1)(deny default)", 0, &err)`, could still read that descriptor, write
stdout, read the clock and allocate, and every later `open` and file creation failed with EPERM.

## Decision

- **Binding.** A root's FileIn and FileOut handles bind, in parameter order, to the next
  command-line argument; `-` means stdin for a FileIn and stdout for a FileOut; a Clock takes no
  argument. A FileOut appends. A missing argument or a path that cannot be opened ends the process
  with exit code 5 before the root runs. The environment-variable bindings are removed.
- **Manifest.** `vg build` writes `<label>.manifest.toml` beside the executables, in docs/04's
  format, with `path = { argv = N }` for each file handle, and reports its path as `manifest`.
  The `fixed` and named `stdin` forms of docs/04 are not generated yet.
- **Sandbox.** The entry point calls `vg_rt_sandbox()` after every handle is open and before the
  root runs, in every executable, pure ones included.
  - macOS: `sandbox_init` with `(version 1)(deny default)`.
  - Linux: a Landlock ruleset that handles every filesystem right of the kernel's ABI and grants
    none, and from ABI 4 the TCP bind and connect rights too; then a seccomp filter that kills a
    wrong-architecture call, refuses x32 numbers, and makes `socket` fail with EPERM. A kernel
    without Landlock makes the program exit with code 5: it never runs unsandboxed.
  - WASM: nothing in vg-rt; WASI preview 1 holds only the directories the runner preopens
    (`wasmtime --dir`), so a path outside them cannot be opened.
- Any failure to apply the sandbox is exit code 5, like a handle that cannot be opened.

## Consequences

`crates/vg-cli/tests/sandbox.rs` shows each target refusing an ungranted file: on macOS and Linux a
probe linked with vg-rt opens its granted handle, sandboxes itself and then fails to open another
file (and on Linux a socket); under wasmtime the root writes inside its preopened directory and is
refused outside it. The Linux test runs only on a Linux host and fails, never skips, without
Landlock. Programs that need a path fixed at build time, or a network, wait for the manifest forms
and NET effects that docs/04 lists. If Apple removes `sandbox_init`, macOS programs will exit with
code 5 rather than run unconfined, and this decision must be revisited.
