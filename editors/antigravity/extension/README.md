# VeriGraph for Antigravity IDE

Language support for VG-Sym (`.vgs`), the text form of VeriGraph programs. It runs the `vg` toolchain and
shows its results in the editor. It works in Antigravity IDE and in any VS Code-compatible editor.

- **Highlighting** of keywords, contract clauses, opcodes and their modes, labels, ports, types and typed
  literals.
- **Diagnostics**: `vg check` runs when a file opens and on every save. Its errors appear as squiggles and
  in the Problems panel, with their `VG-…` code.
- **Verify File** (✓ button in the editor title): `vg verify` proves every obligation with z3. Each
  obligation that is not proven becomes a diagnostic on its node, with the counterexample inputs and
  whether vg-interp reproduced them. The full list goes to the VeriGraph output channel.
- **Run Node…** (▶ button): pick a node, edit the JSON inputs (prefilled from its signature) and see the
  outputs, and whether PRE and POST hold.
- **Build Node…** (package button): pick a node and its targets. A node that is not fully proven builds
  only if you choose *Build with runtime checks*.

## Settings

| Setting | Default | Meaning |
| --- | --- | --- |
| `verigraph.vgPath` | empty | Path to `vg`. When empty: `~/.cargo/bin/vg` if it exists, else `vg` on `PATH`. |
| `verigraph.checkOnSave` | `true` | Run `vg check` on save. |
| `verigraph.verifyOnSave` | `false` | Run `vg verify` instead of `vg check` on save. |
| `verigraph.verifyTimeoutMs` | `10000` | Solver timeout per obligation, in milliseconds. |
| `verigraph.buildOutputDir` | `target/vg` | Where Build Node writes binaries, relative to the workspace folder. |

## Requirements

`vg` and z3 5.4.1. `editors/antigravity/install.sh` installs `vg`, this extension and the Antigravity agent
plugin. The VeriGraph user guide (`docs/guide/`) explains everything else.
