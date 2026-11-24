import {
  $, el, svg, fmt, getJSON, load, encode, f32s, u32, hex, play, CURVE, onFirstView,
  section, proofTag, terminal, drawIn,
} from "../page.js";
import { diagram } from "../diagram.js";

const data = await getJSON("data/compilation.json");

// ---- 7.1 Schéma du pipeline de compilation + extraits IR LLVM ------------------------------------

await section("#pipeline", async () => {
  const cgSvg = $("#fig-codegen");
  const d = await diagram(cgSvg, {
    nodes: [
      { id: "vgir", label: "Graphe vérifié", sub: "vg-smt UNSAT", kind: "ok", key: 0 },
      { id: "erase", label: "1. Effacement", sub: "gardes prouvées", kind: "op", key: 0 },
      { id: "loops", label: "2. Boucles", sub: "SIMD / FOLD", kind: "op", key: 0 },
      { id: "mem", label: "3. Arène", sub: "pile ou scratch", kind: "op", key: 0 },
      { id: "facts", label: "4. Faits LLVM", sub: "nsw · nuw · inbounds", kind: "ai", key: 0 },
      { id: "llvm", label: "5. LLVM IR", sub: "texte pur", kind: "op", key: 0 },
      { id: "clang", label: "6. clang + vg-rt", sub: "3 cibles + manifeste", kind: "out", key: 0 },
    ],
    edges: [
      ["vgir", "erase"],
      ["erase", "loops"],
      ["loops", "mem"],
      ["mem", "facts"],
      ["facts", "llvm"],
      ["llvm", "clang"],
    ],
    dx: 162,
    dy: 84,
    bw: 138,
    bh: 50,
  });
  onFirstView(cgSvg, () => drawIn(d));
  $("#codegen-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");

  $("#ir-midpoint-internal").textContent = data.midpoint_llvm.internal.trim();
  $("#ir-midpoint-checked").textContent = data.midpoint_llvm.checked.trim();
});

// ---- 7.2 Effacement des preuves -------------------------------------------------------------------

$("#erasure-ok-ir").textContent = data.erasure.verified.ir.trim();
$("#erasure-unv-ir").textContent = data.erasure.unverified.ir.trim();
$("#erasure-ok-meta").textContent = `verified: ${data.erasure.verified.verified} · residual_guards: ${data.erasure.verified.residual_guards} · ${fmt(data.erasure.verified.bytes)} octets`;
$("#erasure-unv-meta").textContent = `verified: ${data.erasure.unverified.verified} · residual_guards: ${data.erasure.unverified.residual_guards} (traps par op) · ${fmt(data.erasure.unverified.bytes)} octets`;
$("#guard-demo-count").textContent = String(data.erasure.with_plain_guard.residual_guards);

// ---- 7.3 Cibles et reproductibilité ---------------------------------------------------------------

const targetsBody = $("#targets-tbody");
if (targetsBody) {
  for (const r of data.reproducibility) {
    targetsBody.append(
      el("tr", {},
        el("td", {}, el("code", { text: r.target })),
        el("td", { text: `${fmt(r.bytes)} o` }),
        el("td", {}, el("code", { text: `${r.blake3_build1.slice(0, 18)}…`, title: r.blake3_build1 })),
        el("td", { class: r.identical ? "ok" : "miss", text: r.identical ? "identique (2/2)" : "différente" }),
        el("td", { class: "ok", text: String(r.residual_guards) }),
      )
    );
  }
}

terminal($("#term-build"), data.terminal_session, { title: "vg build --target x3 (2 builds)" });

// ---- 7.4 Manifestes -------------------------------------------------------------------------------

$("#manifest-writer").textContent = data.manifests.file_writer.trim();
$("#manifest-pure").textContent = data.manifests.pure_midpoint.trim();

// ---- 7.5 Benchmarks contre clang -O2 (bar_chart.wasm) ---------------------------------------------

await section("#bench-sec", async () => {
  const [bench, barWasm] = await Promise.all([getJSON("wasm/bench.json"), load("wasm/bar_chart.wasm")]);
  const rows = bench.kernels;
  const out = await barWasm(encode([["f32", rows.map((k) => k.ratio).concat([1.2, 1.0])]]));
  const frac = f32s(out, 0, 12);
  const tickVal = f32s(out, 48, 6);
  const tickPos = f32s(out, 72, 6);
  const nTicks = u32(out, 96);

  const chart = $("#bench-chart");
  const axis = el("div", { class: "axis", "aria-hidden": "true" });
  for (let k = 0; k <= nTicks; k++) {
    axis.append(el("div", { class: "t", style: `--p:${tickPos[k]}` }, el("span", { text: `${+tickVal[k].toFixed(2)}×` })));
  }
  axis.append(el("div", { class: "t gate", style: `--p:${frac[rows.length]}` }, el("span", { text: "objectif G4 : 1,20×" })));
  chart.append(axis);

  const SERIES = {
    "aarch64-apple-darwin": [`${bench.host.cpu} (natif)`, "var(--blue)"],
    "wasm32-wasip1": ["WebAssembly (wasmtime)", "var(--violet)"],
  };
  const fills = [];
  const names = [...new Set(rows.map((r) => r.name))];
  for (const name of names) {
    const row = el("div", { class: "krow" }, el("span", { text: name }));
    rows.forEach((r, i) => {
      if (r.name !== name) return;
      const fill = el("div", { class: "fill", style: `--f:${frac[i]};--c:${SERIES[r.target]?.[1] ?? "var(--muted)"}` });
      fills.push(fill);
      row.append(
        el("div", {
          class: "kbar",
          role: "img",
          "aria-label": `${name} sur ${r.target} : ${r.ratio.toFixed(2)} fois le temps du C`,
          style: `--f:${frac[i]}`,
        }, fill, el("em", { text: `${r.ratio.toFixed(2)}×` }))
      );
    });
    chart.append(row);
  }

  const keyBox = $("#bench-key");
  for (const [label, c] of Object.values(SERIES)) {
    keyBox.append(el("span", {}, el("i", { style: `--c:${c}` }), label));
  }
  onFirstView(chart, () => play(CURVE.OUT_CUBIC, 1200, (e) => fills.forEach((f) => f.style.setProperty("--e", e))));
  $("#bench-proof").innerHTML = await proofTag("@bar_chart", "∀ barre : 0 ≤ longueur ≤ 1, graduations 1-2-5");
  $("#bench-host").textContent = `Mesures : ${bench.host.cpu}, ${bench.host.os}, ${bench.host.clang}, ${bench.host.flags}. Tous les noyaux produisent des octets identiques au C.`;
});

// ---- 7.6 Démo en direct : limiteur audio M1 (tone.wasm + limiter.wasm) ----------------------------

await section("#audio-demo-fig", async () => {
  const [toneWasm, limiterWasm, peakWasm] = await Promise.all([load("wasm/tone.wasm"), load("wasm/limiter.wasm"), load("wasm/peak.wasm")]);
  const driveSlider = $("#drive-slider");
  const driveVal = $("#drive-val");
  const kneeSlider = $("#knee-slider");
  const kneeVal = $("#knee-val");
  // The limiter reads the frame, then its LEARNABLE knee: an implicit trailing input (docs/06).
  const limit = (frameBytes) => limiterWasm(encode([["bytes", frameBytes], ["f32", Number(kneeSlider.value)]]));
  const peakOf = async (frameBytes) => f32s(await peakWasm(encode([["bytes", frameBytes.subarray(0, 4096)]])), 0, 1)[0];
  const num = (v, d) => v.toFixed(d).replace(".", ",");
  const waveSvg = $("#wave-svg");
  const digestEl = $("#frame-digest");
  const peakEl = $("#frame-peaks");
  const statusEl = $("#audio-status");

  const gPlot = svg("g", { transform: "scale(1,-1)" });
  gPlot.append(
    svg("line", { class: "wave-axis", x1: 0, y1: 0, x2: 511, y2: 0 }),
    svg("line", { class: "wave-bound", x1: 0, y1: 1, x2: 511, y2: 1 }),
    svg("line", { class: "wave-bound", x1: 0, y1: -1, x2: 511, y2: -1 })
  );
  const rawLine = svg("polyline", { class: "wave-raw", points: "" });
  const limLine = svg("polyline", { class: "wave-lim", points: "" });
  gPlot.append(rawLine, limLine);

  waveSvg.replaceChildren(gPlot); // stretched to its box: the ±1 lines are labelled in the caption

  const updateWave = async () => {
    const drive = Number(driveSlider.value);
    driveVal.textContent = `${num(drive, 2)}×`;
    kneeVal.textContent = num(Number(kneeSlider.value), 2);
    const rawBytes = await toneWasm(encode([["u32", 0], ["f32", drive]]));
    const limOut = await limit(rawBytes);
    const [peakRaw, peakLim] = await Promise.all([peakOf(rawBytes), peakOf(limOut)]);
    const raw = f32s(rawBytes, 0, 1024);
    const lim = f32s(limOut, 0, 1024);
    const digest = hex(limOut.subarray(4096, 4128));

    // Affiche le canal gauche (indices pairs 0, 2, 4...) des 512 échantillons.
    const leftRaw = [];
    const leftLim = [];
    for (let i = 0; i < 512; i++) {
      leftRaw.push(`${i},${raw[2 * i].toFixed(4)}`);
      leftLim.push(`${i},${lim[2 * i].toFixed(4)}`);
    }
    waveSvg.setAttribute("viewBox", "-6 -9.6 524 19.2");
    rawLine.setAttribute("points", leftRaw.join(" "));
    limLine.setAttribute("points", leftLim.join(" "));
    digestEl.textContent = digest;
    peakEl.textContent = `Pic brut : ${num(peakRaw, 3)} · pic limité : ${num(peakLim, 3)} (peak.wasm)`;
  };

  let audioCtx = null;
  let activeSource = null;
  let playGen = 0;

  const stopAudio = () => {
    playGen++;
    if (activeSource) {
      try { activeSource.stop(); } catch { /* déjà arrêté */ }
      activeSource = null;
    }
    statusEl.textContent = "Arrêté.";
  };

  const playSignal = async (limited) => {
    const me = ++playGen;
    if (activeSource) {
      try { activeSource.stop(); } catch { /* déjà arrêté */ }
      activeSource = null;
    }
    const drive = Number(driveSlider.value);
    const FRAMES = 172; // 172 * 512 = 88 064 échantillons (~2,0 s à 44,1 kHz), avec 172 < 256 (PRE de @tone).
    statusEl.textContent = limited
      ? "Calcul de 172 trames via tone.wasm puis limiter.wasm…"
      : "Calcul de 172 trames via tone.wasm…";

    const AudioCtor = window.AudioContext || window.webkitAudioContext;
    if (!AudioCtor) {
      statusEl.textContent = "Web Audio API indisponible dans ce navigateur.";
      return;
    }
    audioCtx ??= new AudioCtor({ sampleRate: 44100 });
    if (audioCtx.state === "suspended") await audioCtx.resume();

    const buf = audioCtx.createBuffer(2, FRAMES * 512, 44100);
    const chL = buf.getChannelData(0);
    const chR = buf.getChannelData(1);

    for (let f = 0; f < FRAMES; f++) {
      if (me !== playGen) return;
      const rawBytes = await toneWasm(encode([["u32", f], ["f32", drive]]));
      const srcBytes = limited ? await limit(rawBytes) : rawBytes;
      const samples = f32s(srcBytes, 0, 1024);
      const base = f * 512;
      for (let i = 0; i < 512; i++) {
        chL[base + i] = samples[2 * i];
        chR[base + i] = samples[2 * i + 1];
      }
    }
    if (me !== playGen) return;

    const src = audioCtx.createBufferSource();
    src.buffer = buf;
    src.connect(audioCtx.destination);
    src.onended = () => {
      if (activeSource === src) {
        activeSource = null;
        statusEl.textContent = "Lecture terminée.";
      }
    };
    activeSource = src;
    src.start();
    statusEl.textContent = limited
      ? `Lecture du signal limité (~2 s, drive ${drive.toFixed(2)}×, borné dans [-1, 1]).`
      : `Lecture du signal brut (~2 s, drive ${drive.toFixed(2)}× — écrêtage matériel au-delà de ±1 !).`;
  };

  driveSlider.addEventListener("input", updateWave);
  kneeSlider.addEventListener("input", updateWave);
  $("#btn-play-raw").addEventListener("click", () => playSignal(false));
  $("#btn-play-lim").addEventListener("click", () => playSignal(true));
  $("#btn-stop").addEventListener("click", stopAudio);

  await updateWave();
  $("#limiter-proof").innerHTML = await proofTag("@limit_frame", "∀ échantillon : résultat dans [-1, 1], signe conservé");
});
