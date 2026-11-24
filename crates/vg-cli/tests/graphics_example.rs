//! examples/mandelbrot.vgs, the graphics example of docs/guide/examples.md: every obligation is
//! proven, and the built executables draw the same pixels as vg-interp. The whole image takes a
//! minute in a debug vg-interp, so the interpreter checks a fixed sample: every palette entry and
//! 24 pixels spread over the image, inside the set and out.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value as J;

const W: u64 = 256;
const PIXELS: u64 = 256 * 192;
// The default view of the guide: the whole set, 3.5 units across 256 pixels.
const VIEW: (f32, f32, f32) = (-0.75, 0.0, 0.013_671_875);

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/mandelbrot.vgs")
}

/// A fresh working directory for this test process: vg keeps its proof cache in `./.vg`, which must
/// neither land in the source tree nor serve records from an earlier run.
fn scratch_cwd() -> &'static std::path::Path {
    static DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let d =
            std::env::temp_dir().join(format!("vg-test-graphics_example-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    })
}

fn vg(args: &[&str]) -> J {
    let out = Command::new(env!("CARGO_BIN_EXE_vg"))
        .current_dir(scratch_cwd())
        .args(args)
        .output()
        .expect("vg runs");
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "vg {args:?} printed no JSON ({e}): {}",
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn run_u32(node: &str, inputs: J) -> u32 {
    let out = vg(&[
        "run",
        example().to_str().unwrap(),
        node,
        &inputs.to_string(),
    ]);
    assert_eq!(out["ok"], true, "{node} {inputs}: {out}");
    out["outputs"][0].as_u64().expect("a u32") as u32
}

/// Pixel indices to check: the four corners, the centre (inside the set, so black), and 19 more
/// spread over the image by a stride coprime to its size.
fn sample() -> Vec<u64> {
    let mut v = vec![0, W - 1, PIXELS - W, PIXELS - 1, 96 * W + 128];
    v.extend((1..20).map(|k| (k * 2_459) % PIXELS));
    v
}

/// vg-interp's colour for each sampled pixel, computed node by node.
fn interp_sample() -> Vec<(u64, u32)> {
    let palette: Vec<u32> = (0..16u64)
        .map(|j| run_u32("@palette_entry", serde_json::json!({ "j": j })))
        .collect();
    let (cx, cy, scale) = VIEW;
    sample()
        .into_iter()
        .map(|i| {
            let inputs = serde_json::json!({
                "i": i, "cx": cx.to_string(), "cy": cy.to_string(),
                "scale": scale.to_string(), "palette": palette,
            });
            (i, run_u32("@pixel", inputs))
        })
        .collect()
}

/// Builds `@render` for `target` into a fresh directory; the path when it links on this host.
fn build(target: &str) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("vg-mandelbrot-{target}-{}", std::process::id()));
    let out = vg(&[
        "build",
        example().to_str().unwrap(),
        "@render",
        "--target",
        target,
        "-o",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(out["ok"], true, "{out}");
    let a = &out["artifacts"][0];
    assert_eq!(a["residual_guards"], 0, "every check is proven: {a}");
    (a["linked"] == true).then(|| PathBuf::from(a["path"].as_str().expect("a path")))
}

/// The view as the executable reads it: three f32, little-endian.
fn view_bytes() -> Vec<u8> {
    let (cx, cy, scale) = VIEW;
    [cx, cy, scale]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

fn run_image(cmd: &mut Command) -> Vec<u8> {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("the executable starts");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&view_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "exit {:?}", out.status);
    assert_eq!(
        out.stdout.len() as u64,
        PIXELS * 4,
        "one RGBA word per pixel"
    );
    out.stdout
}

fn check_image(target: &str, image: &[u8], want: &[(u64, u32)]) {
    for &(i, colour) in want {
        let at = (i * 4) as usize;
        let got = u32::from_le_bytes(image[at..at + 4].try_into().unwrap());
        assert_eq!(got, colour, "{target}: pixel {i}");
    }
    // POST: every pixel is opaque.
    assert!(
        image.chunks(4).all(|p| p[3] == 255),
        "{target}: a pixel is not opaque"
    );
}

#[test]
fn every_obligation_is_proven() {
    let out = vg(&["verify", example().to_str().unwrap()]);
    let results = out["results"].as_array().expect("results");
    let open: Vec<_> = results
        .iter()
        .filter(|r| r["verdict"] != "proven")
        .map(|r| format!("{} {}", r["id"], r["verdict"]))
        .collect();
    assert!(open.is_empty(), "not proven: {open:?}");
    // The claims the guide makes, among the proven obligations. IDs name ops by canonical index,
    // so match the node and the kind: one CAST.in_range per colour channel, and so on.
    for (node, kind, count) in [
        ("@pixel/", "/FOLD.step", 1),
        ("@pixel/", "/INDEX.in_bounds", 1),
        ("@palette_entry/", "/CAST.in_range", 3),
        ("@render/", "POST.0", 1),
    ] {
        let n = results
            .iter()
            .filter_map(|r| r["id"].as_str())
            .filter(|id| id.starts_with(node) && id.ends_with(kind))
            .count();
        assert_eq!(n, count, "{node}…{kind}");
    }
}

#[test]
fn executables_draw_the_pixels_vg_interp_draws() {
    let want = interp_sample();
    // The view's centre, -0.75 + 0i, is inside the set: black.
    assert!(want.contains(&(96 * W + 128, 0xFF00_0000)));
    let mut ran = 0;
    if cfg!(all(target_os = "macos", target_arch = "aarch64"))
        && let Some(exe) = build("aarch64-apple-darwin")
    {
        check_image(
            "aarch64-apple-darwin",
            &run_image(&mut Command::new(exe)),
            &want,
        );
        ran += 1;
    }
    let wasmtime = Command::new("wasmtime").arg("--version").output().is_ok();
    if let Some(wasm) = build("wasm32-wasip1").filter(|_| wasmtime) {
        let image = run_image(Command::new("wasmtime").arg("run").arg(wasm));
        check_image("wasm32-wasip1", &image, &want);
        ran += 1;
    }
    eprintln!("{ran} executables checked against vg-interp");
}
