//! The `vg` command. Subcommands arrive with P1.10 (`vg asm`, `vg disasm`, `vg hash`).

fn main() {
    eprintln!(
        "vg {}: no commands are implemented yet",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(2);
}
