import {
  $,
  el,
  fmt,
  getJSON,
  section,
  proofTag,
  terminal,
  scrolly,
  onFirstView,
  drawIn,
  pulse,
} from "../page.js";
import { diagram } from "../diagram.js";

section("#term-ops", async () => {
  const data = await getJSON("data/vg-sym.json");
  $("#lark-lines").textContent = fmt(data.lark.lines);
  $("#lark-nt").textContent = fmt(data.lark.nonterminals);
  $("#lark-term").textContent = fmt(data.lark.terminals);
  terminal($("#term-ops"), data.runs, { title: "vg run · examples/quickstart.vgs" });
});

section("#fig-map", async () => {
  const svgEl = $("#fig-map");
  const d = await diagram(svgEl, {
    nodes: [
      { id: "frame", label: "frame", sub: "Tensor<f32,[512,2]>", kind: "input", min: 0 },
      { id: "knee", label: "knee", sub: "f32 ∈ [0.5, 0.99]", kind: "input", min: 0 },
      { id: "map", label: "%0 = SIMD_MAP", sub: "@soft_knee × 1024", kind: "op" },
      { id: "bytes", label: "%1 = AS_BYTES", sub: "vue Slice<u8>", kind: "op" },
      { id: "call", label: "%2 = CALL_HASH", sub: "@sha256", kind: "warn" },
      { id: "ret", label: "RETURN(%0, %2)", sub: "frame + digest", kind: "out" },
    ],
    edges: [
      ["frame", "map"],
      ["knee", "map"],
      ["map", "bytes"],
      ["map", "ret"],
      ["bytes", "call"],
      ["call", "ret"],
    ],
    dx: 195,
    dy: 84,
    bw: 158,
    bh: 52,
  });

  const runAnim = async () => {
    for (const n of d.nodes.values()) n.classList.remove("on");
    for (const e of d.edges) e.el.classList.remove("on");
    d.nodes.get("frame").classList.add("on");
    d.nodes.get("knee").classList.add("on");
    await Promise.all(
      d.edges.filter((e) => e.to === "map").map((e) => (e.el.classList.add("on"), pulse(svgEl, e, 480, "")))
    );
    d.nodes.get("map").classList.add("on");
    const eBytes = d.edges.find((e) => e.from === "map" && e.to === "bytes");
    const eRet = d.edges.find((e) => e.from === "map" && e.to === "ret");
    eBytes.el.classList.add("on");
    eRet.el.classList.add("on");
    await Promise.all([pulse(svgEl, eBytes, 450, ""), pulse(svgEl, eRet, 650, "")]);
    d.nodes.get("bytes").classList.add("on");
    const eCall = d.edges.find((e) => e.from === "bytes" && e.to === "call");
    eCall.el.classList.add("on");
    await pulse(svgEl, eCall, 450, "");
    d.nodes.get("call").classList.add("on");
    const eEnd = d.edges.find((e) => e.from === "call" && e.to === "ret");
    eEnd.el.classList.add("on");
    await pulse(svgEl, eEnd, 450, "");
    d.nodes.get("ret").classList.add("on");
  };

  $("#btn-map").addEventListener("click", runAnim);
  onFirstView(svgEl, async () => {
    await drawIn(d);
    runAnim();
  });
  $("#fig-map-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
});

section("#fig-fold", async () => {
  const svgEl = $("#fig-fold");
  const d = await diagram(svgEl, {
    nodes: [
      { id: "init", label: "init = 0:u32", sub: "k = 0 · acc <= 0", kind: "input" },
      { id: "s0", label: "@add_step", sub: "k = 1 · xs[0]", kind: "op" },
      { id: "s1", label: "@add_step", sub: "k = 2 · xs[1]", kind: "op" },
      { id: "s2", label: "@add_step", sub: "k = 3 · xs[2]", kind: "op" },
      { id: "s3", label: "@add_step", sub: "k = 4 · xs[3]", kind: "ok" },
      { id: "ret", label: "RETURN(%0)", sub: "acc <= 4000", kind: "out" },
    ],
    edges: [
      ["init", "s0"],
      ["s0", "s1"],
      ["s1", "s2"],
      ["s2", "s3"],
      ["s3", "ret"],
    ],
    dx: 174,
    dy: 80,
    bw: 146,
    bh: 52,
  });

  const runAnim = async () => {
    for (const n of d.nodes.values()) n.classList.remove("on");
    for (const e of d.edges) e.el.classList.remove("on");
    d.nodes.get("init").classList.add("on");
    for (const e of d.edges) {
      e.el.classList.add("on");
      await pulse(svgEl, e, 400, "");
      d.nodes.get(e.to).classList.add("on");
    }
  };

  $("#btn-fold").addEventListener("click", runAnim);
  onFirstView(svgEl, async () => {
    await drawIn(d);
    runAnim();
  });
  $("#fig-fold-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
});

section("#fig-gen", async () => {
  const svgEl = $("#fig-gen");
  const d = await diagram(svgEl, {
    nodes: [
      { id: "n16", label: "16:u64", sub: "indices 0..15", kind: "input", min: 0 },
      { id: "pal", label: "GENERATE", sub: "@palette_entry", kind: "op" },
      { id: "p0", label: "%0 : Array", sub: "16 couleurs u32", kind: "ok" },
      { id: "view", label: "cx, cy, scale", sub: "précondition PRE", kind: "input" },
      { id: "n49k", label: "49152:u64", sub: "indices 0..49151", kind: "input" },
      { id: "pix", label: "GENERATE", sub: "@pixel", kind: "op" },
      { id: "ret", label: "RETURN(%1)", sub: "Array<u32, 49152>", kind: "out" },
    ],
    edges: [
      ["n16", "pal"],
      ["pal", "p0"],
      ["p0", "pix"],
      ["view", "pix"],
      ["n49k", "pix"],
      ["pix", "ret"],
    ],
    dx: 184,
    dy: 76,
    bw: 154,
    bh: 52,
  });

  const runAnim = async () => {
    for (const n of d.nodes.values()) n.classList.remove("on");
    for (const e of d.edges) e.el.classList.remove("on");
    d.nodes.get("n16").classList.add("on");
    for (const [a, b] of [
      ["n16", "pal"],
      ["pal", "p0"],
    ]) {
      const e = d.edges.find((x) => x.from === a && x.to === b);
      e.el.classList.add("on");
      await pulse(svgEl, e, 420, "");
      d.nodes.get(b).classList.add("on");
    }
    d.nodes.get("view").classList.add("on");
    d.nodes.get("n49k").classList.add("on");
    await Promise.all(
      d.edges.filter((e) => e.to === "pix").map((e) => (e.el.classList.add("on"), pulse(svgEl, e, 480, "")))
    );
    d.nodes.get("pix").classList.add("on");
    const eRet = d.edges.find((e) => e.from === "pix" && e.to === "ret");
    eRet.el.classList.add("on");
    await pulse(svgEl, eRet, 420, "");
    d.nodes.get("ret").classList.add("on");
  };

  $("#btn-gen").addEventListener("click", runAnim);
  onFirstView(svgEl, async () => {
    await drawIn(d);
    runAnim();
  });
  $("#fig-gen-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
});

section("#story-decode", async () => {
  const tokens = ["%0", "=", "ADD", ".", "checked", "(", "x", ",", "1", ":", "u32", ")", ";", "RETURN", "(", "%0", ")", ";"];
  const steps = [
    {
      at: 0,
      allow: ["NewPort (%0)", "RETURN"],
      block: ["ADD", "x", "1:u32"],
      note: "Au début d'une instruction dans BODY, seul un nouveau port ou RETURN est valide.",
    },
    {
      at: 1,
      allow: ["=", ":", ","],
      block: ["(", ".", "ADD"],
      note: "Après %0, on peut annoter son type (:), lister d'autres ports (,) ou assigner (=).",
    },
    {
      at: 2,
      allow: ["ADD", "SUB", "MUL", "AND", "… (opcodes purs)"],
      block: ["FILE_READ", "FILE_WRITE", "CLOCK_READ"],
      note: "EFFECTS étant vide [], Prefix retire tous les opcodes à effet de allowed().opcodes.",
    },
    {
      at: 3,
      allow: ["."],
      block: ["(", ";", "x"],
      note: "ADD exige un mode : la parenthèse ( est interdite tant que .<mode> n'est pas écrit.",
    },
    {
      at: 4,
      allow: ["checked", "wrap", "sat"],
      block: ["s", "u", "f", "u32"],
      note: "Pour ADD, seuls les trois modes d'arithmétique entière sont acceptés.",
    },
    {
      at: 6,
      allow: ["x: u32", "IntLiteral", "-"],
      block: ["y: f32", "true", "@label"],
      note: "ADD.checked attend un entier : x (u32) est proposé, mais y (f32) est filtré hors des ports permis.",
    },
    {
      at: 10,
      allow: ["u32"],
      block: ["i32", "u8", "f32", "Bool"],
      note: "x a déjà fixé le type de l'addition à u32 : après 1:, seul u32 préserve la cohérence des opérandes.",
    },
    {
      at: 15,
      allow: ["%0: u32", "x: u32", "IntLiteral"],
      block: ["y: f32", "%99"],
      note: "La signature exige un résultat u32 : RETURN( accepte %0 ou x, jamais y: f32.",
    },
  ];

  const lineEl = $("#decode-line");
  const allowEl = $("#decode-allow");
  const blockEl = $("#decode-block");
  const noteEl = $("#decode-note");

  const renderStep = (idx) => {
    const st = steps[idx] ?? steps[0];
    lineEl.replaceChildren(
      ...tokens.map((t, j) => {
        const cls = j < st.at ? "decode-tok done" : j === st.at ? "decode-tok cur" : "decode-tok";
        return el("span", { class: cls, text: t });
      })
    );
    allowEl.replaceChildren(...st.allow.map((s) => el("span", { class: "tok-pill allow", text: s })));
    blockEl.replaceChildren(...st.block.map((s) => el("span", { class: "tok-pill block", text: s })));
    noteEl.textContent = st.note;
  };

  renderStep(0);
  scrolly($("#story-decode"), (i) => renderStep(i));
});
