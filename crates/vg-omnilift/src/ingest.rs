//! P8.1: Ingest C, C++, Rust, Zig, Swift and GLSL sources to normalized textual LLVM IR via
//! each language's LLVM front end + opt (docs/08).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use vg_core::diag::{Diagnostic, Span};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    C,
    Cpp,
    Rust,
    Zig,
    Swift,
    Glsl,
    /// R-13: a typed, pure, int-only Python subset, translated to C by pylift.py.
    Python,
}

impl Language {
    pub const ALL: &[Language] = &[
        Language::C,
        Language::Cpp,
        Language::Rust,
        Language::Zig,
        Language::Swift,
        Language::Glsl,
        Language::Python,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Rust => "rust",
            Language::Zig => "zig",
            Language::Swift => "swift",
            Language::Glsl => "glsl",
            Language::Python => "python",
        }
    }

    /// The `--lang` spelling: the canonical name or a common alias, any case.
    pub fn from_name(name: &str) -> Option<Language> {
        let n = name.to_ascii_lowercase();
        let n = match n.as_str() {
            "c++" | "cxx" => "cpp",
            "rs" => "rust",
            "py" => "python",
            other => other,
        };
        Language::ALL.iter().copied().find(|l| l.as_str() == n)
    }

    pub fn from_extension(ext: &str) -> Option<Language> {
        Some(match ext {
            "c" | "h" => Language::C,
            "cpp" | "cc" | "cxx" | "hpp" | "hh" => Language::Cpp,
            "rs" => Language::Rust,
            "zig" => Language::Zig,
            "swift" => Language::Swift,
            "glsl" | "comp" | "vert" | "frag" => Language::Glsl,
            "py" => Language::Python,
            _ => return None,
        })
    }
}

fn compiler_for(language: Language) -> PathBuf {
    match language {
        Language::C | Language::Cpp | Language::Glsl => find_clang(),
        Language::Python => find_clang(),
        Language::Rust => find_rustc(),
        Language::Zig => PathBuf::from("zig"),
        Language::Swift => PathBuf::from("swiftc"),
    }
}

/// Whether the tools this language's ingest runs are installed (GLSL also needs glslc,
/// Python a python3 of 3.11 or newer).
pub fn has_compiler(language: Language) -> bool {
    if language == Language::Python
        && !Command::new("python3")
            .args([
                "-I",
                "-c",
                "import sys; sys.exit(sys.version_info < (3, 11))",
            ])
            .output()
            .is_ok_and(|o| o.status.success())
    {
        return false;
    }
    let runs = |tool: &Path, arg: &str| {
        Command::new(tool)
            .arg(arg)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let flag = if language == Language::Zig {
        "version"
    } else {
        "--version"
    };
    runs(&compiler_for(language), flag)
        && (language != Language::Glsl || runs(Path::new("glslc"), "--version"))
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
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(p);
            } else {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}

fn ingest_err(msg: String) -> Diagnostic {
    Diagnostic::error("VG-L0001", Span::default(), msg)
}

/// Prepended to GLSL compiled as C++: GLSL's scalar integer names and the builtins OmniLift
/// supports, each with GLSL's meaning. Anything else stays undeclared and fails to compile.
const GLSL_PRELUDE: &str = "\
typedef unsigned int uint;
#define in
#define highp
#define mediump
#define lowp
static inline int min(int a, int b) { return b < a ? b : a; }
static inline uint min(uint a, uint b) { return b < a ? b : a; }
static inline int max(int a, int b) { return a < b ? b : a; }
static inline uint max(uint a, uint b) { return a < b ? b : a; }
static inline int clamp(int x, int lo, int hi) { return min(max(x, lo), hi); }
static inline uint clamp(uint x, uint lo, uint hi) { return min(max(x, lo), hi); }
static inline int abs(int x) { return x < 0 ? -x : x; }
";

/// Rejects GLSL integer literals whose C++ type differs: an unsuffixed hex or octal literal
/// above 0x7FFFFFFF is an `int` bit pattern in GLSL but an `unsigned int` in C++.
fn check_glsl_literals(src: &str) -> Result<(), Diagnostic> {
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if src[i..].starts_with("//") {
            i += src[i..].find('\n').unwrap_or(src.len() - i);
        } else if src[i..].starts_with("/*") {
            i += src[i..].find("*/").map_or(src.len() - i, |e| e + 2);
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
        } else if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.') {
                i += 1;
            }
            let lit = &src[start..i];
            let (digits, radix) = if let Some(h) = lit.strip_prefix("0x").or(lit.strip_prefix("0X"))
            {
                (h, 16)
            } else if lit.len() > 1 && lit.starts_with('0') {
                (&lit[1..], 8)
            } else {
                continue;
            };
            if let Ok(v) = u64::from_str_radix(digits, radix)
                && v > 0x7FFF_FFFF
            {
                return Err(ingest_err(format!(
                    "GLSL literal {lit} is a negative int in GLSL but unsigned in C++; write {lit}u \
                     or a negative decimal"
                )));
            }
        } else {
            i += 1;
        }
    }
    Ok(())
}

/// Validates GLSL with glslc (the reference front end), then writes it as C++ for clang.
fn prepare_glsl(path: &Path, temps: &mut Vec<PathBuf>) -> Result<PathBuf, Diagnostic> {
    let src = std::fs::read_to_string(path)
        .map_err(|e| ingest_err(format!("failed to read {}: {e}", path.display())))?;
    check_glsl_literals(&src)?;
    let has_version = src.trim_start().starts_with("#version");
    let mut comp = if has_version {
        String::new()
    } else {
        "#version 450\n".to_string()
    };
    comp += &src;
    comp += "\nvoid main() {}\n";
    let comp_path = fresh_temp_path("glsl", "comp");
    let spv_path = fresh_temp_path("glsl", "spv");
    temps.extend([comp_path.clone(), spv_path.clone()]);
    std::fs::write(&comp_path, comp).map_err(|e| ingest_err(format!("write failed: {e}")))?;
    let out = Command::new("glslc")
        .args(["-fshader-stage=comp", "-o"])
        .arg(&spv_path)
        .arg(&comp_path)
        .output()
        .map_err(|e| ingest_err(format!("failed to invoke glslc: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(ingest_err(format!(
            "glslc rejected {}: {}",
            path.display(),
            stderr.chars().take(300).collect::<String>().trim()
        )));
    }
    // C++ gets the same lines: #version and precision statements become blank lines.
    let body: String = src
        .lines()
        .map(|l| {
            let t = l.trim_start();
            if t.starts_with("#version") || t.starts_with("precision ") {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let cpp = format!(
        "{GLSL_PRELUDE}#line 1 \"{}\"\nextern \"C\" {{\n{body}\n}}\n",
        path.display()
    );
    let cpp_path = fresh_temp_path("glsl", "cpp");
    temps.push(cpp_path.clone());
    std::fs::write(&cpp_path, cpp).map_err(|e| ingest_err(format!("write failed: {e}")))?;
    Ok(cpp_path)
}

/// R-13: translate the Python function to C with pylift.py (embedded), run isolated.
fn prepare_python(
    path: &Path,
    function: &str,
    temps: &mut Vec<PathBuf>,
) -> Result<PathBuf, Diagnostic> {
    let script = fresh_temp_path("pylift", "py");
    let c_path = fresh_temp_path("pylift", "c");
    temps.extend([script.clone(), c_path.clone()]);
    std::fs::write(&script, include_str!("pylift.py"))
        .map_err(|e| ingest_err(format!("write failed: {e}")))?;
    let out = Command::new("python3")
        .arg("-I")
        .arg(&script)
        .arg(path)
        .arg(function)
        .arg(&c_path)
        .output()
        .map_err(|e| ingest_err(format!("failed to invoke python3: {e}")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(ingest_err(format!(
            "{} is outside the OmniLift Python subset: {}",
            path.display(),
            stderr.chars().take(300).collect::<String>().trim()
        )));
    }
    Ok(c_path)
}

/// GLSL has no pointers: a `ptr` parameter means an array or struct passed by value in GLSL
/// became a reference in C++, which a callee could write through.
fn check_no_pointer_params(ir: &str) -> Result<(), Diagnostic> {
    for line in ir.lines().filter(|l| l.starts_with("define ")) {
        let params = line.split_once('(').map_or("", |(_, p)| p);
        if params.contains("ptr") {
            return Err(ingest_err(format!(
                "GLSL function has a pointer parameter after C++ lowering (array or struct \
                 argument), which OmniLift does not lift: {}",
                line.trim_end_matches('{').trim()
            )));
        }
    }
    Ok(())
}

/// Zig emits `export fn f` as `@f = alias ..., ptr @<module>.f` over a private definition.
/// Fold each alias back so `@f` is an ordinary external definition, as clang emits it.
fn fold_zig_export_aliases(ir: &str) -> String {
    let renames: Vec<(String, String)> = ir
        .lines()
        .filter(|l| l.contains(" = alias "))
        .filter_map(|l| {
            let name = l.split_once(" = alias ")?.0.trim();
            let target = l.rsplit_once("ptr @")?.1.trim();
            Some((format!("@{target}"), name.to_string()))
        })
        .collect();
    let mut out = String::with_capacity(ir.len());
    for line in ir.lines().filter(|l| !l.contains(" = alias ")) {
        let mut line = if line.starts_with("define ") {
            line.replacen("define private ", "define ", 1).replacen(
                "define internal ",
                "define ",
                1,
            )
        } else {
            line.to_string()
        };
        for (from, to) in &renames {
            line = rename_symbol(&line, from, to);
        }
        out += &line;
        out.push('\n');
    }
    out
}

/// Removes optimization-hint attributes from `attributes #N = { ... }` groups.
fn strip_attrs(ir: &str, attrs: &[&str]) -> String {
    ir.lines()
        .map(|l| {
            let mut l = l.to_string();
            if l.starts_with("attributes #") {
                for a in attrs {
                    l = l.replace(a, "");
                }
            }
            l + "\n"
        })
        .collect()
}

/// Replaces whole-symbol occurrences of `from` (not prefixes of longer names).
fn rename_symbol(line: &str, from: &str, to: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(pos) = rest.find(from) {
        let after = &rest[pos + from.len()..];
        let continues = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '$'));
        out += &rest[..pos];
        out += if continues { from } else { to };
        rest = after;
    }
    out + rest
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
    let mut temps = vec![raw_ll.clone(), norm_ll.clone()];
    let translated = match language {
        Language::Glsl => Some(prepare_glsl(path, &mut temps)?),
        Language::Python => Some(prepare_python(path, function, &mut temps)?),
        _ => None,
    };
    let zig_cache = fresh_temp_path("zig_cache", "d");
    temps.push(zig_cache.clone());
    let _cleanup = TempGuard(temps);

    let compiler_path = compiler_for(language);
    let mut compiler_args = match language {
        Language::Python => {
            // -O0 without optnone: every Python operation stays one C operation with its nsw
            // and UB intact; -O1 may reassociate and drop nsw, letting an overflow wrap.
            let mut args = [
                "-O0",
                "-Xclang",
                "-disable-O0-optnone",
                "-Wno-unused-function",
                "-std=c11",
                "-S",
                "-emit-llvm",
                "-target",
                "aarch64-apple-darwin",
            ]
            .map(String::from)
            .to_vec();
            args.extend(extra_args.iter().cloned());
            args.push(translated.as_deref().unwrap_or(path).display().to_string());
            args.push("-o".to_string());
            args.push(raw_ll.display().to_string());
            args
        }
        Language::C | Language::Cpp | Language::Glsl => {
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
            if language != Language::C {
                args.extend(["-x", "c++", "-std=c++20"].map(String::from));
            }
            if language == Language::Glsl {
                // GLSL int arithmetic wraps; unsequenced side effects have no GLSL meaning.
                args.extend(["-fwrapv", "-Werror=unsequenced"].map(String::from));
            }
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
            args.push(translated.as_deref().unwrap_or(path).display().to_string());
            args.push("-o".to_string());
            args.push(raw_ll.display().to_string());
            args
        }
        Language::Zig => {
            // Zig has no -O1 and no unroll or vectorize switches; ReleaseSmall (-Oz) does
            // neither, and its safety checks are off, so no panic calls reach the lifter.
            let mut args = vec![
                "build-obj".to_string(),
                "-O".to_string(),
                "ReleaseSmall".to_string(),
                "-target".to_string(),
                "aarch64-macos".to_string(),
                "-fstrip".to_string(),
                "-fno-emit-bin".to_string(),
                "--cache-dir".to_string(),
                zig_cache.display().to_string(),
            ];
            args.extend(extra_args.iter().cloned());
            args.push(path.display().to_string());
            args.push(format!("-femit-llvm-ir={}", raw_ll.display()));
            args
        }
        Language::Swift => {
            let mut args = vec![
                "-O".to_string(),
                "-emit-ir".to_string(),
                "-parse-as-library".to_string(),
                // Fixed, so Swift's mangled names never carry the temp output path.
                "-module-name".to_string(),
                "lifted".to_string(),
                "-target".to_string(),
                "arm64-apple-macosx14.0".to_string(),
            ];
            for flag in [
                "-vectorize-loops=false",
                "-vectorize-slp=false",
                "-unroll-threshold=0",
            ] {
                args.extend(["-Xllvm".to_string(), flag.to_string()]);
            }
            args.extend(extra_args.iter().cloned());
            args.push(path.display().to_string());
            args.push("-o".to_string());
            args.push(raw_ll.display().to_string());
            args
        }
        Language::Rust => {
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
            args
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

    if matches!(language, Language::Zig | Language::Swift | Language::Glsl) {
        let raw = std::fs::read_to_string(&raw_ll)
            .map_err(|e| ingest_err(format!("failed to read {}: {e}", raw_ll.display())))?;
        let raw = match language {
            Language::Glsl => {
                check_no_pointer_params(&raw)?;
                raw
            }
            // minsize/optsize stop loop-rotate from putting the exit test in the latch.
            Language::Zig => strip_attrs(&fold_zig_export_aliases(&raw), &[" minsize", " optsize"]),
            // Linker keep-alive lists name Swift-ABI wrappers that verification drops, and
            // Swift's stack-probe attribute is one the harness's clang backend cannot lower.
            _ => strip_attrs(
                &raw.lines()
                    .filter(|l| {
                        !l.starts_with("@llvm.used ") && !l.starts_with("@llvm.compiler.used ")
                    })
                    .map(|l| format!("{l}\n"))
                    .collect::<String>(),
                &[" \"probe-stack\"=\"__chkstk_darwin\""],
            ),
        };
        std::fs::write(&raw_ll, raw).map_err(|e| ingest_err(format!("write failed: {e}")))?;
    }

    let opt_path = find_opt();
    // The C and Rust pipeline is the G8 one; front ends whose loops come out unrotated
    // (Zig at -Oz) also get loop-rotate, so every loop exits from its latch.
    let passes = match language {
        Language::Zig => "-passes=mem2reg,loop-simplify,loop-rotate,lcssa,indvars",
        // -O0 code keeps empty join blocks between an if and a loop latch; simplifycfg with
        // speculation, hoisting and sinking off only merges such blocks, so no nsw is lost.
        Language::Python => {
            "-passes=mem2reg,simplifycfg<keep-loops;no-hoist-common-insts;no-sink-common-insts;\
             no-speculate-blocks>,loop-simplify,loop-rotate,lcssa,indvars"
        }
        _ => "-passes=mem2reg,loop-simplify,lcssa,indvars",
    };
    let opt_args = vec![
        passes.to_string(),
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
    let temp_names = [
        (raw_ll.display().to_string(), "<raw.ll>"),
        (zig_cache.display().to_string(), "<zig-cache>"),
    ];
    for arg in &mut compiler_args {
        for (temp, stable) in &temp_names {
            *arg = arg.replace(temp, stable);
        }
        if let Some(cpp) = &translated {
            let stable = match language {
                Language::Python => "<pylift.c>",
                _ => "<glsl.cpp>",
            };
            *arg = arg.replace(&cpp.display().to_string(), stable);
        }
    }
    let stable_opt_args = vec![
        passes.to_string(),
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
    fn language_names_and_extensions_round_trip() {
        for &l in Language::ALL {
            assert_eq!(Language::from_name(l.as_str()), Some(l));
        }
        assert_eq!(Language::from_name("C++"), Some(Language::Cpp));
        assert_eq!(Language::from_name("RS"), Some(Language::Rust));
        assert_eq!(Language::from_name("cobol"), None);
        assert_eq!(Language::from_extension("frag"), Some(Language::Glsl));
        assert_eq!(Language::from_extension("hpp"), Some(Language::Cpp));
        assert_eq!(Language::from_extension("txt"), None);
    }

    #[test]
    fn glsl_literal_check_rejects_only_unsuffixed_hex_and_octal_above_int_max() {
        for ok in [
            "x = 0x7FFFFFFF;",
            "x = 0xFFFFFFFFu;",
            "x = 0x80000000U;",
            "x = 4294967295u;",
            "f = 0.5e3;",
            "x = 017;",
            "// 0xFFFFFFFF in a comment",
            "/* 0x80000000 */ y",
            "id0xFFFFFFFF = 1;",
        ] {
            assert!(check_glsl_literals(ok).is_ok(), "{ok}");
        }
        for bad in ["x = 0xFFFFFFFF;", "x = 0X80000000;", "x = 037777777777;"] {
            assert_eq!(
                check_glsl_literals(bad).unwrap_err().code,
                "VG-L0001",
                "{bad}"
            );
        }
    }

    #[test]
    fn glsl_pointer_params_are_rejected() {
        assert!(check_no_pointer_params("define i32 @f(i32 %0) {\n").is_ok());
        assert!(check_no_pointer_params("define i32 @g(ptr noundef %0) {\n").is_err());
    }

    #[test]
    fn zig_export_aliases_fold_into_external_definitions() {
        let ir = "@f = alias i32 (i32), ptr @m.f\n\
                  define private i32 @m.f(i32 %0) #0 {\n\
                  %2 = call i32 @m.f.helper(i32 %0)\n\
                  ret i32 %2\n}\n\
                  attributes #0 = { minsize nounwind optsize }\n";
        let out = strip_attrs(&fold_zig_export_aliases(ir), &[" minsize", " optsize"]);
        assert!(!out.contains("alias"));
        assert!(out.contains("define i32 @f(i32 %0) #0 {"));
        // A longer symbol that merely starts with the target name keeps its name.
        assert!(out.contains("@m.f.helper("));
        assert!(out.contains("attributes #0 = { nounwind }"));
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
