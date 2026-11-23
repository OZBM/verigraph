//! Compiles textual LLVM IR with clang and links it with vg-rt: aarch64-apple-darwin with the host
//! clang, wasm32-wasip1 with Homebrew LLVM's clang and the WASI sysroot of the wasi-libc and
//! wasi-runtimes formulas, x86_64-unknown-linux-gnu with `zig cc`, which carries glibc. A target
//! whose toolchain is missing gets an object file, reported as not linked.

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

/// A compiler driver: a program and the arguments that come before the rest (`zig cc`).
#[derive(Clone, Debug)]
struct Cc {
    prog: PathBuf,
    pre: Vec<String>,
}

fn run_clang(clang: &Path, args: &[String]) -> Result<(), Diagnostic> {
    run_cc(
        &Cc {
            prog: clang.to_path_buf(),
            pre: vec![],
        },
        args,
    )
}

fn run_cc(cc: &Cc, args: &[String]) -> Result<(), Diagnostic> {
    let out = Command::new(&cc.prog)
        .args(&cc.pre)
        .args(args)
        .output()
        .map_err(|e| {
            Diagnostic::error(
                "VG-B0002",
                Span::default(),
                format!("failed to invoke {}: {e}", cc.prog.display()),
            )
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Diagnostic::error(
            "VG-B0002",
            Span::default(),
            format!("{} failed: {}", cc.prog.display(), stderr.trim()),
        ));
    }
    Ok(())
}

fn runs(prog: &str, arg: &str) -> bool {
    Command::new(prog)
        .arg(arg)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// How one target is linked: the C compiler for vg-rt and the linker driver, each with its target
/// arguments, and the libraries to link.
struct Linker {
    cc: Cc,
    target_args: Vec<String>,
    libs: Vec<String>,
}

/// Homebrew's LLVM clang, which builds wasm32-wasip1 (toolchain.lock); Apple clang before
/// Xcode 27 has no wasm32 target (CI run 45258918686: none of Xcode 26.0 to 26.6 has it).
fn llvm_clang() -> Option<&'static str> {
    [
        "/opt/homebrew/opt/llvm/bin/clang",
        "/usr/local/opt/llvm/bin/clang",
    ]
    .into_iter()
    .find(|c| runs(c, "--version"))
}

/// The linker of a target, when its toolchain is installed.
fn linker(target: Target, host_clang: &Path) -> Option<Linker> {
    let clang_args = || {
        let mut a = vec![format!("--target={}", target.clang_target())];
        a.extend(target.clang_cpu_flags().iter().map(|f| f.to_string()));
        a
    };
    match target {
        Target::Aarch64Darwin => Some(Linker {
            cc: Cc {
                prog: host_clang.to_path_buf(),
                pre: vec![],
            },
            target_args: clang_args(),
            libs: vec![],
        }),
        Target::Wasm32Wasi => {
            let clang = llvm_clang()?;
            // The wasi-libc and wasi-runtimes formulas install this sysroot and the clang config
            // that points wasm32-wasip1 at it and at its compiler-rt builtins.
            let sysroot = Path::new(clang)
                .ancestors()
                .nth(4)?
                .join("share/wasi-sysroot");
            if !sysroot.join("lib/wasm32-wasip1/libc.a").is_file() {
                return None;
            }
            Some(Linker {
                cc: Cc {
                    prog: PathBuf::from(clang),
                    pre: vec![],
                },
                target_args: clang_args(),
                libs: vec!["-lm".into()],
            })
        }
        Target::X86_64Linux => {
            let zig = ["/opt/homebrew/bin/zig", "/usr/local/bin/zig", "zig"]
                .into_iter()
                .find(|z| runs(z, "version"))?;
            Some(Linker {
                cc: Cc {
                    prog: PathBuf::from(zig),
                    pre: vec!["cc".into()],
                },
                // zig names the x86-64-v3 baseline (docs/06) its own way.
                target_args: vec![
                    "-target".into(),
                    "x86_64-linux-gnu".into(),
                    "-mcpu=x86_64_v3".into(),
                ],
                libs: vec!["-lm".into()],
            })
        }
    }
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

    let Some(linker) = linker(opts.target, &clang) else {
        // No toolchain to link this target on this host: the object file alone.
        let mut obj_args = base_flags;
        obj_args.extend([
            "-c".to_string(),
            ll_path.display().to_string(),
            "-o".to_string(),
            out.display().to_string(),
        ]);
        let obj_clang = match opts.target {
            Target::Wasm32Wasi => llvm_clang().map_or(clang, PathBuf::from),
            _ => clang,
        };
        run_clang(&obj_clang, &obj_args)?;
        return Ok(BuildReport {
            artifact: out.to_path_buf(),
            linked: false,
            residual_guards: emitted.residual_guards,
        });
    };

    let mod_obj = tmp.0.join("module.o");
    let mut mod_args = base_flags;
    mod_args.extend([
        "-c".to_string(),
        ll_path.display().to_string(),
        "-o".to_string(),
        mod_obj.display().to_string(),
    ]);
    let ir_cc = if opts.target == Target::Wasm32Wasi {
        linker.cc.clone()
    } else {
        Cc {
            prog: clang.clone(),
            pre: vec![],
        }
    };
    run_cc(&ir_cc, &mod_args)?;

    let rt_c = tmp.0.join("vg_rt.c");
    let rt_obj = tmp.0.join("vg_rt.o");
    for (path, text) in [
        (tmp.0.join("vg_rt.h"), vg_rt::VG_RT_H),
        (rt_c.clone(), vg_rt::VG_RT_C),
    ] {
        std::fs::write(&path, text).map_err(|e| {
            Diagnostic::error(
                "VG-B0002",
                Span::default(),
                format!("failed to write {}: {e}", path.display()),
            )
        })?;
    }
    let mut rt_args = vec![
        "-O2".to_string(),
        "-ffp-contract=off".to_string(),
        "-std=c11".to_string(),
        "-Wall".to_string(),
        "-Wextra".to_string(),
    ];
    rt_args.extend(linker.target_args.iter().cloned());
    rt_args.extend([
        "-c".to_string(),
        rt_c.display().to_string(),
        "-o".to_string(),
        rt_obj.display().to_string(),
    ]);
    run_cc(&linker.cc, &rt_args)?;

    let mut link_args = linker.target_args.clone();
    link_args.extend([
        mod_obj.display().to_string(),
        rt_obj.display().to_string(),
        "-o".to_string(),
        out.display().to_string(),
    ]);
    link_args.extend(linker.libs.iter().cloned());
    run_cc(&linker.cc, &link_args)?;

    Ok(BuildReport {
        artifact: out.to_path_buf(),
        linked: true,
        residual_guards: emitted.residual_guards,
    })
}
