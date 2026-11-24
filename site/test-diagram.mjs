// Usage: node site/test-diagram.mjs
// Differential test of site/wasm/layout.wasm against `vg run site/vg/diagram.vgs @layout`.
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { run, encode } from "./js/vg.js";

const root = new URL("..", import.meta.url).pathname;
const VG = `${root}target/release/vg`;
const VGS = "site/vg/diagram.vgs";
let failures = 0;

const layoutModule = await WebAssembly.compile(readFileSync(`${root}site/wasm/layout.wasm`));
const wasmLayout = (input) => run(layoutModule, input);

const interp = (args) => {
  const r = JSON.parse(
    execFileSync(VG, ["run", VGS, "@layout", JSON.stringify(args)], {
      encoding: "utf8",
      maxBuffer: 1 << 26,
    })
  );
  if (!r.ok) throw new Error(`@layout: ${r.error}`);
  return r.outputs;
};

const flat = (v) => (Array.isArray(v) ? v.flatMap(flat) : [v]);

function packSpec(spec) {
  const nodes = spec.nodes;
  const fwd = spec.edges ?? [];
  const back = spec.back ?? [];
  const indexOf = new Map(nodes.map((n, i) => [n.id, i]));
  const key = new Array(32).fill(0);
  const minl = new Array(32).fill(0);
  nodes.forEach((n, i) => {
    key[i] = n.key ?? i;
    minl[i] = n.min ?? 0;
  });
  const es = new Array(64).fill(0xffffffff);
  let slot = 0;
  for (const [u, v] of fwd) {
    es[slot++] = (indexOf.get(u) << 8) | indexOf.get(v);
  }
  for (const [u, v] of back) {
    es[slot++] = (1 << 16) | (indexOf.get(u) << 8) | indexOf.get(v);
  }
  const g = [spec.dx ?? 200, spec.dy ?? 84, spec.bw ?? 156, spec.bh ?? 52];
  const vert = Boolean(spec.vertical);

  const bytes = encode([
    ["u32", nodes.length],
    ["u32", es],
    ["f32", key],
    ["u32", minl],
    ["f32", g],
    ["bool", vert],
  ]);
  const jsonArgs = {
    n: nodes.length,
    es,
    key: key.map((x) => String(Math.fround(x))),
    minl,
    g: g.map((x) => String(Math.fround(x))),
    vert,
  };
  return { bytes, jsonArgs };
}

function check(name, bytes, scalars) {
  const view = new DataView(bytes.buffer);
  let at = 0;
  let bad = 0;
  scalars.forEach((s, i) => {
    const got = view.getFloat32(at, true);
    const want = Math.fround(Number(s));
    at += 4;
    if (!Object.is(got, want) && bad++ < 3) {
      console.error(`  ${name}[${i}]: binary ${got}, vg-interp ${want}`);
    }
  });
  if (at !== bytes.length) {
    bad++;
    console.error(`  ${name}: ${bytes.length} bytes, expected ${at}`);
  }
  console.log(`${bad ? "FAIL" : "ok  "} ${name}: ${scalars.length} values`);
  if (bad) failures++;
}

const specs = [
  {
    name: "4-node chain",
    spec: {
      nodes: [{ id: "a" }, { id: "b" }, { id: "c" }, { id: "d" }],
      edges: [
        ["a", "b"],
        ["b", "c"],
        ["c", "d"],
      ],
    },
  },
  {
    name: "12-crate toolchain",
    spec: {
      nodes: [
        { id: "vg-core" },
        { id: "vg-store" },
        { id: "vg-types" },
        { id: "vg-interp" },
        { id: "vg-smt" },
        { id: "vg-codegen" },
        { id: "vg-rt" },
        { id: "vg-decode" },
        { id: "vg-telemetry" },
        { id: "vg-mesh" },
        { id: "vg-omnilift" },
        { id: "vg-cli" },
      ],
      edges: [
        ["vg-core", "vg-store"],
        ["vg-core", "vg-types"],
        ["vg-types", "vg-interp"],
        ["vg-types", "vg-smt"],
        ["vg-interp", "vg-smt"],
        ["vg-smt", "vg-codegen"],
        ["vg-codegen", "vg-rt"],
        ["vg-core", "vg-decode"],
        ["vg-interp", "vg-telemetry"],
        ["vg-smt", "vg-telemetry"],
        ["vg-core", "vg-mesh"],
        ["vg-codegen", "vg-omnilift"],
        ["vg-smt", "vg-cli"],
        ["vg-codegen", "vg-cli"],
        ["vg-mesh", "vg-cli"],
        ["vg-decode", "vg-cli"],
        ["vg-telemetry", "vg-cli"],
        ["vg-omnilift", "vg-cli"],
      ],
    },
  },
  {
    name: "6-node vertical with back edge",
    spec: {
      nodes: [
        { id: "n0", key: 1.5, min: 0 },
        { id: "n1", key: 0.5, min: 0 },
        { id: "n2", key: 0.5, min: 1 },
        { id: "n3", key: 2.0, min: 0 },
        { id: "n4", key: 1.0, min: 2 },
        { id: "n5", key: 0.0, min: 0 },
      ],
      edges: [
        ["n0", "n1"],
        ["n0", "n2"],
        ["n1", "n3"],
        ["n2", "n4"],
        ["n3", "n5"],
        ["n4", "n5"],
      ],
      back: [["n5", "n0"]],
      vertical: true,
      dx: 180,
      dy: 90,
      bw: 140,
      bh: 48,
    },
  },
];

for (const { name, spec } of specs) {
  const { bytes, jsonArgs } = packSpec(spec);
  check(name, await wasmLayout(bytes), flat(interp(jsonArgs)));
}

// Checked entry must refuse n = 0 (breaks PRE: n >= 1 && n <= 32).
const invalidInput = encode([
  ["u32", 0],
  ["u32", new Array(64).fill(0xffffffff)],
  ["f32", new Array(32).fill(0)],
  ["u32", new Array(32).fill(0)],
  ["f32", [200, 84, 156, 52]],
  ["bool", false],
]);
try {
  await wasmLayout(invalidInput);
  console.error("FAIL @layout accepted n = 0 outside PRE");
  failures++;
} catch (e) {
  const ok = String(e.message).startsWith("exit 1");
  console.log(`${ok ? "ok  " : "FAIL"} @layout refuses n = 0: ${e.message.split("\n")[0]}`);
  if (!ok) failures++;
}

process.exit(failures ? 1 : 0);
