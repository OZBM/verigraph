# Installation

## Requirements

| Tool | Version | Used by | How to get it |
| --- | --- | --- | --- |
| Rust | 1.90.0, pinned in `rust-toolchain.toml` | building `vg` | [rustup](https://rustup.rs); rustup reads the pin |
| z3 | 5.4.1, pinned in `toolchain.lock` | `vg verify`, `vg build` | macOS: `brew install z3`. Linux: the release archive from the Z3 GitHub releases, as CI does (`.github/workflows/ci.yml`) |
| Bitwuzla | 0.11.0, pinned in `toolchain.lock` | float queries; `vg verify --mode release` | macOS: `brew install bitwuzla`. Optional: without it z3 takes the float queries |
| cvc5 | 1.4.2, pinned in `toolchain.lock` | `vg verify --mode release` | The release binary from the cvc5 GitHub releases. Optional: release mode needs two of the three solvers |
| clang | Apple clang 21 on macOS | `vg build` | An Xcode whose `clang --version` says 21, then `xcode-select --install`. Older clangs, such as Xcode 16's, have no wasm32 target: `--target wasm32-wasip1` then fails with VG-B0002 |

Version 0 is built for macOS on Apple Silicon. On Linux x86_64, `vg check`, `vg verify`, `vg run` and
`vg mcp` work, and `vg build` can only produce object files (see [Troubleshooting](troubleshooting.md)).

## Build and install `vg`

From the repository root:

```bash
cargo install --path crates/vg-cli --locked
```

This builds the release binary and copies it to `~/.cargo/bin/vg`. Rustup's installer puts
`~/.cargo/bin` on `PATH`. During development you can use `target/release/vg` instead:

```bash
cargo build --release -p vg-cli
```

`vg` finds each solver in `/opt/homebrew/bin`, then `/usr/local/bin`, then on `PATH`.

## Check the installation

```bash
vg check examples/quickstart.vgs
```

```bash
vg verify examples/quickstart.vgs
```

The first prints `"ok": true` and the five nodes of the module; the second proves every obligation of
those nodes in a few seconds and also ends with `"ok": true`. Both exit with code 0.

## Antigravity IDE

To use VeriGraph from Antigravity IDE, run the installer after the steps above:

```bash
editors/antigravity/install.sh
```

[Antigravity IDE](antigravity.md) describes what it installs.

## Uninstall

```bash
cargo uninstall vg-cli
```

The Antigravity chapter explains how to remove the extension and the plugin.
