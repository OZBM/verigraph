//! Invokes Apple clang 21 to compile textual LLVM IR and link vg-rt where supported.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use vg_core::diag::{Diagnostic, Span};
use vg_core::ir::{Hash, Node};

use crate::emit::emit_module;
use crate::{BuildReport, Options, Target};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn create() -> Result<Self, Diagnostic> {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("vg-codegen-{pid}-{seq}"));
        std::fs::create_dir_all(&dir).map_err(|e| {
            Diagnostic::error(
                "VG-B0002",
                Span::default(),
                format!("failed to create build temp dir: {e}"),
            )
        })?;
        Ok(Self(dir))
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn find_clang() -> Result<PathBuf, Diagnostic> {
    for cand in ["/usr/bin/clang", "clang"] {
        let out = Command::new(cand).arg("--version").output();
        if let Ok(o) = out
            && o.status.success()
        {
            return Ok(PathBuf::from(cand));
        }
    }
    Err(Diagnostic::error(
        "VG-B0002",
        Span::default(),
        "clang not found on host",
    ))
}

fn run_clang(clang: &Path, args: &[String]) -> Result<(), Diagnostic> {
    let out = Command::new(clang).args(args).output().map_err(|e| {
        Diagnostic::error(
            "VG-B0002",
            Span::default(),
            format!("failed to invoke clang: {e}"),
        )
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Diagnostic::error(
            "VG-B0002",
            Span::default(),
            format!("clang failed: {}", stderr.trim()),
        ));
    }
    Ok(())
}

pub fn build_artifact(
    root: &Node,
    callee: &dyn Fn(&Hash) -> Option<Node>,
    opts: &Options,
    out: &Path,
) -> Result<BuildReport, Diagnostic> {
    let emitted = emit_module(root, callee, opts)?;
    let clang = find_clang()?;
    let tmp = TempDirGuard::create()?;

    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            Diagnostic::error(
                "VG-B0002",
                Span::default(),
                format!("failed to create output directory: {e}"),
            )
        })?;
    }

    let ll_path = tmp.0.join("module.ll");
    std::fs::write(&ll_path, &emitted.ir).map_err(|e| {
        Diagnostic::error(
            "VG-B0002",
            Span::default(),
            format!("failed to write LLVM IR: {e}"),
        )
    })?;

    let mut base_flags = vec![
        "-O2".to_string(),
        "-ffp-contract=off".to_string(),
        "-Wno-override-module".to_string(),
        format!("--target={}", opts.target.clang_target()),
    ];
    for f in opts.target.clang_cpu_flags() {
        base_flags.push((*f).to_string());
    }

    match opts.target {
        Target::Aarch64Darwin => {
            let mod_obj = tmp.0.join("module.o");
            let mut mod_args = base_flags.clone();
            mod_args.extend([
                "-c".to_string(),
                ll_path.display().to_string(),
                "-o".to_string(),
                mod_obj.display().to_string(),
            ]);
            run_clang(&clang, &mod_args)?;

            let rt_h = tmp.0.join("vg_rt.h");
            let rt_c = tmp.0.join("vg_rt.c");
            let rt_obj = tmp.0.join("vg_rt.o");
            std::fs::write(&rt_h, vg_rt::VG_RT_H).map_err(|e| {
                Diagnostic::error(
                    "VG-B0002",
                    Span::default(),
                    format!("failed to write vg_rt.h: {e}"),
                )
            })?;
            std::fs::write(&rt_c, vg_rt::VG_RT_C).map_err(|e| {
                Diagnostic::error(
                    "VG-B0002",
                    Span::default(),
                    format!("failed to write vg_rt.c: {e}"),
                )
            })?;

            let mut rt_args = vec![
                "-O2".to_string(),
                "-ffp-contract=off".to_string(),
                "-std=c11".to_string(),
                "-Wall".to_string(),
                "-Wextra".to_string(),
                format!("--target={}", opts.target.clang_target()),
            ];
            for f in opts.target.clang_cpu_flags() {
                rt_args.push((*f).to_string());
            }
            rt_args.extend([
                "-c".to_string(),
                rt_c.display().to_string(),
                "-o".to_string(),
                rt_obj.display().to_string(),
            ]);
            run_clang(&clang, &rt_args)?;

            let mut link_args = vec![format!("--target={}", opts.target.clang_target())];
            for f in opts.target.clang_cpu_flags() {
                link_args.push((*f).to_string());
            }
            link_args.extend([
                mod_obj.display().to_string(),
                rt_obj.display().to_string(),
                "-o".to_string(),
                out.display().to_string(),
            ]);
            run_clang(&clang, &link_args)?;

            Ok(BuildReport {
                artifact: out.to_path_buf(),
                linked: true,
                residual_guards: emitted.residual_guards,
            })
        }
        Target::X86_64Linux | Target::Wasm32Wasi => {
            // No Linux sysroot or wasi-sdk/wasm-ld installed on host: emit object file (`-c`).
            let mut obj_args = base_flags;
            obj_args.extend([
                "-c".to_string(),
                ll_path.display().to_string(),
                "-o".to_string(),
                out.display().to_string(),
            ]);
            run_clang(&clang, &obj_args)?;

            Ok(BuildReport {
                artifact: out.to_path_buf(),
                linked: false,
                residual_guards: emitted.residual_guards,
            })
        }
    }
}
