# Antigravity IDE

VeriGraph plugs into Antigravity IDE in two places:

- **The editor**: the VeriGraph extension highlights VG-Sym, shows `vg check` errors as you work, and
  adds buttons to verify, run and build.
- **The agent**: the VeriGraph plugin gives Antigravity's agent the `vg` MCP server and the
  verigraph-synthesis skill, so you can ask it for verified code in plain language.

The extension is a standard VS Code extension, so it also works in VS Code and its other forks. The
plugin uses Antigravity's global configuration, so the Antigravity app and the agy CLI get it too.

## Install

You need `vg`'s requirements ([Installation](install.md)) and Antigravity IDE in `/Applications`.
From the repository root:

```bash
editors/antigravity/install.sh
```

The script does three things, and can be run again after every update of the repository:

1. Builds `vg` and installs it in `~/.cargo/bin` (`cargo install --locked`), then checks
   `examples/quickstart.vgs` with it.
2. Packages `editors/antigravity/extension` as `target/antigravity/verigraph-0.1.0.vsix` and installs
   it with the IDE's command-line tool. The package is reproducible: the same sources give the same
   bytes.
3. Writes the plugin to `~/.gemini/config/plugins/verigraph`: `plugin.json`, the skill copied from
   `skills/verigraph-synthesis`, and an `mcp_config.json` that starts `vg mcp` from its absolute path.

Then reload the IDE window (command palette, *Developer: Reload Window*) or restart the IDE.

| Variable | Default | Use |
| --- | --- | --- |
| `ANTIGRAVITY_IDE_CLI` | `/Applications/Antigravity IDE.app/Contents/Resources/app/bin/antigravity-ide` | The IDE's command-line tool |
| `ANTIGRAVITY_PLUGINS` | `~/.gemini/config/plugins` | Where the plugin goes |
| `CARGO_HOME`, `CARGO_INSTALL_ROOT` | `~/.cargo` | Where `cargo install` puts `vg` |

## The editor extension

Open any `.vgs` file; the status bar shows the language as **VG-Sym**.

| Feature | What happens |
| --- | --- |
| Highlighting | Keywords, contract clauses, opcodes and their modes, labels, ports, types and typed literals each get their own color. |
| Diagnostics | `vg check` runs when a file opens and on every save. Errors appear as squiggles, in the Problems panel and in the file's tab, with their `VG-…` code. An error that `vg` places on a node, such as a type error, underlines the node's `NODE` line. |
| ✓ Verify File | `vg verify` runs with a progress notification. Each obligation that is not proven becomes a diagnostic on its node's line, with its verdict, code, counterexample inputs and whether vg-interp reproduced them; the full list goes to the **VeriGraph** output channel. |
| ▶ Run Node… | Pick a node (the one under the cursor comes first), then edit its inputs, prefilled from its signature in the JSON value forms. The outputs, and whether PRE and POST held, appear in a notification and the output channel. |
| Build Node… | Pick a node and its targets. A node that is not fully proven shows its failing obligations and offers *Build with runtime checks*. Binaries go to `target/vg` in the workspace. |

The three buttons sit in the editor's title bar when a `.vgs` file is active. The commands are also in
the command palette, under **VeriGraph**: *Check File*, *Verify File*, *Run Node…*, *Build Node…*.
Commands save the file first, since `vg` reads it from disk.

| Setting | Default | Meaning |
| --- | --- | --- |
| `verigraph.vgPath` | empty | Path to `vg`. When empty: `~/.cargo/bin/vg` if it exists, else `vg` on `PATH`. |
| `verigraph.checkOnSave` | `true` | Run `vg check` on save. |
| `verigraph.verifyOnSave` | `false` | Run `vg verify` instead of `vg check` on save. |
| `verigraph.verifyTimeoutMs` | `10000` | Solver timeout per obligation, in milliseconds. |
| `verigraph.buildOutputDir` | `target/vg` | Where Build Node writes binaries, relative to the workspace folder. |

## The agent plugin

The plugin appears as **VeriGraph** on Antigravity's Customizations page, where you can turn it off and
on. While it is on:

- The agent has the tools `vg_check`, `vg_verify`, `vg_run` and `vg_build` ([MCP server](mcp.md)).
- The agent has the **verigraph-synthesis** skill: write a contract first, check, verify, repair from
  counterexamples, never ship an unproven node.
- The plugin suggests prompts such as *"Write a VeriGraph node that adds two u32 with saturation, and
  prove it"* and *"Verify examples/soft_knee_draft.vgs and explain each counterexample"*.

A good request names the behavior and the guarantees, and lets the agent write the contract for you to
confirm: *"Write a node that returns the larger of two f32, never NaN when both inputs are finite;
prove it and build it for aarch64-apple-darwin."*

To confirm that the agent loaded the server, open a conversation and look for the four `vg_` tools,
or check that a `vg mcp` process runs under the agent:

```bash
ps -axo pid,ppid,args | grep "[v]g mcp"
```

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| *VeriGraph: spawn vg ENOENT* | The extension cannot find `vg`. Run the installer, or set `verigraph.vgPath`. |
| Every obligation comes back as `error` or `unknown` | z3 is missing: `brew install z3`, then check that `z3 --version` says 5.4.1. |
| No colors and no diagnostics | The extension is not loaded: check that `antigravity-ide --list-extensions` lists `verigraph.verigraph`, then reload the window. |
| The agent has no `vg_` tools | Check `~/.gemini/config/plugins/verigraph/mcp_config.json` names an existing `vg`, that the plugin is on in Customizations, and restart the IDE. |
| The agent mentions `contract_text`, Mesh or `vg_explain` | Those tools come after version 0; the skill says so, and the agent works without them. |

## Uninstall

```bash
"/Applications/Antigravity IDE.app/Contents/Resources/app/bin/antigravity-ide" --uninstall-extension verigraph.verigraph
```

```bash
rm -r ~/.gemini/config/plugins/verigraph
```
