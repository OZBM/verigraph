// Chapter 11: the site's own library, motion table, proof counts, fractal explorer and colophon.
// Every value drawn comes from a binary in wasm/ or from the build's reports (proofs.json, builds.json).
import { $, $$, el, svg, fmt, load, encode, f32s, u32, motion, play, CURVE, onFirstView, section, proofTag, siteReports, drawIn, reduced } from "../page.js";
import { diagram } from "../diagram.js";

const narrow = matchMedia("(max-width: 720px)").matches;
const getText = (url) => fetch(url).then((r) => (r.ok ? r.text() : Promise.reject(new Error(`${url} : ${r.status}`))));
const { proofs, builds } = await siteReports();

// ---- 11.1 How the site is made -------------------------------------------------------------------

await section("#pipe-fig", async () => {
  const d = await diagram($("#pipe-fig"), {
    nodes: [
      { id: "src", label: "sources .vgs", sub: "weblib, diagram, demos…", kind: "input", key: 0 },
      { id: "ex", label: "exemples", sub: "examples/*.vgs, zlib", kind: "input", key: 1 },
      { id: "verify", label: "vg verify", sub: `${proofs.modules.length} modules prouvés`, kind: "ok", key: 0 },
      { id: "build", label: "vg build", sub: "wasm32-wasip1", kind: "op", key: 0 },
      { id: "runs", label: "vg run, explain…", sub: "scripts des chapitres", kind: "op", key: 2 },
      { id: "wasm", label: "wasm/*.wasm", sub: `${builds.binaries.length} binaires`, kind: "out", key: 0 },
      { id: "json", label: "data/*.json", sub: "rapports réels", kind: "out", key: 2 },
      { id: "page", label: "la page", sub: "JS : temps et octets", kind: "ai", key: 1 },
    ],
    edges: [["src", "verify"], ["ex", "verify"], ["verify", "build"], ["build", "wasm"], ["ex", "runs"], ["runs", "json"], ["wasm", "page"], ["json", "page"], ["verify", "json"]],
    vertical: narrow,
  });
  $("#pipe-proof").innerHTML = await proofTag("@layout");
  onFirstView($("#pipe-fig"), () => drawIn(d));
});

// ---- 11.2 The library ------------------------------------------------------------------------------

const LIBRARY = [
  ["site/vg/weblib.vgs", "@ease_out_bounce", "easeOutBounce, tel que jQuery UI, anime.js et GSAP le fournissent"],
  ["site/vg/weblib.vgs", "@cubic_bezier", "cubic-bezier() de CSS, résolu comme le UnitBezier de WebKit"],
  ["site/vg/weblib.vgs", "@sin_turns", "Math.sin, reconstruit à partir d'un polynôme"],
  ["site/vg/weblib.vgs", "@rgba", "un pixel d'un ImageData de canvas"],
  ["site/vg/weblib.vgs", "@bar_chart", "d3.max, d3.ticks, nice() et scaleLinear"],
  ["site/vg/weblib.vgs", "@donut", "d3.pie et d3.arc"],
  ["site/vg/weblib.vgs", "@aurora", "un fragment shader WebGL"],
  ["site/vg/weblib.vgs", "@zoom", "le contrôleur d'une carte ou d'une visionneuse"],
  ["site/vg/diagram.vgs", "@layout", "dagre ou ELK : la mise en page des schémas en couches"],
  ["site/vg/demos.vgs", "@grid", "la graduation d'un axe de graphique"],
  ["site/vg/demos.vgs", "@knee_curve", "la courbe du chapitre 6, version réparée"],
  ["site/vg/demos.vgs", "@tone", "un oscillateur Web Audio"],
];

await section("#lib", async () => {
  const files = Object.fromEntries(await Promise.all([...new Set(LIBRARY.map(([f]) => f))].map(async (f) => [f, await getText(f.replace("site/", ""))])));
  const own = Object.fromEntries(proofs.by_node.map((n) => [`${n.module}:${n.label}`, n]));
  const box = $("#lib");
  for (const [file, label, from] of LIBRARY) {
    const block = files[file].split(`NODE ${label} [`)[1]?.split("BODY")[0] ?? "";
    const contract = block.split("\n").map((l) => l.trim()).filter((l) => /^(PRE|POST|INVARIANT)/.test(l));
    const pre = el("pre");
    for (const line of contract) {
      const [kw, ...rest] = line.split(":");
      pre.append(el("span", { class: "k", text: kw }), `:${rest.join(":")}\n`);
    }
    if (!contract.length) pre.append("(ni PRE ni POST : une aide totale)\n");
    const module = file.split("/").pop().replace(".vgs", "");
    const p = own[`${module}:${label}`];
    box.append(el("article", { class: "fn reveal" },
      el("h3", { text: label }),
      el("p", { class: "from", text: `remplace ${from}` }),
      pre,
      el("div", { class: "meta" }, el("span", {}, el("b", { text: p ? `${p.proven}/${p.n}` : "—" }), " prouvées"), el("span", { text: file.replace("site/vg/", "") }))));
  }
  for (const [i, card] of [...box.children].entries()) {
    if (reduced) continue;
    card.style.setProperty("--e", 0);
    onFirstView(card, async () => (await motion, play(CURVE.OUT_CUBIC, 850, (e) => card.style.setProperty("--e", e), (i % 3) * 60)), 0.1);
  }
});

// ---- 11.3 Motion -------------------------------------------------------------------------------------

const CURVES = [
  ["@ease_in_quad", "easeInQuad", "var(--blue)"],
  ["@ease_out_cubic", "easeOutCubic", "var(--proven)"],
  ["@ease_in_out_cubic", "easeInOutCubic", "var(--teal)"],
  ["@ease_out_quart", "easeOutQuart", "var(--sky)"],
  ["@ease_in_out_quint", "easeInOutQuint", "var(--violet)"],
  ["@ease_out_back", "easeOutBack", "var(--pink)"],
  ["@ease_out_bounce", "easeOutBounce", "var(--amber)"],
  ["@css_ease", "CSS ease", "var(--ink)"],
];

await section("#curves", async () => {
  const table = await motion;
  if (!table) throw new Error("la table de @motion manque");
  const box = $("#curves");
  const cards = CURVES.map(([label, name, color], k) => {
    const g = svg("g", { transform: "scale(1,-1)" });
    g.append(svg("polyline", { class: "grid", points: "0,0 255,0" }), svg("polyline", { class: "grid", points: "0,1 255,1" }));
    g.append(svg("polyline", { class: "line", points: table.slice(k * 256, k * 256 + 256).map((v, j) => `${j},${v}`).join(" "), stroke: color }));
    const head = svg("line", { stroke: color, "stroke-width": 9, "stroke-linecap": "round", "vector-effect": "non-scaling-stroke", x1: 255, y1: 1, x2: 255, y2: 1 });
    g.append(head);
    const plot = svg("svg", { viewBox: "-6 -1.2 267 1.32", preserveAspectRatio: "none", "aria-hidden": "true" });
    plot.append(g);
    const puck = el("span", { class: "puck" });
    box.append(el("figure", { class: "panel curve", style: `--c:${color}` }, el("h3", {}, name, el("small", { text: label })), plot, el("div", { class: "track" }, puck)));
    return { k, head, puck };
  });
  const run = () => Promise.all(cards.map(({ k, head, puck }) => play(k, 1700, (v, j) => {
    puck.style.setProperty("--e", v);
    for (const a of ["x1", "x2"]) head.setAttribute(a, j);
    for (const a of ["y1", "y2"]) head.setAttribute(a, v);
  })));
  $("#replay").addEventListener("click", run);
  onFirstView(box, run);
  $("#motion-proof").innerHTML = await proofTag("@motion", "∀ courbe : e(0) = 0 et e(1) = 1");
});

// ---- 11.4 The site's proofs ------------------------------------------------------------------------

const KINDS = {
  POST: ["postconditions", "ce que chaque fonction promet"],
  NONVACUOUS: ["contrats satisfiables", "au moins une entrée respecte PRE"],
  PRE: ["contrats bien définis", "chaque terme lu par PRE est défini"],
  CALL: ["préconditions des appels", "chaque CALL_HASH respecte le PRE appelé"],
  CAST: ["conversions dans les bornes", "aucun cast flottant → entier ne déborde"],
  GEN: ["préconditions de GENERATE", "le PRE du noyau, pour chaque indice"],
  FOLD: ["invariants de boucle", "cas de base, pas, PRE du noyau"],
  MAP: ["préconditions de SIMD_MAP", "le PRE du noyau, pour chaque élément"],
  INDEX: ["lectures dans les bornes", "aucun indice après la fin"],
  ADD: ["pas de débordement", "additions entières"],
  MUL: ["pas de débordement", "multiplications entières"],
  SUB: ["pas de soustraction négative", "soustractions entières"],
  DIV: ["pas de division par zéro", "divisions entières"],
  MOD: ["pas de division par zéro", "restes entiers"],
  SHL: ["décalages dans les bornes", "décalage inférieur à la largeur"],
  other: ["toutes les autres", "les petites sortes réunies"],
};
const PALETTE = ["var(--proven)", "var(--blue)", "var(--violet)", "var(--amber)", "var(--teal)", "var(--pink)", "var(--sky)", "var(--muted)"];
const SOLVER = { none: "pré-criblage", z3: "z3", bitwuzla: "Bitwuzla", cvc5: "cvc5" };
const EVIDENCE = { "re-solved": "résolue", cached: "en cache", prescreen: "pré-criblage", "two-solvers": "deux solveurs" };

await section("#preuves", async () => {
  const [donut, barChart] = await Promise.all([load("wasm/donut.wasm"), load("wasm/bar_chart.wasm")]);
  const kinds = proofs.kinds8;
  const values = kinds.map((k) => k.n).concat(Array(8 - kinds.length).fill(0));
  const ring = $("#donut");
  const paths = kinds.map((k, s) => {
    const p = svg("path", { fill: PALETTE[s] });
    p.append(svg("title"));
    p.firstChild.textContent = `${KINDS[k.kind]?.[0] ?? k.kind} : ${k.n}`;
    ring.append(p);
    return p;
  });
  // Draws the ring at a sweep in [0, 1]; frames that arrive while one is computing are skipped.
  let pending = null, drawing = false;
  const draw = async (sweep) => {
    pending = sweep;
    if (drawing) return;
    drawing = true;
    while (pending !== null) {
      const s = pending;
      pending = null;
      const xy = f32s(await donut(encode([["f32", values], ["f32", s]])), 0, 1056);
      paths.forEach((p, seg) => p.setAttribute("d", `M${Array.from({ length: 66 }, (_, i) => `${xy[seg * 132 + 2 * i].toFixed(4)},${xy[seg * 132 + 2 * i + 1].toFixed(4)}`).join("L")}Z`));
    }
    drawing = false;
  };
  await draw(1);
  $("#donut-total").textContent = fmt(proofs.total);
  const legend = $("#legend");
  kinds.forEach((k, s) => {
    const [name, note] = KINDS[k.kind] ?? [k.kind, ""];
    const li = el("li", { tabindex: "0" }, el("i", { style: `--c:${PALETTE[s]}` }), el("span", {}, name, el("small", { text: `${k.kind} · ${note}` })), el("b", { text: fmt(k.n) }));
    const focus = (on) => paths.forEach((p, i) => p.classList.toggle("dim", on && i !== s));
    li.addEventListener("pointerenter", () => focus(true));
    li.addEventListener("pointerleave", () => focus(false));
    li.addEventListener("focus", () => focus(true));
    li.addEventListener("blur", () => focus(false));
    legend.append(li);
  });
  await motion;
  onFirstView(ring, () => play(CURVE.OUT_QUART, 1500, (e) => draw(e)));
  $("#donut-proof").innerHTML = await proofTag("@donut", "∀ coordonnée : |x| ≤ 1");

  // Bars: @bar_chart turns counts into fractions of a nice axis.
  const bars = async (host, rows, names) => {
    const frac = f32s(await barChart(encode([["f32", rows.map((r) => r.n).concat(Array(12 - rows.length).fill(0))]])), 0, 12);
    const fills = rows.map((r, i) => {
      const fill = el("div", { class: "fill", style: `--f:${frac[i]};--c:${PALETTE[i]}` });
      host.append(el("div", { class: "hbar" }, el("span", { text: names[r.key] ?? r.key }), el("div", { class: "rail" }, fill), el("b", { text: fmt(r.n) })));
      return fill;
    });
    onFirstView(host, () => play(CURVE.OUT_BACK, 1200, (e) => fills.forEach((f) => f.style.setProperty("--e", e))));
  };
  await bars($("#solvers"), proofs.by_solver.map((s) => ({ key: s.solver, n: s.n })), SOLVER);
  await bars($("#evidence"), proofs.by_evidence.map((s) => ({ key: s.evidence, n: s.n })), EVIDENCE);
});

// ---- 11.5 Fractal explorer -------------------------------------------------------------------------

await section("#fractale", async () => {
  const [zoom, render] = await Promise.all([load("wasm/zoom.wasm"), load("wasm/render.wasm")]);
  const canvas = $("#fractal-canvas");
  const ctx = canvas.getContext("2d");
  const HOME = [-0.5, 0, 0.0125];
  const num = (v, f) => f(v).replace(".", ",").replace("-", "−");
  let view = HOME, busy = false;
  const show = async (v) => {
    canvas.classList.add("busy");
    const start = performance.now();
    const px = await render(encode([["f32", v]]));
    ctx.putImageData(new ImageData(new Uint8ClampedArray(px.buffer), 256, 192), 0, 0);
    $("#fx-ms").textContent = `${Math.round(performance.now() - start)} ms`;
    $("#fx-cx").textContent = num(v[0], (x) => x.toPrecision(8));
    $("#fx-cy").textContent = num(v[1], (x) => x.toPrecision(8));
    $("#fx-s").textContent = num(v[2], (x) => x.toExponential(3));
    canvas.classList.remove("busy");
  };
  const step = async (px, py, zin) => {
    if (busy) return;
    busy = true;
    try {
      view = f32s(await zoom(encode([["f32", view], ["u32", [px, py]], ["bool", zin]])), 0, 3);
      await show(view);
    } finally {
      busy = false;
    }
  };
  canvas.addEventListener("click", (e) => {
    const r = canvas.getBoundingClientRect();
    // The click, as a pixel of the 256 x 192 image.
    const px = Math.min(255, Math.max(0, Math.floor(((e.clientX - r.left) / r.width) * 256)));
    const py = Math.min(191, Math.max(0, Math.floor(((e.clientY - r.top) / r.height) * 192)));
    step(px, py, !e.shiftKey);
  });
  canvas.addEventListener("keydown", (e) => {
    if (e.key === "Enter" || e.key === "+") e.preventDefault(), step(128, 96, true);
    if (e.key === "-") e.preventDefault(), step(128, 96, false);
  });
  $("#fx-reset").addEventListener("click", async () => {
    if (busy) return;
    busy = true;
    try { view = HOME; await show(view); } finally { busy = false; }
  });
  onFirstView(canvas, () => show(view), 0.05);
  $("#fractal-proof").innerHTML = `${await proofTag("@zoom", "la nouvelle vue respecte le PRE de @render")}<br>${await proofTag("@render", "∀ pixel : opaque")}`;
});

// ---- 11.6 Colophon ---------------------------------------------------------------------------------

await section("#colophon", async () => {
  const body = $("#builds tbody");
  for (const b of builds.binaries) {
    body.append(el("tr", {},
      el("td", { text: `${b.name}.wasm` }),
      el("td", { text: b.node }),
      el("td", { text: b.verified ? fmt(b.closure_nodes) : "—" }),
      el("td", { class: b.verified ? "ok" : "miss", text: b.verified ? `${fmt(b.proven)} / ${fmt(b.obligations)}` : "non vérifié" }),
      el("td", { class: "h", text: `${b.blake3.slice(0, 16)}…`, title: b.blake3 }),
      el("td", { text: `${String(b.kb).replace(".", ",")} Ko` }),
      el("td", { class: "ok", text: String(b.residual_guards) })));
  }
  const mods = $("#modules tbody");
  for (const m of proofs.modules) {
    mods.append(el("tr", {}, el("td", { text: m.file }), el("td", { text: fmt(m.nodes) }),
      el("td", { class: "ok", text: `${fmt(m.proven)} / ${fmt(m.total)}` }), el("td", { text: `${fmt(m.solver_ms)} ms` })));
  }
});
