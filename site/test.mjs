// Usage: node site/test.mjs, after site/build.sh. A differential test of the site's binaries: each
// one runs under the page's own host (js/vg.js) and must give, bit for bit, what vg-interp gives
// for the same node and inputs (`vg run`). A checked entry must also refuse inputs that break PRE.
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { run, encode, f32s, hex } from "./js/vg.js";

const root = new URL("..", import.meta.url).pathname;
const VG = `${root}target/release/vg`;
let failures = 0;

const wasm = async (name, input) => run(await WebAssembly.compile(readFileSync(`${root}site/wasm/${name}.wasm`)), input);
const interp = (file, node, args) => {
  const r = JSON.parse(execFileSync(VG, ["run", file, node, JSON.stringify(args)], { encoding: "utf8", maxBuffer: 1 << 26 }));
  if (!r.ok) throw new Error(`${node}: ${r.error}`);
  return r.outputs;
};
// Flattens interpreter outputs (strings, arrays, tuples) into the scalars in docs/13 byte order.
const flat = (v) => (Array.isArray(v) ? v.flatMap(flat) : [v]);
const str = (xs) => xs.map(String);

// Compares bytes from the binary with the interpreter's scalars, given each scalar's type.
function check(name, bytes, scalars, types) {
  const view = new DataView(bytes.buffer);
  let at = 0, bad = 0;
  scalars.forEach((s, i) => {
    const type = types(i);
    const got = type === "f32" ? view.getFloat32(at, true) : view.getUint32(at, true);
    const want = type === "f32" ? Math.fround(Number(s)) : Number(s);
    at += 4;
    if (!Object.is(got, want) && bad++ < 3) console.error(`  ${name}[${i}]: binary ${got}, vg-interp ${want}`);
  });
  if (at !== bytes.length) bad++, console.error(`  ${name}: ${bytes.length} bytes, expected ${at}`);
  console.log(`${bad ? "FAIL" : "ok  "} ${name}: ${scalars.length} values`);
  failures += bad ? 1 : 0;
}

const LIB = "site/vg/weblib.vgs";

check("@motion", await wasm("motion"), flat(interp(LIB, "@motion", {})), () => "f32");

const ratios = [0.792, 1.593, 0.996, 1.0, 4.391, 4.656, 1.381, 1.403, 2.636, 2.93, 1.2, 1.0];
check("@bar_chart", await wasm("bar_chart", encode([["f32", ratios]])),
  flat(interp(LIB, "@bar_chart", { vs: str(ratios.map(Math.fround)) })), (i) => (i < 24 ? "f32" : "u32"));

const counts = [120, 64, 40, 31, 22, 15, 9, 4];
for (const sweep of [1, 0.375]) {
  check(`@donut sweep ${sweep}`, await wasm("donut", encode([["f32", counts], ["f32", sweep]])),
    flat(interp(LIB, "@donut", { vs: str(counts), sweep: String(sweep) })), () => "f32");
}

for (const [px, py, zin] of [[200, 10, true], [0, 191, false], [128, 96, true]]) {
  const view = [-0.5, 0, 0.0125];
  check(`@zoom (${px}, ${py}, ${zin})`, await wasm("zoom", encode([["f32", view], ["u32", [px, py]], ["bool", zin]])),
    flat(interp(LIB, "@zoom", { cx: "-0.5", cy: "0.0", scale: "0.0125", px, py, zin })), () => "f32");
}

check("@aurora t = 0.25", await wasm("aurora", encode([["f32", 0.25]])), flat(interp(LIB, "@aurora", { t: "0.25" })), () => "u32");

// The checked entry validates PRE at the trust boundary: a scale above 0.02 must exit 1.
try {
  await wasm("zoom", encode([["f32", [-0.5, 0, 0.05]], ["u32", [1, 1]], ["bool", true]]));
  console.error("FAIL @zoom accepted a scale outside its PRE");
  failures++;
} catch (e) {
  console.log(`${String(e.message).startsWith("exit 1") ? "ok  " : "FAIL"} @zoom refuses scale 0.05: ${e.message.split("\n")[0]}`);
  if (!String(e.message).startsWith("exit 1")) failures++;
}

const renderBytes = await wasm("render", encode([["f32", [-0.5, 0, 0.0125]]]));
const renderView = new DataView(renderBytes.buffer);
const colors = new Set();
let transparent = 0;
if (renderBytes.length !== 196608) {
  console.error(`FAIL @render: ${renderBytes.length} bytes, expected 196608`);
  failures++;
} else {
  for (let i = 0; i < 49152; i++) {
    const px = renderView.getUint32(i * 4, true);
    if ((px >>> 24) !== 0xff) transparent++;
    colors.add(px);
  }
  const ok = transparent === 0 && colors.size > 1;
  console.log(`${ok ? "ok  " : "FAIL"} @render: 49152 opaque pixels, ${colors.size} distinct colors, ${transparent} transparent`);
  if (!ok) failures++;
}

try {
  await wasm("render", encode([["f32", [-0.5, 0, 0.05]]]));
  console.error("FAIL @render accepted a scale outside its PRE");
  failures++;
} catch (e) {
  console.log(`${String(e.message).startsWith("exit 1") ? "ok  " : "FAIL"} @render refuses scale 0.05: ${e.message.split("\n")[0]}`);
  if (!String(e.message).startsWith("exit 1")) failures++;
}

// ---- The chapters' lab binaries ---------------------------------------------------------------

const DEMOS = "site/vg/demos.vgs";
const MESH_ROOT = `${root}target/site/mesh`;
const same = (name, ok, detail) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}: ${detail}`);
  if (!ok) failures++;
};

const grid = await wasm("grid", encode([["f32", [-3, 3]]]));
check("@grid", grid, flat(interp(DEMOS, "@grid", { lo: "-3.0", hi: "3.0" })), () => "f32");
const xs = f32s(grid, 0, 256);
check("@knee_curve", await wasm("knee", encode([["f32", xs], ["f32", 0.7]])),
  flat(interp(DEMOS, "@knee_curve", { xs: str(xs), knee: "0.7" })), () => "f32");
const huge = [-3.4028235e38, -2, -0.6, 0, 0.6, 2, 3e38, ...Array(249).fill(1)];
check("@draft_curve", await wasm("knee_draft", encode([["f32", huge], ["f32", 0.5]])),
  flat(interp("site/vg/draft.vgs", "@draft_curve", { xs: str(huge.map(Math.fround)), knee: "0.5" })), () => "f32");
const draftOut = f32s(await wasm("knee_draft", encode([["f32", huge], ["f32", 0.5]])), 0, 2);
same("@draft_curve at x = -3.4028235e38", Number.isNaN(draftOut[0]), `draft gives ${draftOut[0]} (NaN expected: the bug)`);
try {
  await wasm("knee", encode([["f32", [NaN, ...Array(255).fill(0)]], ["f32", 0.7]]));
  same("@knee_curve refuses NaN", false, "accepted");
} catch (e) {
  same("@knee_curve refuses NaN", String(e.message).startsWith("exit 1"), e.message.split("\n")[0]);
}

const tone = await wasm("tone", encode([["u32", 3], ["f32", 6]]));
check("@tone", tone, flat(interp(DEMOS, "@tone", { frame: 3, drive: "6.0" })), () => "f32");
const toneVals = f32s(tone, 0, 1024);
const peakBytes = await wasm("peak", encode([["bytes", tone]]));
check("@peak", peakBytes, flat(interp(DEMOS, "@peak", { xs: str(toneVals) })), () => "f32");
const peak = f32s(peakBytes, 0, 1)[0];
same("@peak is the largest |sample|", peak === Math.max(...toneVals.map(Math.abs)) && peak > 1, `peak ${peak.toFixed(3)} (the chord is too loud on purpose)`);

// The limiter reads tone's bytes as they are, then its LEARNABLE knee (an implicit trailing input,
// docs/06); its digest must be the SHA-256 of its own output.
const limited = await wasm("limiter", encode([["bytes", tone], ["f32", 0.7]]));
same("@limit_frame output size", limited.length === 4096 + 32, `${limited.length} bytes`);
const out = f32s(limited, 0, 1024);
same("@limit_frame POST", out.every((v, i) => v >= -1 && v <= 1 && (toneVals[i] >= 0) === (v >= 0)), "every sample in [-1, 1], sign kept");
const digest = hex(limited.subarray(4096));
const sha = hex(await wasm("sha256", encode([["slice", limited.subarray(0, 4096)]])));
same("@limit_frame digest = @sha256_impl(output)", digest === sha, digest.slice(0, 16));
const interpLimited = flat(JSON.parse(execFileSync(VG, ["run", "examples/audio_limiter.vgs", "@limit_frame",
  JSON.stringify({ frame: Array.from({ length: 512 }, (_, i) => [String(toneVals[2 * i]), String(toneVals[2 * i + 1])]), knee: "0.7" }), "--root", MESH_ROOT],
  { encoding: "utf8", maxBuffer: 1 << 26, cwd: root })).outputs);
check("@limit_frame vs vg-interp (samples)", limited.subarray(0, 4096), interpLimited.slice(0, 1024), () => "f32");

// FIPS 180-4's first example, and the standard CRC-32 check value.
same("@sha256_impl(\"abc\")", hex(await wasm("sha256", encode([["slice", new TextEncoder().encode("abc")]]))) ===
  "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "FIPS 180-4 example");
const crc = await wasm("crc32", encode([["u64", 0], ["slice", new TextEncoder().encode("123456789")]]));
same("@crc32_z(\"123456789\")", new DataView(crc.buffer).getUint32(0, true) === 0xcbf43926, "check value cbf43926");

process.exit(failures ? 1 : 0);
