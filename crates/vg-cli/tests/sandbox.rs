//! P4.6 and G4 (docs/04, "Sandbox manifests"): a built program keeps exactly the files its manifest
//! names. vg-rt opens the root's handles from the command line, then drops every other authority:
//! one test per target shows a granted file working and the OS refusing an ungranted one. The
//! Linux test runs only on a Linux host, and fails, never skips, when the kernel lacks Landlock.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value as J;

/// A root whose one handle is a FileOut: it appends the bytes it reads from stdin.
const WRITER: &str = "module writer;
NODE @main [intent: \"Append stdin's bytes to the file its handle names\", export] {
  SIGNATURE: (e: Eff, dst: FileOut, data: Slice<u8>) -> (Eff, Result<u64, u32>);
  EFFECTS: [FILE_WRITE];
  CONTRACT { }
  BODY {
    %0, %1 = FILE_WRITE(e, dst, data);
    RETURN(%0, %1);
  }
}
";

/// A program linked with vg-rt that does what a root's entry does (open its handle from the
/// command line, then apply the sandbox) and then tries what no root can express: opening a file
/// its manifest does not name, and a socket. argv[1] is granted; argv[2] is not.
const PROBE: &str = r#"#include "vg_rt.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/socket.h>
int main(int argc, char **argv) {
    if (argc != 3) return 2;
    vg_ctx ctx;
    vg_rt_set_args(2, argv);
    vg_rt_init(&ctx, 4096, "probe");
    uint32_t h = vg_rt_resolve_handle(&ctx, 0, VG_HANDLE_FILE_IN);
    vg_rt_sandbox();
    vg_res_slice_u8_u32 r;
    vg_rt_file_read(&ctx, h, 64, &r);
    int fd = open(argv[2], O_RDONLY);
    int open_errno = errno;
    int s = socket(AF_INET, SOCK_STREAM, 0);
    int socket_errno = errno;
    printf("granted=%s ungranted=%s/%d socket=%s/%d\n", r.is_ok ? "read" : "failed",
           fd < 0 ? "refused" : "opened", fd < 0 ? open_errno : 0,
           s < 0 ? "refused" : "opened", s < 0 ? socket_errno : 0);
    return 0;
}
"#;

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vg-sandbox-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Builds the probe with the host's C compiler against runtime/vg_rt.c and runs it.
#[allow(dead_code)]
fn probe(dir: &Path) -> String {
    std::fs::write(dir.join("probe.c"), PROBE).unwrap();
    let cc = if cfg!(target_os = "macos") {
        "/usr/bin/clang"
    } else {
        "cc"
    };
    let rt = repo().join("runtime");
    let st = Command::new(cc)
        .args(["-std=c11", "-O2", "-I"])
        .arg(&rt)
        .arg(dir.join("probe.c"))
        .arg(rt.join("vg_rt.c"))
        .arg("-o")
        .arg(dir.join("probe"))
        .status()
        .expect("the C compiler runs");
    assert!(st.success(), "the probe builds");
    std::fs::write(dir.join("granted.txt"), "granted\n").unwrap();
    std::fs::write(dir.join("other.txt"), "not granted\n").unwrap();
    let out = Command::new(dir.join("probe"))
        .arg(dir.join("granted.txt"))
        .arg(dir.join("other.txt"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "the probe ran sandboxed: {:?} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn vg(args: &[&str]) -> J {
    let out = Command::new(env!("CARGO_BIN_EXE_vg"))
        .args(args)
        .output()
        .expect("vg runs");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Builds the writer root for `target`; the executable when it links on this host.
#[allow(dead_code)]
fn build_writer(dir: &Path, target: &str) -> Option<PathBuf> {
    let src = dir.join("writer.vgs");
    std::fs::write(&src, WRITER).unwrap();
    let out = vg(&[
        "build",
        src.to_str().unwrap(),
        "@main",
        "--root",
        dir.to_str().unwrap(),
        "--target",
        target,
        "-o",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(out["ok"], true, "{out}");
    let manifest = std::fs::read_to_string(out["manifest"].as_str().expect("a manifest")).unwrap();
    assert_eq!(
        manifest,
        "format = 1\nroot = \"@main\"\n\n[[handle]]\nparam = \"dst\"\nkind = \"FileOut\"\npath = { argv = 1 }\n"
    );
    let a = &out["artifacts"][0];
    (a["linked"] == true).then(|| PathBuf::from(a["path"].as_str().unwrap()))
}

/// The writer's stdin: a slice of bytes, its u64 length first (docs/13).
#[allow(dead_code)]
fn run_writer(cmd: &mut Command, data: &[u8]) -> Output {
    use std::io::Write;
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = (data.len() as u64).to_le_bytes().to_vec();
    input.extend_from_slice(data);
    child.stdin.take().unwrap().write_all(&input).unwrap();
    child.wait_with_output().unwrap()
}

#[cfg(target_os = "macos")]
#[test]
fn macos_sandbox_refuses_an_ungranted_file() {
    let dir = scratch("macos");
    if let Some(exe) = build_writer(&dir, "aarch64-apple-darwin") {
        let granted = dir.join("granted-out.txt");
        let out = run_writer(Command::new(&exe).arg(&granted), b"through the handle\n");
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            std::fs::read_to_string(&granted).unwrap(),
            "through the handle\n"
        );
        // A path that cannot be opened stops the program before the root runs.
        let out = run_writer(Command::new(&exe).arg(dir.join("no/such/dir/x")), b"x");
        assert_eq!(out.status.code(), Some(5), "{out:?}");
    }
    let seen = probe(&dir);
    assert!(
        seen.starts_with("granted=read ungranted=refused/1 "),
        "{seen}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_sandbox_refuses_an_ungranted_file() {
    let dir = scratch("linux");
    // Landlock refuses the open with EACCES; seccomp refuses the socket with EPERM.
    let seen = probe(&dir);
    assert_eq!(seen, "granted=read ungranted=refused/13 socket=refused/1\n");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn wasm_sandbox_refuses_an_ungranted_file() {
    let wasmtime = Command::new("wasmtime").arg("--version").output().is_ok();
    let dir = scratch("wasm");
    let exe = if wasmtime {
        build_writer(&dir, "wasm32-wasip1")
    } else {
        None
    };
    let Some(exe) = exe else {
        assert!(
            std::env::var("VG_REQUIRE_WASMTIME").as_deref() != Ok("1"),
            "wasmtime or the WASI toolchain is missing and VG_REQUIRE_WASMTIME=1"
        );
        eprintln!("skipped: wasmtime or the WASI toolchain is missing");
        return;
    };
    std::fs::create_dir_all(dir.join("granted")).unwrap();
    std::fs::create_dir_all(dir.join("other")).unwrap();
    let wasm = |path: &str| {
        let mut c = Command::new("wasmtime");
        c.current_dir(&dir)
            .args(["run", "--dir", "granted"])
            .arg(&exe)
            .arg(path);
        c
    };
    let out = run_writer(&mut wasm("granted/out.txt"), b"preopened\n");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(dir.join("granted/out.txt")).unwrap(),
        "preopened\n"
    );
    // WASI holds only the preopened directory: a path outside it cannot be opened.
    let out = run_writer(&mut wasm("other/out.txt"), b"x");
    assert_eq!(out.status.code(), Some(5), "{out:?}");
    assert!(!dir.join("other/out.txt").exists());
    let _ = std::fs::remove_dir_all(&dir);
}
