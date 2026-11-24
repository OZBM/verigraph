import {
  $,
  $$,
  el,
  svg,
  fmt,
  getJSON,
  section,
  proofTag,
  terminal,
  onFirstView,
  drawIn,
  pulse,
} from "../page.js";
import { diagram } from "../diagram.js";

function renderGapTable(host, rows, isDraft) {
  const table = el("table");
  const head = el(
    "thead",
    {},
    el(
      "tr",
      {},
      el("th", { text: "Entrée x" }),
      el("th", { text: "knee" }),
      el("th", { text: "PRE" }),
      el("th", { text: "Sortie" }),
      el("th", { text: "POST" }),
      el("th", { text: "Source" })
    )
  );
  const body = el("tbody");
  for (const r of rows) {
    const tr = el(
      "tr",
      {},
      el("td", {}, el("code", { text: r.x })),
      el("td", {}, el("code", { text: r.knee })),
      el("td", { text: r.pre ? "true" : "false" }),
      el("td", {}, el("code", { text: r.output })),
      el("td", { class: r.post ? "ok" : "miss", text: r.post ? "true" : "false" }),
      el("td", { text: isDraft ? "vg run (draft)" : "vg run (réparé)" })
    );
    body.append(tr);
  }
  table.append(head, body);
  host.replaceChildren(table);
}

section("#fig-gap", async () => {
  const data = await getJSON("data/idee.json");
  const fig = $("#fig-gap");
  const buttons = $$(".gap-modes button", fig);
  const cov = $("#gap-coverage", fig);
  const summary = $("#gap-summary", fig);
  const tableHost = $("#gap-table", fig);

  const setMode = (mode) => {
    buttons.forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.mode === mode)));
    cov.className = "gap-coverage";
    if (mode === "tests") {
      cov.classList.add("is-partial");
      summary.textContent = `${fmt(data.draft_tests.length)} tests ordinaires autour de 0 : tous renvoient post = true, mais ils ne couvrent qu'une minuscule fraction des flottants f32.`;
      renderGapTable(tableHost, data.draft_tests, true);
    } else if (mode === "verify") {
      cov.classList.add("is-broken");
      const ce = data.counterexample;
      const run = data.draft_counterexample_run;
      summary.textContent = `${ce.id} réfuté (${ce.code}, evidence: ${ce.evidence}, reproduced: ${ce.reproduced}) avec x = ${ce.inputs.x}, knee = ${ce.inputs.knee} -> sortie ${run.output}, post = ${run.post}.`;
      renderGapTable(tableHost, [run], true);
    } else {
      cov.classList.add("is-proven");
      const rep = data.repaired_obligations;
      const run = data.repaired_counterexample_run;
      summary.textContent = `Version réparée (examples/quickstart.vgs) : ${fmt(rep.proven)} obligations sur ${fmt(rep.total)} prouvées. Sur le même contre-exemple x = ${run.x}, la sortie vaut ${run.output} (post = ${run.post}).`;
      renderGapTable(tableHost, [run], false);
    }
  };

  for (const b of buttons) b.addEventListener("click", () => setMode(b.dataset.mode));
  setMode("tests");

  $("#fig-gap-proof").innerHTML = "mesuré par <code>vg run</code> et <code>vg verify</code> (<code>site/data/idee.json</code>)";
  terminal($("#term-gap"), data.terminal_steps, { title: "vg run & vg verify · @soft_knee" });
});

section("#fig-pipe", async () => {
  const svgEl = $("#fig-pipe");
  const d = await diagram(svgEl, {
    nodes: [
      { id: "req", label: "Demande", sub: "langage naturel", kind: "input" },
      { id: "ai", label: "Modèle IA", sub: "rédige le graphe", kind: "ai" },
      { id: "vgs", label: "VG-Sym", sub: "projection texte", kind: "op" },
      { id: "check", label: "vg check", sub: "structure · types", kind: "op" },
      { id: "verify", label: "vg verify", sub: "solveurs SMT", kind: "warn" },
      { id: "build", label: "vg build", sub: "LLVM IR + vg-rt", kind: "ok" },
      { id: "x86", label: "x86_64", sub: "Linux ELF", kind: "out" },
      { id: "arm", label: "ARM64", sub: "macOS Mach-O", kind: "out" },
      { id: "wasm", label: "WASM", sub: "wasm32-wasip1", kind: "out" },
    ],
    edges: [
      ["req", "ai"],
      ["ai", "vgs"],
      ["vgs", "check"],
      ["check", "verify"],
      ["verify", "build"],
      ["build", "x86"],
      ["build", "arm"],
      ["build", "wasm"],
    ],
    back: [["verify", "ai"]],
    dx: 184,
    dy: 78,
    bw: 152,
    bh: 52,
  });

  const edgeOf = (from, to) => d.edges.find((e) => e.from === from && e.to === to);
  const reset = () => {
    for (const n of d.nodes.values()) n.classList.remove("on", "bad", "dim");
    for (const e of d.edges) e.el.classList.remove("on", "bad", "dim");
  };

  let running = false;
  const playRepair = async () => {
    if (running) return;
    running = true;
    reset();
    for (const id of ["req", "ai", "vgs", "check", "verify"]) {
      const prev = id === "req" ? null : edgeOf(id === "ai" ? "req" : id === "vgs" ? "ai" : id === "check" ? "vgs" : "check", id);
      if (prev) await pulse(svgEl, prev, 420, id === "ai" ? "ai" : "");
      d.nodes.get(id).classList.add(id === "verify" ? "bad" : "on");
    }
    const back = edgeOf("verify", "ai");
    back.el.classList.add("bad");
    await pulse(svgEl, back, 850, "bad");
    d.nodes.get("verify").classList.remove("bad");
    back.el.classList.remove("bad");
    for (const [a, b, cls] of [
      ["ai", "vgs", "ai"],
      ["vgs", "check", ""],
      ["check", "verify", ""],
      ["verify", "build", ""],
    ]) {
      const ed = edgeOf(a, b);
      ed.el.classList.add("on");
      await pulse(svgEl, ed, 440, cls);
      d.nodes.get(b).classList.add("on");
    }
    await Promise.all([
      pulse(svgEl, edgeOf("build", "x86"), 520, ""),
      pulse(svgEl, edgeOf("build", "arm"), 520, ""),
      pulse(svgEl, edgeOf("build", "wasm"), 520, ""),
    ]);
    for (const t of ["x86", "arm", "wasm"]) d.nodes.get(t).classList.add("on");
    running = false;
  };

  const playPass = async () => {
    if (running) return;
    running = true;
    reset();
    for (const [a, b, cls] of [
      ["req", "ai", "ai"],
      ["ai", "vgs", "ai"],
      ["vgs", "check", ""],
      ["check", "verify", ""],
      ["verify", "build", ""],
    ]) {
      d.nodes.get(a).classList.add("on");
      const ed = edgeOf(a, b);
      ed.el.classList.add("on");
      await pulse(svgEl, ed, 420, cls);
      d.nodes.get(b).classList.add("on");
    }
    await Promise.all([
      pulse(svgEl, edgeOf("build", "x86"), 520, ""),
      pulse(svgEl, edgeOf("build", "arm"), 520, ""),
      pulse(svgEl, edgeOf("build", "wasm"), 520, ""),
    ]);
    for (const t of ["x86", "arm", "wasm"]) d.nodes.get(t).classList.add("on");
    running = false;
  };

  $("#pipe-repair").addEventListener("click", playRepair);
  $("#pipe-pass").addEventListener("click", playPass);
  onFirstView(svgEl, async () => {
    await drawIn(d);
    playRepair();
  });
  $("#fig-pipe-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
});

section("#fig-arch", async () => {
  const svgEl = $("#fig-arch");
  const links = {
    core: ["graphe.html", "Chapitre 2 · Un programme est un graphe"],
    store: ["graphe.html", "Chapitre 2 · Dépôt vg-store"],
    decode: ["vg-sym.html", "Chapitre 3 · Décodage contraint"],
    types: ["types.html", "Chapitre 4 · Types, effets et capacités"],
    smt: ["preuves.html", "Chapitre 5 · Contrats et preuves"],
    interp: ["contre-exemple.html", "Chapitre 6 · Interpréteur de référence"],
    telemetry: ["contre-exemple.html", "Chapitre 6 · Télémétrie causale"],
    codegen: ["compilation.html", "Chapitre 7 · Compilation LLVM IR"],
    rt: ["compilation.html", "Chapitre 7 · Runtime C vg-rt"],
    cli: ["agent.html", "Chapitre 8 · Outil vg"],
    mcp: ["agent.html", "Chapitre 8 · Serveur vg mcp"],
    ide: ["agent.html", "Chapitre 8 · Extension Antigravity"],
    mesh: ["mesh.html", "Chapitre 9 · Registre VG-Mesh"],
    lift: ["omnilift.html", "Chapitre 10 · OmniLift"],
  };

  const d = await diagram(svgEl, {
    nodes: [
      { id: "core", label: "vg-core", sub: "IR · codec · BLAKE3", kind: "ok" },
      { id: "store", label: "vg-store", sub: "objets · vg.lock", kind: "op" },
      { id: "types", label: "vg-types", sub: "types · effets · coût", kind: "op" },
      { id: "decode", label: "vg-decode", sub: "grammaires · Prefix", kind: "ai" },
      { id: "interp", label: "vg-interp", sub: "oracle exécutable", kind: "op" },
      { id: "smt", label: "vg-smt", sub: "z3 · cvc5 · Bitwuzla", kind: "warn" },
      { id: "telemetry", label: "vg-telemetry", sub: "tranches d'échec", kind: "warn" },
      { id: "codegen", label: "vg-codegen", sub: "LLVM IR · bacs à sable", kind: "ok" },
      { id: "rt", label: "vg-rt", sub: "runtime C", kind: "op" },
      { id: "mesh", label: "vg-mesh", sub: "registre par contrat", kind: "op" },
      { id: "lift", label: "vg-omnilift", sub: "C / Rust -> VG-IR", kind: "op" },
      { id: "cli", label: "vg-cli", sub: "commande vg", kind: "out" },
      { id: "mcp", label: "vg mcp", sub: "8 outils MCP stdio", kind: "ai" },
      { id: "ide", label: "Antigravity", sub: "extension · plugin", kind: "ai" },
    ],
    edges: [
      ["core", "store"],
      ["core", "types"],
      ["core", "decode"],
      ["types", "interp"],
      ["types", "smt"],
      ["types", "codegen"],
      ["interp", "smt"],
      ["interp", "telemetry"],
      ["smt", "telemetry"],
      ["smt", "codegen"],
      ["store", "mesh"],
      ["smt", "mesh"],
      ["smt", "lift"],
      ["codegen", "lift"],
      ["rt", "codegen"],
      ["codegen", "cli"],
      ["decode", "cli"],
      ["telemetry", "cli"],
      ["mesh", "cli"],
      ["lift", "cli"],
      ["cli", "mcp"],
      ["mcp", "ide"],
    ],
    dx: 176,
    dy: 76,
    bw: 150,
    bh: 50,
  });

  for (const [id, nodeEl] of d.nodes.entries()) {
    const [href, title] = links[id];
    const a = svg("a", { href, "aria-label": `${id} — ${title}` });
    nodeEl.replaceWith(a);
    a.append(nodeEl);
  }

  onFirstView(svgEl, () => drawIn(d));
  $("#fig-arch-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
});
