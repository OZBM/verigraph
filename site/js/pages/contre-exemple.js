import {
  $, el, svg, fmt, getJSON, load, encode, f32s,
  section, proofTag, terminal, scrolly, drawIn, pulse, onFirstView,
} from "../page.js";
import { diagram } from "../diagram.js";

const data = await getJSON("data/contre-exemple.json");

// Renseigne les empreintes et compteurs tirés de data/contre-exemple.json.
const setText = (sel, text) => {
  const n = $(sel);
  if (n) n.textContent = text;
};
setText("#hash-draft", data.hashes.draft.node.slice(0, 16) + "…");
setText("#hash-repaired", data.hashes.quickstart.node.slice(0, 16) + "…");
setText("#hash-contract", data.hashes.draft.contract.slice(0, 16) + "…");
setText("#repaired-proven", `${fmt(data.repaired_verify.proven)} / ${fmt(data.repaired_verify.total)}`);
const draftFailed = data.draft_verify.results.filter((r) => r.verdict !== "proven").length;
const draftTotal = data.draft_verify.results.length;
setText("#draft-failed-count", `${fmt(draftFailed)} sur ${fmt(draftTotal)}`);

// ---- 6.3 Scrollytelling sur le graphe de @soft_knee ----------------------------------------------

await section("#histoire", async () => {
  const storySvg = $("#story-fig");
  const readout = $("#story-readout");
  const spec = {
    nodes: [
      { id: "x", label: "x: f32", sub: "entrée", kind: "input", key: 0 },
      { id: "knee", label: "knee: f32", sub: "[0.5, 0.99]", kind: "input", key: 1 },
      { id: "p0", label: "%0 FABS(x)", sub: "|x|", kind: "op", key: 0 },
      { id: "p1", label: "%1 FSUB", sub: "%0 - knee", kind: "op", key: 0 },
      { id: "p3", label: "%3 FSUB", sub: "1 - knee", kind: "op", key: 1 },
      { id: "p10", label: "%10 LE.f", sub: "%0 <= knee", kind: "op", key: 2 },
      { id: "p2", label: "%2 FMAX", sub: "max(%1, 0)", kind: "op", key: 0 },
      { id: "p4", label: "%4 FDIV", sub: "t = %2 / %3", kind: "warn", key: 0 },
      { id: "p5", label: "%5 FADD", sub: "1 + t", kind: "op", key: 0 },
      { id: "p6", label: "%6 FDIV", sub: "t / (1+t)", kind: "warn", key: 0 },
      { id: "p7", label: "%7 FMIN", sub: "min(%6, 1)", kind: "warn", key: 0 },
      { id: "p8", label: "%8 FMA", sub: "%3*%7+knee", kind: "op", key: 0 },
      { id: "p9", label: "%9 COPYSIGN", sub: "signe de x", kind: "op", key: 0 },
      { id: "p11", label: "%11 SELECT", sub: "%10 ? x : %9", kind: "op", key: 0 },
      { id: "p12", label: "%12 FCLAMP", sub: "[-1, 1]", kind: "warn", key: 0 },
      { id: "ret", label: "RETURN", sub: "result", kind: "out", key: 0 },
    ],
    edges: [
      ["x", "p0"], ["p0", "p1"], ["knee", "p1"], ["knee", "p3"], ["p0", "p10"], ["knee", "p10"],
      ["p1", "p2"], ["p2", "p4"], ["p3", "p4"], ["p4", "p5"], ["p4", "p6"], ["p5", "p6"],
      ["p6", "p7"], ["p3", "p8"], ["p7", "p8"], ["knee", "p8"], ["p8", "p9"], ["x", "p9"],
      ["p10", "p11"], ["x", "p11"], ["p9", "p11"], ["p11", "p12"], ["p12", "ret"],
    ],
    vertical: true,
    dx: 126,
    dy: 56,
    bw: 114,
    bh: 40,
  };

  const d = await diagram(storySvg, spec);
  onFirstView(storySvg, () => drawIn(d));
  $("#story-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");

  const setBoxText = (id, label, sub) => {
    const g = d.nodes.get(id);
    if (!g) return;
    const l = $(".dg-label", g);
    const s = $(".dg-sub", g);
    if (l) l.textContent = label;
    if (s) s.textContent = sub;
  };

  const resetStyles = () => {
    for (const g of d.nodes.values()) g.classList.remove("on", "bad", "dim");
    for (const e of d.edges) e.el.classList.remove("on", "bad", "dim");
  };

  const rep = data.explain.report;
  const sliceMap = Object.fromEntries(rep.slice.map((s) => [s.port, s.value]));
  const sliceEdgePairs = new Set([
    "x->p0", "p0->p1", "p1->p2", "p2->p4", "p4->p5", "p4->p6", "p5->p6", "p6->p7",
    "p7->p8", "p8->p9", "p9->p11", "p11->p12", "p12->ret",
  ]);
  const sliceNodeIds = ["x", "p0", "p4", "p5", "p6", "p12", "ret"];
  const coneNodeIds = ["x", "knee", "p0", "p1", "p2", "p3", "p4", "p5", "p6", "p7", "p8", "p9", "p11", "p12", "ret"];

  const renderReadout = (step) => {
    readout.replaceChildren();
    if (step <= 1) {
      readout.append(
        el("div", { class: "slice-strip" },
          el("span", { class: "slice-chip" }, "Contrat POST.0 : ", el("b", { text: rep.clause })),
          el("span", { class: "slice-chip bad" }, "Verdict : ", el("b", { text: `${rep.verdict} (${rep.code})` })),
        )
      );
    } else if (step === 2 || step === 3) {
      readout.append(
        el("div", { class: "slice-strip" },
          el("span", { class: "slice-chip bad" }, "x = ", el("b", { text: rep.inputs.x })),
          el("span", { class: "slice-chip" }, "knee = ", el("b", { text: rep.inputs.knee })),
          el("span", { class: "slice-chip bad" }, "vg-interp → ", el("b", { text: `${data.runs[3].draft.outputs[0]} (reproduced: ${rep.reproduced})` })),
        )
      );
    } else if (step === 4) {
      const strip = el("div", { class: "slice-strip" });
      for (const entry of rep.slice) {
        strip.append(el("span", { class: "slice-chip bad" }, `${entry.port} ${entry.op.split("(")[0]} → `, el("b", { text: String(entry.value) })));
      }
      readout.append(strip);
    } else if (step === 5) {
      readout.append(el("div", { class: "diff-banner" }, `Condition diff : ${rep.diff}`));
    } else {
      readout.append(
        el("div", { class: "slice-strip" },
          el("span", { class: "slice-chip ok" }, "%6 = ", el("b", { text: "FDIV(1, %5) → 0" })),
          el("span", { class: "slice-chip ok" }, "%7 = ", el("b", { text: "FSUB(1, %6) → 1" })),
          el("span", { class: "slice-chip ok" }, "POST.0..2 : ", el("b", { text: `${data.repaired_verify.proven} / ${data.repaired_verify.total} prouvées` })),
        ),
        el("div", { class: "diff-banner ok" }, `x = ${rep.inputs.x}, knee = ${rep.inputs.knee} → vg-interp : ${data.runs[3].repaired.outputs[0]} (POST = true)`)
      );
    }
  };

  let pulseRun = 0;
  const playSlicePulses = async () => {
    const me = ++pulseRun;
    const seq = [["x", "p0"], ["p2", "p4"], ["p4", "p5"], ["p5", "p6"], ["p6", "p7"], ["p11", "p12"], ["p12", "ret"]];
    for (const [u, v] of seq) {
      if (me !== pulseRun) return;
      const edge = d.edges.find((e) => e.from === u && e.to === v);
      if (edge) await pulse(storySvg, edge, 420, "bad");
    }
  };

  const applyStep = (step) => {
    resetStyles();
    if (step >= 6) {
      setBoxText("p6", "%6 FDIV", "1 / (1 + t)");
      setBoxText("p7", "%7 FSUB", "1 - %6");
    } else {
      setBoxText("p6", "%6 FDIV", "t / (1+t)");
      setBoxText("p7", "%7 FMIN", "min(%6, 1)");
    }

    if (step === 0) {
      d.nodes.get("x")?.classList.add("on");
      d.nodes.get("knee")?.classList.add("on");
      d.nodes.get("ret")?.classList.add("on");
    } else if (step === 1) {
      d.nodes.get("p6")?.classList.add("bad");
      d.nodes.get("p12")?.classList.add("bad");
      d.nodes.get("ret")?.classList.add("bad");
    } else if (step === 2 || step === 3) {
      for (const [id, g] of d.nodes) {
        g.classList.add(coneNodeIds.includes(id) ? (["p4", "p5", "p6", "p12", "ret"].includes(id) ? "bad" : "on") : "dim");
      }
      for (const e of d.edges) {
        if (sliceEdgePairs.has(`${e.from}->${e.to}`)) e.el.classList.add("bad");
      }
    } else if (step === 4) {
      for (const [id, g] of d.nodes) {
        if (sliceNodeIds.includes(id)) g.classList.add("bad");
        else g.classList.add("dim");
      }
      for (const e of d.edges) {
        if (sliceEdgePairs.has(`${e.from}->${e.to}`)) e.el.classList.add("bad");
        else e.el.classList.add("dim");
      }
      if (sliceMap["%0"]) setBoxText("p0", "%0 FABS(x)", sliceMap["%0"]);
      if (sliceMap["%4"]) setBoxText("p4", "%4 FDIV", sliceMap["%4"]);
      if (sliceMap["%5"]) setBoxText("p5", "%5 FADD", sliceMap["%5"]);
      if (sliceMap["%6"]) setBoxText("p6", "%6 FDIV", sliceMap["%6"]);
      if (sliceMap["%12"]) setBoxText("p12", "%12 FCLAMP", sliceMap["%12"]);
      playSlicePulses();
    } else if (step === 5) {
      for (const [id, g] of d.nodes) {
        if (id === "p4") g.classList.add("bad");
        else if (!["x", "p0", "p5", "p6", "p12", "ret"].includes(id)) g.classList.add("dim");
      }
      d.nodes.get("p6")?.classList.add("bad");
      d.nodes.get("ret")?.classList.add("bad");
    } else if (step === 6) {
      d.nodes.get("p6")?.classList.add("on");
      d.nodes.get("p7")?.classList.add("on");
      d.nodes.get("p4")?.classList.add("on");
      d.nodes.get("p5")?.classList.add("on");
    } else {
      for (const g of d.nodes.values()) g.classList.add("on");
      for (const e of d.edges) e.el.classList.add("on");
    }
    if (step !== 4) {
      setBoxText("p0", "%0 FABS(x)", "|x|");
      setBoxText("p4", "%4 FDIV", "t = %2 / %3");
      setBoxText("p5", "%5 FADD", "1 + t");
      setBoxText("p12", "%12 FCLAMP", "[-1, 1]");
    }
    renderReadout(step);
  };

  scrolly($("#story"), applyStep);
  applyStep(0);
});

// ---- 6.4 Labo en direct : knee.wasm et knee_draft.wasm -------------------------------------------

await section("#knee-lab", async () => {
  const [gridWasm, kneeWasm, draftWasm] = await Promise.all([
    load("wasm/grid.wasm"),
    load("wasm/knee.wasm"),
    load("wasm/knee_draft.wasm"),
  ]);

  const xsBytes = await gridWasm(encode([["f32", [-3.0, 3.0]]]));
  const xs = f32s(xsBytes, 0, 256);

  const plotSvg = $("#knee-svg");
  const slider = $("#knee-slider");
  const kneeVal = $("#knee-val");
  const xInput = $("#x-input");
  const outOk = $("#eval-ok");
  const outDraft = $("#eval-draft");
  const outRefusal = $("#eval-refusal");

  // Prépare la structure SVG du graphique [-3, 3] x [-1.25, 1.25].
  const gPlot = svg("g", { transform: "scale(1,-1)" });
  gPlot.append(
    svg("line", { class: "knee-axis", x1: -3.1, y1: 0, x2: 3.1, y2: 0 }),
    svg("line", { class: "knee-axis", x1: 0, y1: -1.2, x2: 0, y2: 1.2 }),
    svg("line", { class: "knee-bound", x1: -3.1, y1: 1, x2: 3.1, y2: 1 }),
    svg("line", { class: "knee-bound", x1: -3.1, y1: -1, x2: 3.1, y2: -1 })
  );
  const pathOk = svg("polyline", { class: "knee-curve-ok", points: "" });
  const pathDraft = svg("polyline", { class: "knee-curve-draft", points: "" });
  gPlot.append(pathDraft, pathOk);
  // Étiquettes lisibles hors miroir Y.
  const lbl = (x, y, text) => {
    const t = svg("text", { class: "knee-tick", x, y, "text-anchor": "middle" });
    t.textContent = text;
    return t;
  };
  plotSvg.replaceChildren(
    gPlot,
    lbl(-2.85, -0.96, "+1.0"),
    lbl(-2.85, 1.08, "-1.0"),
    lbl(-2.95, -0.06, "-3"),
    lbl(2.95, -0.06, "+3"),
  );

  const parseScalar = (raw) => {
    const s = raw.trim();
    if (/^[+-]?nan$/i.test(s)) return Number.NaN;
    if (/^\+?inf(inity)?$/i.test(s)) return Number.POSITIVE_INFINITY;
    if (/^-inf(inity)?$/i.test(s)) return Number.NEGATIVE_INFINITY;
    const n = Number(s);
    if (s === "" || Number.isNaN(n)) return null;
    return n;
  };

  const evaluateSingle = async () => {
    const k = Number(slider.value);
    const raw = xInput.value;
    const x = parseScalar(raw);
    if (x === null) {
      outOk.textContent = "saisie invalide";
      outDraft.textContent = "—";
      outRefusal.hidden = true;
      return;
    }
    const payload = encode([["f32", Array(256).fill(x)], ["f32", k]]);
    outRefusal.hidden = true;
    try {
      const yOk = f32s(await kneeWasm(payload), 0, 1)[0];
      outOk.textContent = String(yOk);
      outOk.className = "ok";
    } catch (e) {
      outOk.textContent = "refusé (PRE)";
      outOk.className = "miss";
      outRefusal.textContent = `Entrée vérifiée (ADR 0009) : le binaire a refusé l'entrée hors PRE (${e.message.trim().replace(/:$/, "")}).`;
      outRefusal.hidden = false;
    }
    try {
      const yDr = f32s(await draftWasm(payload), 0, 1)[0];
      outDraft.textContent = Number.isNaN(yDr) ? "NaN" : String(yDr);
      outDraft.className = Number.isNaN(yDr) ? "miss" : "";
    } catch (e) {
      outDraft.textContent = "refusé (PRE)";
      outDraft.className = "miss";
    }
  };

  const updatePlot = async () => {
    const k = Number(slider.value);
    kneeVal.textContent = k.toFixed(2);
    const payload = encode([["f32", xs], ["f32", k]]);
    const [bOk, bDr] = await Promise.all([kneeWasm(payload), draftWasm(payload)]);
    const ysOk = f32s(bOk, 0, 256);
    const ysDr = f32s(bDr, 0, 256);
    pathOk.setAttribute("points", xs.map((x, i) => `${x.toFixed(4)},${ysOk[i].toFixed(4)}`).join(" "));
    pathDraft.setAttribute(
      "points",
      xs.map((x, i) => (Number.isFinite(ysDr[i]) ? `${x.toFixed(4)},${ysDr[i].toFixed(4)}` : "")).filter(Boolean).join(" ")
    );
    await evaluateSingle();
  };

  slider.addEventListener("input", updatePlot);
  xInput.addEventListener("input", evaluateSingle);
  for (const btn of document.querySelectorAll("[data-preset-x]")) {
    btn.addEventListener("click", () => {
      xInput.value = btn.dataset.presetX;
      evaluateSingle();
    });
  }

  await updatePlot();
  $("#knee-proof").innerHTML = await proofTag("@knee_curve", "∀ x ∈ [-3, 3] : résultat dans [-1, 1], signe conservé");
});

// ---- Table des exécutions vg run + terminal -------------------------------------------------------

const tbody = $("#runs-tbody");
if (tbody) {
  for (const r of data.runs) {
    const dr = r.draft.outputs[0];
    const rp = r.repaired.outputs[0];
    tbody.append(
      el("tr", {},
        el("td", { text: r.label }),
        el("td", {}, el("code", { text: r.inputs.x })),
        el("td", { class: r.draft.post ? "" : "miss" }, el("code", { text: `${dr} (post: ${r.draft.post})` })),
        el("td", { class: "ok" }, el("code", { text: `${rp} (post: ${r.repaired.post})` }))
      )
    );
  }
}

terminal($("#term-explain"), data.terminal_session, { title: "vg explain · vg run" });

// ---- 6.5 Schéma vg-telemetry ----------------------------------------------------------------------

await section("#telemetrie", async () => {
  const telSvg = $("#fig-telemetry");
  const d = await diagram(telSvg, {
    nodes: [
      { id: "trace", label: "Trace d'échec", sub: "vg-interp", kind: "bad", key: 0 },
      { id: "cone", label: "Cône arrière", sub: "dépendances", kind: "op", key: 0 },
      { id: "pass", label: "Entrée valide", sub: "z3 (20M ops)", kind: "op", key: 1 },
      { id: "ddmin", label: "ddmin", sub: "Zeller", kind: "ai", key: 0 },
      { id: "collapse", label: "Passe-plats", sub: "si > 5 ops", kind: "op", key: 0 },
      { id: "out", label: "Tranche + diff", sub: "≤ 5 ops", kind: "ok", key: 0 },
    ],
    edges: [
      ["trace", "cone"],
      ["cone", "ddmin"],
      ["pass", "ddmin"],
      ["ddmin", "collapse"],
      ["collapse", "out"],
    ],
    dx: 175,
    dy: 84,
    bw: 146,
    bh: 50,
  });
  onFirstView(telSvg, () => drawIn(d));
  $("#telemetry-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
});
