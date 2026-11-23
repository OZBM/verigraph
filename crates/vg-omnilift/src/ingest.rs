//! P8.1: Ingest C and Rust sources to normalized textual LLVM IR via clang / rustc + opt.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use vg_core::diag::{Diagnostic, Span};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    C,
    Rust,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Language::C => "c",
            Language::Rust => "rust",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub path: String,
    pub function: String,
    pub language: Language,
    pub compiler: String,
    pub compiler_args: Vec<String>,
    pub opt: String,
    pub opt_args: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct IngestedIr {
    pub llvm_ir: String,
    pub provenance: Provenance,
}

struct TempGuard(Vec<PathBuf>);

impl Drop for TempGuard {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn fresh_temp_path(stem: &str, ext: &str) -> PathBuf {
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("vg_omnilift_{pid}_{seq}_{stem}.{ext}"))
}

pub fn find_clang() -> PathBuf {
    let homebrew = PathBuf::from("/opt/homebrew/opt/llvm/bin/clang");
    if homebrew.is_file() {
        homebrew
    } else {
        PathBuf::from("clang")
    }
}

pub fn find_opt() -> PathBuf {
    let homebrew = PathBuf::from("/opt/homebrew/opt/llvm/bin/opt");
    if homebrew.is_file() {
        homebrew
    } else {
        PathBuf::from("opt")
    }
}

pub fn has_opt() -> bool {
    Command::new(find_opt())
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn find_rustc() -> PathBuf {
    PathBuf::from("rustc")
}

/// Default preprocessor flags for vendored C targets when the caller did not pass custom ones.
pub fn default_c_extra_args(path: &Path) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            args.push(format!("-I{}", parent.display()));
        }
        let shim_inc = parent.join("shim_include");
        if shim_inc.is_dir() {
            args.push(format!("-I{}", shim_inc.display()));
        }
    }
    if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
        if name == "crc32.c" {
            // Leave W and ARMCRC32 undefined so crc32_z uses the byte-wise table loop.
            args.push("-DZ_TESTW=0".to_string());
        } else if name == "sha256_shim.c" {
            args.push("-DMBEDTLS_SHA256_SMALLER".to_string());
        }
    }
    args
}

/// Compile `path` to LLVM IR with fixed docs/08 flags and normalize with `opt`.
pub fn ingest_to_llvm_ir(
    path: &Path,
    function: &str,
    language: Language,
    extra_args: &[String],
) -> Result<IngestedIr, Diagnostic> {
    if !path.is_file() {
        return Err(Diagnostic::error(
            "VG-L0001",
            Span::default(),
            format!("source file not found: {}", path.display()),
        ));
    }

    let raw_ll = fresh_temp_path("raw", "ll");
    let norm_ll = fresh_temp_path("norm", "ll");
    let _cleanup = TempGuard(vec![raw_ll.clone(), norm_ll.clone()]);

    let (compiler_path, mut compiler_args) = match language {
        Language::C => {
            let clang = find_clang();
            let mut args = vec![
                "-O1".to_string(),
                "-fno-unroll-loops".to_string(),
                "-fno-vectorize".to_string(),
                "-fno-slp-vectorize".to_string(),
                "-S".to_string(),
                "-emit-llvm".to_string(),
                "-target".to_string(),
                "aarch64-apple-darwin".to_string(),
            ];
            if extra_args.is_empty() {
                args.extend(default_c_extra_args(path));
            } else {
                if let Some(parent) = path.parent()
                    && !parent.as_os_str().is_empty()
                {
                    args.push(format!("-I{}", parent.display()));
                }
                args.extend(extra_args.iter().cloned());
            }
            args.push(path.display().to_string());
            args.push("-o".to_string());
            args.push(raw_ll.display().to_string());
            (clang, args)
        }
        Language::Rust => {
            let rustc = find_rustc();
            let mut args = vec![
                "--edition=2024".to_string(),
                "--crate-type=lib".to_string(),
                "--emit=llvm-ir".to_string(),
                "-C".to_string(),
                "opt-level=1".to_string(),
                "-C".to_string(),
                "llvm-args=-unroll-threshold=0".to_string(),
                "-C".to_string(),
                "no-vectorize-loops".to_string(),
                "-C".to_string(),
                "no-vectorize-slp".to_string(),
            ];
            args.extend(extra_args.iter().cloned());
            args.push(path.display().to_string());
            args.push("-o".to_string());
            args.push(raw_ll.display().to_string());
            (rustc, args)
        }
    };

    let compile_out = Command::new(&compiler_path)
        .args(&compiler_args)
        .output()
        .map_err(|e| {
            Diagnostic::error(
                "VG-L0001",
                Span::default(),
                format!("failed to invoke {}: {e}", compiler_path.display()),
            )
        })?;
    if !compile_out.status.success() {
        let stderr = String::from_utf8_lossy(&compile_out.stderr);
        let excerpt: String = stderr.chars().take(300).collect();
        return Err(Diagnostic::error(
            "VG-L0001",
            Span::default(),
            format!(
                "{} failed for {}: {}",
                compiler_path.display(),
                path.display(),
                excerpt.trim()
            ),
        ));
    }

    let opt_path = find_opt();
    let opt_args = vec![
        "-passes=mem2reg,loop-simplify,lcssa,indvars".to_string(),
        "-S".to_string(),
        raw_ll.display().to_string(),
        "-o".to_string(),
        norm_ll.display().to_string(),
    ];
    let opt_out = Command::new(&opt_path)
        .args(&opt_args)
        .output()
        .map_err(|e| {
            Diagnostic::error(
                "VG-L0001",
                Span::default(),
                format!("failed to invoke {}: {e}", opt_path.display()),
            )
        })?;
    if !opt_out.status.success() {
        let stderr = String::from_utf8_lossy(&opt_out.stderr);
        let excerpt: String = stderr.chars().take(300).collect();
        return Err(Diagnostic::error(
            "VG-L0001",
            Span::default(),
            format!(
                "{} normalization failed: {}",
                opt_path.display(),
                excerpt.trim()
            ),
        ));
    }

    let llvm_ir = std::fs::read_to_string(&norm_ll).map_err(|e| {
        Diagnostic::error(
            "VG-L0001",
            Span::default(),
            format!("failed to read normalized IR {}: {e}", norm_ll.display()),
        )
    })?;

    // Replace temp paths in provenance args with stable placeholders for deterministic provenance.
    let n_args = compiler_args.len();
    if n_args >= 2 && compiler_args[n_args - 2] == "-o" {
        compiler_args[n_args - 1] = "<raw.ll>".to_string();
    }
    let stable_opt_args = vec![
        "-passes=mem2reg,loop-simplify,lcssa,indvars".to_string(),
        "-S".to_string(),
        "<raw.ll>".to_string(),
        "-o".to_string(),
        "<norm.ll>".to_string(),
    ];

    Ok(IngestedIr {
        llvm_ir,
        provenance: Provenance {
            path: path.display().to_string(),
            function: function.to_string(),
            language,
            compiler: compiler_path.display().to_string(),
            compiler_args,
            opt: opt_path.display().to_string(),
            opt_args: stable_opt_args,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_c_and_rust_snippets_produce_normalized_llvm_ir() {
        if std::env::var("VG_REQUIRE_ALIVE2").as_deref() != Ok("1")
            && (!crate::verify::has_alive_tv() || !has_opt())
        {
            eprintln!("SKIP: alive-tv or opt not installed on this host");
            return;
        }
        let c_file = fresh_temp_path("ingest_sample", "c");
        let rs_file = fresh_temp_path("ingest_sample", "rs");
        let _guard = TempGuard(vec![c_file.clone(), rs_file.clone()]);

        std::fs::write(
            &c_file,
            "unsigned int add_u(unsigned int a, unsigned int b) { return a + b; }\n",
        )
        .unwrap();
        std::fs::write(
            &rs_file,
            "#[unsafe(no_mangle)]\npub extern \"C\" fn add_rs(a: u32, b: u32) -> u32 { a.wrapping_add(b) }\n",
        )
        .unwrap();

        let c_ir = ingest_to_llvm_ir(&c_file, "add_u", Language::C, &[]).unwrap();
        assert!(c_ir.llvm_ir.contains("@add_u("));
        assert!(
            c_ir.provenance
                .compiler_args
                .contains(&"-fno-unroll-loops".to_string())
        );
        assert!(
            c_ir.provenance
                .opt_args
                .contains(&"-passes=mem2reg,loop-simplify,lcssa,indvars".to_string())
        );

        let rs_ir = ingest_to_llvm_ir(&rs_file, "add_rs", Language::Rust, &[]).unwrap();
        assert!(rs_ir.llvm_ir.contains("@add_rs("));
        assert!(
            rs_ir
                .provenance
                .compiler_args
                .contains(&"no-vectorize-loops".to_string())
        );
    }

    #[test]
    fn ingest_missing_file_returns_vg_l0001() {
        let err = ingest_to_llvm_ir(
            Path::new("/nonexistent/path/foo.c"),
            "foo",
            Language::C,
            &[],
        )
        .unwrap_err();
        assert_eq!(err.code, "VG-L0001");
    }
}
