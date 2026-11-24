import {
  $, $$, el, fmt, getJSON, load, encode, hex, proofTag, terminal,
  section, drawIn, pulse, highlight, reduced
} from "../page.js";
import { diagram } from "../diagram.js";

const pretty = (obj) => JSON.stringify(obj, null, 2);

async function wait(ms) {
  if (reduced) return;
  return new Promise((r) => setTimeout(r, ms));
}

async function main() {
  const data = await getJSON("data/omnilift.json");

  await section("#fig-omnilift-pipe-wrap", async () => {
    const svgEl = $("#fig-omnilift-pipe");
    const d = await diagram(svgEl, {
      nodes: [
        { id: "src", label: "Source C / Rust", sub: "zlib, Mbed TLS, sha2", kind: "input" },
        { id: "clang", label: "clang -O1", sub: "sans déroulage", kind: "op" },
        { id: "llvm", label: "LLVM IR", sub: "opt mem2reg + LCSSA", kind: "op" },
        { id: "lift", label: "VG-IR élevé", sub: "FOLD_BOUNDED + Slice", kind: "ai" },
        { id: "cure", label: "Guérison UB", sub: "PRE / Result::Err", kind: "warn" },
        { id: "alive2", label: "Alive2", sub: "unroll = 8", kind: "ok" },
        { id: "fuzz", label: "Fuzzing", sub: "1 000 000 cas", kind: "ok" },
        { id: "verify", label: "vg verify", sub: "obligations internes", kind: "ok" },
        { id: "bin", label: "Binaires", sub: "ARM64 · x86_64 · WASM", kind: "out" },
      ],
      edges: [
        ["src", "clang"],
        ["clang", "llvm"],
        ["llvm", "lift"],
        ["lift", "cure"],
        ["cure", "alive2"],
        ["alive2", "fuzz"],
        ["fuzz", "verify"],
        ["verify", "bin"],
      ],
      dx: 164,
      dy: 84,
      bw: 138,
      bh: 52,
    });
    $("#fig-omnilift-pipe-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
    await drawIn(d);

    const status = $("#pipe-status");
    const btn = $("#btn-pipe-walk");
    const labels = [
      ["src", "clang", "Compilation à -O1 sans vectorisation ni déroulage…"],
      ["clang", "llvm", "Normalisation des boucles (mem2reg, LCSSA)…"],
      ["llvm", "lift", "Traduction mécanique des boucles en FOLD_BOUNDED…"],
      ["lift", "cure", "Détection et guérison d'UB (pointeur nul, bornes)…"],
      ["cure", "alive2", "Preuve bornée Alive2 (unroll = 8) via l'adaptateur…"],
      ["alive2", "fuzz", "Fuzzing différentiel sur 1 000 000 d'entrées…"],
      ["fuzz", "verify", "Vérification des obligations VG-IR par Z3…"],
      ["verify", "bin", "Compilation vers ARM64, x86_64 et WASM."],
    ];
    const edgeMap = new Map(d.edges.map((e) => [`${e.from}->${e.to}`, e]));

    btn.addEventListener("click", async () => {
      btn.disabled = true;
      for (const n of d.nodes.values()) n.classList.remove("on", "bad", "dim");
      for (const [a, b, msg] of labels) {
        status.textContent = msg;
        d.nodes.get(a).classList.add("on");
        await pulse(svgEl, edgeMap.get(`${a}->${b}`), 420, "");
        d.nodes.get(b).classList.add("on");
        await wait(120);
      }
      btn.disabled = false;
    });
  });

  await section("#sec-crc32", async () => {
    const c = data.crc32;
    const cv = data.crc32_verify;
    const a = data.adler32;
    const av = data.adler32_verify;

    $("#lift-stats").replaceChildren(
      el("div", { class: "stat" }, el("b", { text: `${fmt(cv.proven)} / ${fmt(cv.total)}` }), el("span", { text: "obligations prouvées pour @crc32_z (10 nœuds)" })),
      el("div", { class: "stat" }, el("b", { text: `${fmt(av.proven)} / ${fmt(av.total)}` }), el("span", { text: "obligations prouvées pour @adler32_z (30 nœuds)" })),
      el("div", { class: "stat" }, el("b", { text: fmt(c.report.fuzz_cases) }), el("span", { text: "cas de fuzzing différentiel par fonction" })),
      el("div", { class: "stat" }, el("b", { text: `unroll ${fmt(c.report.unroll)}` }), el("span", { text: `vérifié par ${c.report.bounded_check}` })),
    );

    const vgsPre = $("#lifted-vgsym");
    vgsPre.textContent = c.vgsym_tail;
    highlight(vgsPre.parentElement);

    const steps = [
      {
        cmd: "vg lift tests/lift/zlib/crc32.c crc32_z --lang c",
        out: pretty({
          node_hash: c.node_hash,
          root_label: c.root_label,
          report: c.report,
          provenance: c.provenance,
          vgsym_lines: c.vgsym_line_count,
        }),
      },
      {
        cmd: "vg verify crc32_lifted.vgs --mode edit --detail 1",
        out: pretty({
          ok: cv.ok,
          nodes: cv.nodes.length,
          proven: cv.proven,
          total: cv.total,
          sample: cv.sample.slice(0, 3),
        }),
      },
      {
        cmd: "vg lift tests/lift/zlib/adler32.c adler32_z --lang c",
        out: pretty({
          node_hash: a.node_hash,
          root_label: a.root_label,
          report: a.report,
          verify_ok: av.ok,
          verify_proven: `${av.proven}/${av.total}`,
        }),
      },
    ];
    terminal($("#term-lift"), steps, { title: "vg lift & vg verify — zlib crc32_z et adler32_z", speed: 16 });
  });

  await section("#fig-sha-class-wrap", async () => {
    const svgEl = $("#fig-sha-class");
    const d = await diagram(svgEl, {
      nodes: [
        { id: "c", label: "mbedtls_sha256_compress", sub: "C · Mbed TLS 4.3.0", kind: "input" },
        { id: "rust", label: "compress256", sub: "Rust · sha2 0.10.9", kind: "input" },
        { id: "att_c", label: "Attestation C", sub: "bounded(8) + 10⁶ fuzz", kind: "ok" },
        { id: "att_r", label: "Attestation Rust", sub: "bounded(8) + 10⁶ fuzz", kind: "ok" },
        { id: "spec", label: "SPEC @sha256_compress", sub: "classe de spécification Mesh", kind: "out" },
      ],
      edges: [
        ["c", "att_c"],
        ["rust", "att_r"],
        ["att_c", "spec"],
        ["att_r", "spec"],
      ],
      dx: 220,
      dy: 90,
      bw: 186,
      bh: 54,
    });
    $("#fig-sha-class-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
    await drawIn(d);
  });

  await section("#demo-crc", async () => {
    const runCrc = await load("wasm/crc32.wasm");
    const input = $("#crc-input");
    const got = $("#crc-got");
    const lenEl = $("#crc-len");
    const enc = new TextEncoder();

    const compute = async () => {
      const bytes = enc.encode(input.value);
      const out = await runCrc(encode([["u64", 0], ["slice", bytes]]));
      const be = new Uint8Array([out[3], out[2], out[1], out[0]]);
      got.textContent = hex(be);
      lenEl.textContent = `${fmt(bytes.length)} octet${bytes.length > 1 ? "s" : ""}`;
    };

    $("#crc-run-btn").addEventListener("click", compute);
    input.addEventListener("input", compute);
    for (const b of $$(".crc-preset")) {
      b.addEventListener("click", () => {
        input.value = b.dataset.val ?? "";
        compute();
      });
    }
    await compute();
  });
}

main();
