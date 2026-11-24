import {
  $, el, svg, fmt, getJSON, load, encode, f32s, play, CURVE, onFirstView,
  section, proofTag, terminal, drawIn, pulse,
} from "../page.js";
import { diagram } from "../diagram.js";

const data = await getJSON("data/preuves.json");

// Topologies des nœuds explorés (quickstart.vgs et mandelbrot.vgs) avec correspondance vers les ports canoniques.
const GRAPHS = {
  "@add_sat": {
    nodes: [
      { id: "a", label: "a: u8", kind: "input", key: 0 },
      { id: "b", label: "b: u8", kind: "input", key: 1 },
      { id: "p0", label: "%0 ADD.sat", sub: "saturant", kind: "ok", obPort: "%0" },
      { id: "ret", label: "RETURN", sub: "u8", kind: "out" },
    ],
    edges: [["a", "p0"], ["b", "p0"], ["p0", "ret"]],
  },
  "@midpoint": {
    nodes: [
      { id: "a", label: "a: u32", kind: "input", key: 0 },
      { id: "b", label: "b: u32", kind: "input", key: 1 },
      { id: "p0", label: "%0 AND", sub: "bits communs", kind: "op", key: 0 },
      { id: "p1", label: "%1 XOR", sub: "bits impairs", kind: "op", key: 1 },
      { id: "p2", label: "%2 SHR.checked", sub: "SHR.amount", kind: "warn", obPort: "%2" },
      { id: "p3", label: "%3 ADD.checked", sub: "ADD.no_overflow", kind: "warn", obPort: "%3" },
      { id: "ret", label: "RETURN", sub: "(a + b) / 2", kind: "out" },
    ],
    edges: [["a", "p0"], ["b", "p0"], ["a", "p1"], ["b", "p1"], ["p1", "p2"], ["p0", "p3"], ["p2", "p3"], ["p3", "ret"]],
  },
  "@add_step": {
    nodes: [
      { id: "acc", label: "acc: u32", sub: "<= 3000", kind: "input", key: 0 },
      { id: "x", label: "x: u32", sub: "<= 1000", kind: "input", key: 1 },
      { id: "p0", label: "%0 ADD.checked", sub: "ADD.no_overflow", kind: "warn", obPort: "%0" },
      { id: "ret", label: "RETURN", sub: "acc + x", kind: "out" },
    ],
    edges: [["acc", "p0"], ["x", "p0"], ["p0", "ret"]],
  },
  "@sum4": {
    nodes: [
      { id: "xs", label: "xs: Array<u32,4>", sub: "xs[i] <= 1000", kind: "input", key: 0 },
      { id: "p0", label: "%0 FOLD_BOUNDED", sub: "FOLD.base/pre/step", kind: "warn", obPort: "%0" },
      { id: "ret", label: "RETURN", sub: "<= 4000", kind: "out" },
    ],
    edges: [["xs", "p0"], ["p0", "ret"]],
  },
  "@soft_knee": {
    nodes: [
      { id: "x", label: "x: f32", sub: "is_finite(x)", kind: "input", key: 0 },
      { id: "knee", label: "knee: f32", sub: "[0.5, 0.99]", kind: "input", key: 1 },
      { id: "p0", label: "FABS(x)", kind: "op" },
      { id: "p3", label: "1 - knee", kind: "op" },
      { id: "p4", label: "t = excess / (1-k)", kind: "op" },
      { id: "p7", label: "1 - 1/(1+t)", kind: "ok" },
      { id: "p8", label: "FMA", kind: "op" },
      { id: "p9", label: "FCOPYSIGN", sub: "FCOPYSIGN.sign", kind: "warn", obPort: "%10" },
      { id: "p11", label: "SELECT", kind: "op" },
      { id: "p12", label: "FCLAMP", sub: "FCLAMP.bounds", kind: "warn", obPort: "%12" },
      { id: "ret", label: "RETURN", sub: "[-1, 1]", kind: "out" },
    ],
    edges: [
      ["x", "p0"], ["p0", "p4"], ["knee", "p3"], ["p3", "p4"], ["p4", "p7"],
      ["p3", "p8"], ["p7", "p8"], ["knee", "p8"], ["p8", "p9"], ["x", "p9"],
      ["p0", "p11"], ["x", "p11"], ["p9", "p11"], ["p11", "p12"], ["p12", "ret"],
    ],
  },
  "@escape_step": {
    nodes: [
      { id: "z", label: "z: Tuple", sub: "z.2 < 64", kind: "input", key: 0 },
      { id: "c", label: "cr, ci", kind: "input", key: 1 },
      { id: "norm", label: "|z|^2 <= 4", kind: "op", key: 0 },
      { id: "reim", label: "z^2 + c", kind: "op", key: 1 },
      { id: "inc", label: "%14 ADD.checked", sub: "ADD.no_overflow", kind: "warn", obPort: "%14", key: 2 },
      { id: "sel", label: "SELECT x3", kind: "op" },
      { id: "ret", label: "RETURN", sub: "Tuple", kind: "out" },
    ],
    edges: [["z", "norm"], ["z", "reim"], ["c", "reim"], ["z", "inc"], ["norm", "sel"], ["reim", "sel"], ["inc", "sel"], ["sel", "ret"]],
  },
  "@palette_entry": {
    nodes: [
      { id: "j", label: "j: u64", sub: "j < 16", kind: "input" },
      { id: "t", label: "t = (j+0.5)/16", kind: "op" },
      { id: "g", label: "%8 CAST.u8 (G)", sub: "CAST.in_range", kind: "warn", obPort: "%8", key: 0 },
      { id: "r", label: "%14 CAST.u8 (R)", sub: "CAST.in_range", kind: "warn", obPort: "%14", key: 1 },
      { id: "b", label: "%20 CAST.u8 (B)", sub: "CAST.in_range", kind: "warn", obPort: "%20", key: 2 },
      { id: "shlg", label: "%10 SHL.checked", sub: "SHL.amount", kind: "warn", obPort: "%10", key: 0 },
      { id: "shlb", label: "%22 SHL.checked", sub: "SHL.amount", kind: "warn", obPort: "%22", key: 2 },
      { id: "or", label: "OR alpha 0xff", kind: "op" },
      { id: "ret", label: "RETURN", sub: "u32 RGBA", kind: "out" },
    ],
    edges: [["j", "t"], ["t", "g"], ["t", "r"], ["t", "b"], ["g", "shlg"], ["b", "shlb"], ["shlg", "or"], ["r", "or"], ["shlb", "or"], ["or", "ret"]],
  },
  "@pixel": {
    nodes: [
      { id: "i", label: "i: u64", sub: "i < 49152", kind: "input", key: 0 },
      { id: "pal", label: "palette", sub: "Array<u32,16>", kind: "input", key: 1 },
      { id: "mod", label: "%1 MOD.checked", sub: "MOD.nonzero", kind: "warn", obPort: "%1", key: 0 },
      { id: "div", label: "%6 DIV.checked", sub: "DIV.nonzero", kind: "warn", obPort: "%6", key: 1 },
      { id: "fold", label: "%12 FOLD_BOUNDED", sub: "FOLD.base/pre/step", kind: "warn", obPort: "%12" },
      { id: "idx", label: "%17 INDEX", sub: "INDEX.in_bounds", kind: "warn", obPort: "%17" },
      { id: "ret", label: "RETURN", sub: "pixel u32", kind: "out" },
    ],
    edges: [["i", "mod"], ["i", "div"], ["mod", "fold"], ["div", "fold"], ["fold", "idx"], ["pal", "idx"], ["idx", "ret"]],
  },
  "@render": {
    nodes: [
      { id: "view", label: "cx, cy, scale", sub: "scale <= 0.02", kind: "input" },
      { id: "gen0", label: "%0 GENERATE", sub: "GEN.pre.0 (@palette)", kind: "warn", obPort: "%0" },
      { id: "gen1", label: "%1 GENERATE", sub: "GEN.pre.0/1 (@pixel)", kind: "warn", obPort: "%1" },
      { id: "ret", label: "RETURN", sub: "Array<49152>", kind: "out" },
    ],
    edges: [["view", "gen0"], ["view", "gen1"], ["gen0", "gen1"], ["gen1", "ret"]],
  },
};

// ---- 5.2 Explorateur interactif d'obligations ------------------------------------------------------

await section("#ob-explorer", async () => {
  const allNodes = data.modules.flatMap((m) => m.nodes.map((n) => ({ ...n, file: m.file })));
  const allObs = data.modules.flatMap((m) => m.results);
  const picker = $("#explorer-picker");
  const meta = $("#explorer-meta");
  const graphSvg = $("#explorer-fig");
  const obList = $("#explorer-obs");

  const explorable = allNodes.filter((n) => GRAPHS[n.label]);
  let currentLabel = "@sum4";
  let currentDiagram = null;
  let selectedPort = null;

  const updateHighlights = () => {
    if (!currentDiagram) return;
    const spec = GRAPHS[currentLabel];
    for (const nSpec of spec.nodes) {
      const g = currentDiagram.nodes.get(nSpec.id);
      if (!g) continue;
      g.classList.toggle("on", Boolean(selectedPort && nSpec.obPort === selectedPort));
    }
    for (const row of obList.querySelectorAll(".ob-row")) {
      const p = row.dataset.port || null;
      const match = selectedPort ? p === selectedPort : !p;
      row.classList.toggle("on", Boolean(selectedPort && match));
    }
  };

  const renderNode = async (label) => {
    currentLabel = label;
    selectedPort = null;
    for (const b of picker.querySelectorAll("button")) {
      b.setAttribute("aria-pressed", String(b.dataset.node === label));
    }
    const info = explorable.find((n) => n.label === label);
    const obs = allObs.filter((o) => o.node === label);

    meta.replaceChildren(
      el("span", {}, "Fichier : ", el("b", { text: info.file })),
      el("span", {}, "Empreinte : ", el("b", { text: info.node_hash.slice(0, 14) + "…" })),
      el("span", {}, "Coût statique : ", el("b", { text: `${fmt(info.cost_ops)} ops` })),
      el("span", {}, "Obligations : ", el("b", { text: `${fmt(obs.length)} / ${fmt(obs.length)} prouvées` })),
    );

    const spec = GRAPHS[label];
    currentDiagram = await diagram(graphSvg, {
      nodes: spec.nodes,
      edges: spec.edges,
      dx: 168,
      dy: 74,
      bw: 144,
      bh: 48,
    });
    await drawIn(currentDiagram, 700);

    for (const nSpec of spec.nodes) {
      const g = currentDiagram.nodes.get(nSpec.id);
      if (!g || !nSpec.obPort) continue;
      g.style.cursor = "pointer";
      g.setAttribute("tabindex", "0");
      g.setAttribute("role", "button");
      g.setAttribute("aria-label", `Sélectionner les obligations de ${nSpec.label}`);
      const toggle = () => {
        selectedPort = selectedPort === nSpec.obPort ? null : nSpec.obPort;
        updateHighlights();
      };
      g.addEventListener("click", toggle);
      g.addEventListener("keydown", (e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          toggle();
        }
      });
    }

    obList.replaceChildren();
    for (const o of obs) {
      const btn = el("button", { type: "button", class: "ob-row", "data-port": o.port ?? "" },
        el("div", { class: "ob-row-top" },
          el("span", { class: "ob-row-id", text: o.id }),
          el("span", { class: "badge-ok", text: o.verdict })
        ),
        el("div", { class: "ob-row-sub" },
          el("span", { text: `solveur : ${o.solver === "none" ? "pré-filtrage" : o.solver}` }),
          el("span", { text: `preuve : ${o.evidence}` }),
          el("span", { text: `${fmt(o.time_ms)} ms` }),
        )
      );
      btn.addEventListener("click", () => {
        selectedPort = o.port ? (selectedPort === o.port ? null : o.port) : null;
        updateHighlights();
        if (!o.port) btn.classList.add("on");
      });
      obList.append(btn);
    }
  };

  for (const n of explorable) {
    const btn = el("button", { type: "button", "data-node": n.label, "aria-pressed": "false", text: n.label });
    btn.addEventListener("click", () => renderNode(n.label));
    picker.append(btn);
  }

  await renderNode("@sum4");
  $("#explorer-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
});

// ---- 5.3 Pipeline SMT animé -----------------------------------------------------------------------

await section("#mecanisme-fig", async () => {
  const pipeSvg = $("#fig-pipeline");
  const d = await diagram(pipeSvg, {
    nodes: [
      { id: "ob", label: "Obligation", sub: "assumptions ∧ ¬goal", kind: "input", key: 0 },
      { id: "smt", label: "SMT-LIB2", sub: "QF_AUFBVFP", kind: "op", key: 0 },
      { id: "sol", label: "z3 / Bitwuzla / cvc5", sub: "pipe + timeout", kind: "ai", key: 0 },
      { id: "unsat", label: "UNSAT", sub: "proven", kind: "ok", key: 0 },
      { id: "sat", label: "SAT", sub: "rejeu vg-interp", kind: "bad", key: 1 },
      { id: "unk", label: "UNKNOWN", sub: "jamais une preuve", kind: "warn", key: 2 },
    ],
    edges: [
      ["ob", "smt"],
      ["smt", "sol"],
      ["sol", "unsat"],
      ["sol", "sat"],
      ["sol", "unk"],
    ],
    dx: 185,
    dy: 84,
    bw: 152,
    bh: 50,
  });
  onFirstView(pipeSvg, () => drawIn(d));
  $("#pipeline-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");

  const status = $("#pipe-sim-status");
  const simulate = async (branch, cls, msg) => {
    for (const g of d.nodes.values()) g.classList.remove("on", "bad");
    for (const e of d.edges) e.el.classList.remove("on", "bad");
    status.textContent = msg;
    const seq = [["ob", "smt"], ["smt", "sol"], ["sol", branch]];
    for (const [u, v] of seq) {
      const edge = d.edges.find((e) => e.from === u && e.to === v);
      if (edge) {
        edge.el.classList.add(cls === "bad" ? "bad" : "on");
        await pulse(pipeSvg, edge, 480, cls);
      }
    }
    d.nodes.get(branch)?.classList.add(cls === "bad" ? "bad" : "on");
  };

  $("#sim-unsat").addEventListener("click", () =>
    simulate("unsat", "", "UNSAT : aucune entrée ne brise le but sous PRE → obligation prouvée."));
  $("#sim-sat").addEventListener("click", () =>
    simulate("sat", "bad", "SAT : le solveur fournit un modèle ; vg-interp doit le rejouer (ADR 0006)."));
  $("#sim-unk").addEventListener("click", () =>
    simulate("unk", "ai", "UNKNOWN / délai dépassé : aucune preuve ; le nœud reste refusé sans garde."));
});

// ---- 5.4 Terminal cache & release ------------------------------------------------------------------

const run1Re = data.cache_demo.run1_by_evidence.find((e) => e.evidence === "re-solved")?.n ?? 0;
const run2Cache = data.cache_demo.run2_by_evidence.find((e) => e.evidence === "cached")?.n ?? 0;
const relTwo = data.release.by_evidence.find((e) => e.evidence === "two-solvers")?.n ?? 0;
const cacheSummary = $("#cache-summary");
if (cacheSummary) {
  cacheSummary.textContent = `1re exécution sur examples/quickstart.vgs : ${fmt(run1Re)} obligations résolues par solveur et 5 par pré-filtrage. 2e exécution sur le même --root : ${fmt(run2Cache)} reprises du cache (cached) sans relancer z3 ni Bitwuzla. En mode --mode release : ${fmt(relTwo)} obligations prouvées par deux solveurs (two-solvers).`;
}
terminal($("#term-verify"), data.terminal_session, { title: "vg verify --mode edit · cache · release" });

// ---- 5.5 Graphiques WASM : donut.wasm et bar_chart.wasm -------------------------------------------

const KINDS = {
  POST: ["Postconditions", "promesses de sortie"],
  NONVACUOUS: ["Non-vacuité", "PRE satisfaisable"],
  FOLD: ["Invariants de boucle", "base, étape, PRE noyau"],
  ADD: ["Additions sûres", "sans débordement"],
  CAST: ["Conversions bornées", "aucun débordement"],
  GEN: ["GENERATE", "PRE pour chaque indice"],
  PRE: ["Préconditions bien définies", "aucun terme indéfini"],
  other: ["Autres espèces", "DIV, MOD, SHL, SHR, INDEX…"],
};
const PALETTE = ["var(--proven)", "var(--blue)", "var(--violet)", "var(--amber)", "var(--teal)", "var(--pink)", "var(--sky)", "var(--muted)"];

await section("#chiffres-fig", async () => {
  const [donutWasm, barWasm] = await Promise.all([load("wasm/donut.wasm"), load("wasm/bar_chart.wasm")]);

  const kinds = data.aggregates.kinds8;
  const values8 = kinds.map((k) => k.n).concat(Array(8 - kinds.length).fill(0));
  const ring = $("#donut-svg");
  const paths = kinds.map((k, s) => {
    const p = svg("path", { fill: PALETTE[s] });
    const title = svg("title");
    title.textContent = `${KINDS[k.kind]?.[0] ?? k.kind} : ${k.n}`;
    p.append(title);
    ring.append(p);
    return p;
  });

  const drawRing = async (sweep) => {
    const xy = f32s(await donutWasm(encode([["f32", values8], ["f32", sweep]])), 0, 1056);
    paths.forEach((p, seg) => {
      const pts = Array.from({ length: 66 }, (_, i) => `${xy[seg * 132 + 2 * i].toFixed(4)},${xy[seg * 132 + 2 * i + 1].toFixed(4)}`);
      p.setAttribute("d", `M${pts.join("L")}Z`);
    });
  };
  await drawRing(1);
  $("#donut-total").textContent = fmt(data.aggregates.total);

  const legend = $("#donut-legend");
  kinds.forEach((k, s) => {
    const [name, note] = KINDS[k.kind] ?? [k.kind, ""];
    const li = el("li", { tabindex: "0" },
      el("i", { style: `--c:${PALETTE[s]}` }),
      el("span", {}, name, el("small", { text: `${k.kind} · ${note}` })),
      el("b", { text: fmt(k.n) })
    );
    const focus = (on) => paths.forEach((p, i) => p.classList.toggle("dim", on && i !== s));
    li.addEventListener("pointerenter", () => focus(true));
    li.addEventListener("pointerleave", () => focus(false));
    li.addEventListener("focus", () => focus(true));
    li.addEventListener("blur", () => focus(false));
    legend.append(li);
  });
  onFirstView(ring, () => play(CURVE.OUT_QUART, 1300, (e) => drawRing(e)));
  $("#donut-proof").innerHTML = await proofTag("@donut", "∀ coordonnée : |x| ≤ 1");

  const solvers = data.aggregates.by_solver;
  const barOut = await barWasm(encode([["f32", solvers.map((s) => s.n).concat(Array(12 - solvers.length).fill(0))]]));
  const frac = f32s(barOut, 0, 12);
  const solverBox = $("#solver-bars");
  const fills = solvers.map((s, i) => {
    const fill = el("div", { class: "fill", style: `--f:${frac[i]};--c:${PALETTE[i]}` });
    solverBox.append(el("div", { class: "hbar" },
      el("span", { text: s.solver === "none" ? "pré-filtrage" : s.solver }),
      el("div", { class: "rail" }, fill),
      el("b", { text: fmt(s.n) })
    ));
    return fill;
  });
  onFirstView(solverBox, () => play(CURVE.OUT_BACK, 1100, (e) => fills.forEach((f) => f.style.setProperty("--e", e))));
  $("#solver-proof").innerHTML = await proofTag("@bar_chart", "∀ barre : 0 ≤ longueur ≤ 1");
});
