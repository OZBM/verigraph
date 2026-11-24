import {
  $,
  el,
  fmt,
  getJSON,
  section,
  proofTag,
  terminal,
  highlight,
  onFirstView,
  drawIn,
  pulse,
} from "../page.js";
import { diagram } from "../diagram.js";

section("#fig-softknee", async () => {
  const data = await getJSON("data/graphe.json");
  const svgEl = $("#fig-softknee");

  const nodes = [
    { id: "x", label: "x", sub: "entrée f32", kind: "input", min: 0 },
    { id: "knee", label: "knee", sub: "entrée f32", kind: "input", min: 0 },
    ...data.soft_knee_ops.map((o) => ({
      id: o.port,
      label: `${o.port} · ${o.op}`,
      sub: o.operands.join(", "),
      kind: o.port === "%12" ? "ok" : "op",
    })),
    { id: "ret", label: "RETURN(%12)", sub: "sortie f32", kind: "out" },
  ];

  const edgeKeys = new Set();
  const edges = [];
  const addEdge = (from, to) => {
    const k = `${from}->${to}`;
    if (edgeKeys.has(k)) return;
    edgeKeys.add(k);
    edges.push([from, to]);
  };

  for (const o of data.soft_knee_ops) {
    for (const arg of o.operands) {
      if (arg === "x" || arg === "knee" || arg.startsWith("%")) {
        addEdge(arg, o.port);
      }
    }
  }
  addEdge("%12", "ret");

  const d = await diagram(svgEl, {
    nodes,
    edges,
    dx: 184,
    dy: 68,
    bw: 154,
    bh: 48,
  });

  let busy = false;
  const playFlow = async () => {
    if (busy) return;
    busy = true;
    for (const n of d.nodes.values()) n.classList.remove("on");
    for (const e of d.edges) e.el.classList.remove("on");

    d.nodes.get("x").classList.add("on");
    d.nodes.get("knee").classList.add("on");
    const order = [...data.soft_knee_ops.map((o) => o.port), "ret"];
    for (const target of order) {
      const incoming = d.edges.filter((e) => e.to === target);
      await Promise.all(
        incoming.map((e) => {
          e.el.classList.add("on");
          return pulse(svgEl, e, 300, "");
        })
      );
      d.nodes.get(target).classList.add("on");
    }
    busy = false;
  };

  $("#sk-flow").addEventListener("click", playFlow);
  onFirstView(svgEl, async () => {
    await drawIn(d);
    playFlow();
  });
  $("#fig-sk-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
});

section("#fig-variants", async () => {
  const data = await getJSON("data/graphe.json");
  const bar = $("#fig-variants .variant-bar");
  const summary = $("#var-summary");
  const badge = $("#var-badge");
  const pre = $("#var-code");
  const lbl = $("#var-label");
  const nHash = $("#var-node");
  const bHash = $("#var-body");
  const cHash = $("#var-contract");

  const buttons = data.variants.map((v) => {
    const b = el("button", { class: "btn small", type: "button", text: v.title, "data-id": v.id });
    bar.append(b);
    return b;
  });

  const select = (id) => {
    const v = data.variants.find((x) => x.id === id) ?? data.variants[0];
    buttons.forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.id === v.id)));
    summary.textContent = `${v.summary} (${v.file})`;
    badge.className = v.same_as_ref ? "chip" : "chip coral";
    badge.textContent = v.same_as_ref ? "même empreinte" : "empreinte différente";
    pre.setAttribute("data-file", v.file);
    pre.replaceChildren(el("code", { text: v.code }));
    highlight(pre);
    lbl.textContent = v.label;
    nHash.textContent = v.node;
    bHash.textContent = v.body;
    cHash.textContent = v.contract;
  };

  buttons.forEach((b) => b.addEventListener("click", () => select(b.dataset.id)));
  select(data.variants[0].id);
});

section("#fig-mandelbrot", async () => {
  const data = await getJSON("data/graphe.json");
  const svgEl = $("#fig-mandelbrot");
  const byLabel = Object.fromEntries(data.mandelbrot_hashes.map((m) => [m.label, m]));

  const kindOf = {
    "@tick": "input",
    "@escape_step": "op",
    "@palette_entry": "op",
    "@pixel": "ok",
    "@render": "out",
  };
  const d = await diagram(svgEl, {
    nodes: ["@tick", "@escape_step", "@palette_entry", "@pixel", "@render"].map((lbl) => ({
      id: lbl,
      label: lbl,
      sub: `#${byLabel[lbl].short12}`,
      kind: kindOf[lbl],
    })),
    edges: [
      ["@tick", "@pixel"],
      ["@escape_step", "@pixel"],
      ["@palette_entry", "@render"],
      ["@pixel", "@render"],
    ],
    dx: 215,
    dy: 90,
    bw: 164,
    bh: 54,
  });

  const callers = {
    "@tick": ["@pixel", "@render"],
    "@escape_step": ["@pixel", "@render"],
    "@palette_entry": ["@render"],
    "@pixel": ["@render"],
    "@render": [],
  };

  const status = $("#mandel-status");
  const buttons = [...document.querySelectorAll("[data-mandel]")];

  const trigger = async (id) => {
    const affected = new Set([id, ...callers[id]]);
    for (const [nid, n] of d.nodes.entries()) {
      n.classList.remove("bad", "on", "dim");
      if (affected.has(nid)) n.classList.add("bad");
      else n.classList.add("dim");
    }
    const activeEdges = d.edges.filter((e) => affected.has(e.from) && affected.has(e.to));
    for (const e of d.edges) {
      e.el.classList.remove("bad", "on", "dim");
      if (activeEdges.includes(e)) e.el.classList.add("bad");
      else e.el.classList.add("dim");
    }
    const others = callers[id];
    status.textContent = others.length
      ? `Modifier ${id} (#${byLabel[id].short12}) invalide aussi ${others.join(" et ")}.`
      : `Modifier ${id} (#${byLabel[id].short12}) ne touche aucun appelant : c'est la racine.`;
    await Promise.all(activeEdges.map((e) => pulse(svgEl, e, 650, "bad")));
  };

  for (const [nid, nodeEl] of d.nodes.entries()) {
    nodeEl.style.cursor = "pointer";
    nodeEl.addEventListener("click", () => trigger(nid));
  }
  buttons.forEach((b) => b.addEventListener("click", () => trigger(b.dataset.mandel)));

  onFirstView(svgEl, () => drawIn(d));
  $("#fig-mandel-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
});

section("#term-store", async () => {
  const data = await getJSON("data/graphe.json");
  const objLines = data.store.objects.map((o) => `${fmt(o.bytes)} octets  ${o.path}`).join("\n");
  terminal(
    $("#term-store"),
    [
      {
        cmd: "vg asm examples/quickstart.vgs --root target/site/graphe/store",
        out: data.store.asm_out.trimEnd(),
      },
      {
        cmd: "cat target/site/graphe/store/vg.lock",
        out: data.store.vg_lock.trimEnd(),
      },
      {
        cmd: "ls -la target/site/graphe/store/.vg/objects/*/*.vgir",
        out: objLines,
      },
    ],
    { title: "vg-store · objets .vgir et vg.lock" }
  );
});
