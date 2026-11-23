//! Tiny C runtime packaged as a crate: entry point, arenas, capability handles, telemetry.

/// The C header for the VeriGraph runtime (`runtime/vg_rt.h`).
pub const VG_RT_H: &str = include_str!("../../../runtime/vg_rt.h");

/// The C implementation for the VeriGraph runtime (`runtime/vg_rt.c`).
pub const VG_RT_C: &str = include_str!("../../../runtime/vg_rt.c");

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    #[cfg_attr(
        not(all(target_os = "macos", target_arch = "aarch64")),
        ignore = "compiles for aarch64-apple-darwin, which needs the macOS SDK"
    )]
    fn packaged_c_sources_compile_cleanly_for_all_targets() {
        assert!(VG_RT_H.contains("vg_ctx"));
        assert!(VG_RT_C.contains("vg_rt_init"));
        assert!(VG_RT_C.contains("vg_emit_guard"));
        assert!(VG_RT_C.contains("vg_f16_to_f32"));
        assert!(VG_RT_C.contains("vg_u64_to_bf16"));

        let dir = std::env::temp_dir().join(format!("vg-rt-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let h_path = dir.join("vg_rt.h");
        let c_path = dir.join("vg_rt.c");
        std::fs::write(&h_path, VG_RT_H).unwrap();
        std::fs::write(&c_path, VG_RT_C).unwrap();

        let obj = dir.join("vg_rt.o");
        let out = Command::new("clang")
            .args([
                "-O2",
                "-ffp-contract=off",
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "--target=aarch64-apple-darwin",
                "-mcpu=apple-m1",
                "-c",
            ])
            .arg(&c_path)
            .arg("-o")
            .arg(&obj)
            .output()
            .expect("clang should run");
        assert!(
            out.status.success(),
            "clang failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(obj.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
