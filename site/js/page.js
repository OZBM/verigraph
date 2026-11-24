// What every page of the site shares: the chapter list, the page chrome (header, menu, table of
// contents, pager, footer), the @motion easing table and the small components the chapters use
// (reveal, quiz, tabs, annotated code, terminal replay, scrollytelling, diagram animation).
// Like the rest of the site's JavaScript it keeps time, routes events and moves bytes and text into
// the DOM: every value a reader sees comes from a VeriGraph binary in wasm/ or from a toolchain
// report written by site/build.sh.
import { load, encode, f32s, u32, hex } from "./vg.js";

export { load, encode, f32s, u32, hex };
export const $ = (sel, root = document) => root.querySelector(sel);
export const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];
export const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
export const fmt = (n) => Number(n).toLocaleString("fr-FR");
export const getJSON = (url) => fetch(url).then((r) => (r.ok ? r.json() : Promise.reject(new Error(`${url} : ${r.status}`))));

export const el = (tag, attrs = {}, ...kids) => {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "text") n.textContent = v;
    else if (k === "html") n.innerHTML = v;
    else n.setAttribute(k, v);
  }
  n.append(...kids);
  return n;
};
export const svg = (tag, attrs = {}) => {
  const n = document.createElementNS("http://www.w3.org/2000/svg", tag);
  for (const [k, v] of Object.entries(attrs)) n.setAttribute(k, v);
  return n;
};

// [file, number shown, title, the crates or tools it covers]
export const CHAPTERS = [
  ["index.html", "", "Accueil", "VeriGraph en une page"],
  ["idee.html", "1", "Pourquoi VeriGraph", "le problème, l'idée, les garanties"],
  ["graphe.html", "2", "Un programme est un graphe", "VG-IR · vg-core · vg-store"],
  ["vg-sym.html", "3", "Lire et écrire VG-Sym", "le langage · vg-decode"],
  ["types.html", "4", "Types, effets et capacités", "vg-types"],
  ["preuves.html", "5", "Contrats et preuves", "vg-smt · z3 · cvc5 · Bitwuzla"],
  ["contre-exemple.html", "6", "Quand la preuve échoue", "vg-interp · vg-telemetry"],
  ["compilation.html", "7", "Du graphe au binaire", "vg-codegen · vg-rt"],
  ["agent.html", "8", "La boucle agent", "vg-cli · vg mcp · le skill"],
  ["mesh.html", "9", "VG-Mesh, le registre", "vg-mesh"],
  ["omnilift.html", "10", "OmniLift : importer l'existant", "vg-omnilift"],
  ["ce-site.html", "11", "Ce site est un programme", "la bibliothèque web vérifiée"],
  ["utiliser.html", "12", "Utiliser VeriGraph", "installer · premier nœud · avec une IA"],
  ["glossaire.html", "", "Glossaire", "tous les termes, en une page"],
];

// ---- Motion: the @motion table, 8 curves x 256 samples, in @ease_sample's order -------------------

export const CURVE = { IN_QUAD: 0, OUT_CUBIC: 1, IN_OUT_CUBIC: 2, OUT_QUART: 3, IN_OUT_QUINT: 4, OUT_BACK: 5, OUT_BOUNCE: 6, CSS_EASE: 7 };
let table = null;
export const motion = (async () => {
  const run = await load(new URL("../wasm/motion.wasm", import.meta.url));
  table = f32s(await run(), 0, 2048);
  // CSS transitions use the same curves: linear() through every 8th sample, as computed.
  const linear = (k) => `linear(${table.slice(k * 256, k * 256 + 256).filter((_, j) => j % 8 === 0 || j === 255).map((v) => +v.toFixed(4)).join(", ")})`;
  if (CSS.supports("transition-timing-function", "linear(0, 1)")) {
    const root = document.documentElement.style;
    root.setProperty("--vg-out", linear(CURVE.OUT_CUBIC));
    root.setProperty("--vg-back", linear(CURVE.OUT_BACK));
    root.setProperty("--vg-css", linear(CURVE.CSS_EASE));
    root.setProperty("--vg-inout", linear(CURVE.IN_OUT_CUBIC));
  }
  return table;
})().catch((e) => (console.error(e), null));

// Plays curve k for ms milliseconds; set(value, j) gets sample j of the table on each frame.
export function play(k, ms, set, delay = 0) {
  return new Promise((done) => {
    if (!table || reduced) return set(1, 255), done();
    let t0 = null;
    const frame = (now) => {
      t0 ??= now + delay;
      const j = Math.max(0, Math.min(255, Math.floor(((now - t0) / ms) * 255)));
      set(table[k * 256 + j], j);
      j < 255 ? requestAnimationFrame(frame) : done();
    };
    requestAnimationFrame(frame);
  });
}

// Runs fn once, the first time target scrolls into view. With reduced motion it runs at once.
export function onFirstView(target, fn, threshold = 0.15) {
  if (!target) return;
  const io = new IntersectionObserver((entries) => {
    if (entries.some((e) => e.isIntersecting)) io.disconnect(), fn();
  }, { threshold });
  io.observe(target);
}

// Runs one part of a page; if it fails, says so in place instead of leaving it blank.
export async function section(where, fn) {
  try {
    await fn();
  } catch (e) {
    const host = typeof where === "string" ? $(where) : where;
    host?.append(el("p", { class: "err", text: `Cette partie n'a pas pu s'exécuter : ${e.message}` }));
    console.error(e);
  }
}

// ---- Proof tags: what the toolchain proved about the node that drew a figure -----------------------

let reports = null;
const loadReports = () => (reports ??= Promise.all([
  getJSON(new URL("../wasm/proofs.json", import.meta.url)),
  getJSON(new URL("../wasm/builds.json", import.meta.url)),
]).then(([proofs, builds]) => ({
  node: Object.fromEntries(proofs.by_node.map((n) => [n.label, n])),
  bin: Object.fromEntries(builds.binaries.map((b) => [b.node, b])),
  proofs, builds,
})));
export const siteReports = loadReports;

// "calculé par @layout · 196 obligations sur 196 prouvées (19 nœuds) · 0 garde à l'exécution".
// For a binary's root the count covers every node it calls; otherwise the node's own obligations.
export async function proofTag(label, claim = "") {
  const { node, bin } = await loadReports();
  const b = bin[label];
  const p = node[label];
  const parts = [`calculé par ${label}`];
  if (claim) parts.push(claim);
  if (b && !b.verified) parts.push(`<span class="miss">construit avec --unverified : rien n'est prouvé</span>`);
  else if (b) parts.push(`<b>${fmt(b.proven)} obligations sur ${fmt(b.obligations)} prouvées</b> (${fmt(b.closure_nodes)} nœud${b.closure_nodes > 1 ? "s" : ""})`);
  else if (p) parts.push(`<b>${fmt(p.proven)} obligations sur ${fmt(p.n)} prouvées</b>`);
  if (b) parts.push(`${fmt(b.residual_guards)} garde${b.residual_guards > 1 ? "s" : ""} à l'exécution`);
  return parts.join(" · ");
}

// ---- Page chrome -------------------------------------------------------------------------------------

const here = location.pathname.split("/").pop() || "index.html";
const at = Math.max(0, CHAPTERS.findIndex(([f]) => f === here));

function chrome() {
  const bar = $("header.bar");
  if (bar && !bar.children.length) {
    const [, num, title] = CHAPTERS[at];
    const menu = el("nav", { class: "menu", id: "menu", "aria-label": "Chapitres" });
    const ol = el("ol");
    for (const [file, n, t, sub] of CHAPTERS) {
      const a = el("a", { href: file }, el("span", { class: "n", text: n || "·" }), el("span", { class: "t" }, t, el("small", { text: sub })));
      if (file === here) a.setAttribute("aria-current", "page");
      ol.append(el("li", {}, a));
    }
    menu.append(ol);
    const btn = el("button", { class: "menu-btn", type: "button", "aria-expanded": "false", "aria-controls": "menu", text: "Chapitres" });
    const toggle = (open) => (menu.classList.toggle("open", open), btn.setAttribute("aria-expanded", String(open)));
    btn.addEventListener("click", () => toggle(!menu.classList.contains("open")));
    document.addEventListener("keydown", (e) => e.key === "Escape" && toggle(false));
    document.addEventListener("click", (e) => !bar.contains(e.target) && toggle(false));
    const links = el("div", { class: "bar-links" });
    for (const [file, label] of [["utiliser.html", "Utiliser"], ["glossaire.html", "Glossaire"]]) {
      const a = el("a", { href: file, text: label });
      if (file === here) a.setAttribute("aria-current", "page");
      links.append(a);
    }
    links.append(btn);
    const inner = el("div", { class: "wrap bar-in" },
      el("a", { class: "mark", href: "index.html", "aria-label": "VeriGraph, accueil", html: "Veri<em>Graph</em>" }),
      at > 0 ? el("span", { class: "here", html: num ? `<b>${num}</b> · ${title}` : `<b>${title}</b>` }) : "",
      links, menu, el("div", { class: "progress", "aria-hidden": "true" }));
    bar.append(inner);
    document.body.prepend(el("a", { class: "skip", href: "#top", text: "Aller au contenu" }));
  }

  // Table of contents from the article's h2 headings, with the one in view marked.
  const toc = $("#toc");
  const heads = $$(".prose h2[id]");
  if (toc && heads.length) {
    const ol = el("ol");
    const links = heads.map((h) => {
      const a = el("a", { href: `#${h.id}`, text: h.dataset.toc || h.textContent.replace(/^\s*\d+(\.\d+)?\s*/, "") });
      ol.append(el("li", {}, a));
      return a;
    });
    toc.append(el("p", { class: "label", text: "Dans ce chapitre" }), ol);
    // Each observed section stands for its heading's link.
    const headOf = new Map(heads.map((h) => [h.closest("section") ?? h, h.id]));
    const io = new IntersectionObserver((entries) => {
      for (const e of entries) if (e.isIntersecting) links.forEach((a) => a.classList.toggle("on", a.getAttribute("href") === `#${headOf.get(e.target)}`));
    }, { rootMargin: "-20% 0px -70% 0px" });
    headOf.forEach((_, target) => io.observe(target));
  }

  const pager = $("#pager");
  if (pager && !pager.children.length) {
    const prev = CHAPTERS[at - 1], next = CHAPTERS[at + 1];
    if (prev) pager.append(el("a", { class: "prev", href: prev[0] }, el("span", { class: "label", text: "← Précédent" }), el("b", { text: prev[2] })));
    if (next) pager.append(el("a", { class: "next", href: next[0] }, el("span", { class: "label", text: "Suivant →" }), el("b", { text: next[2] })));
  }

  const foot = $("footer.foot");
  if (foot && !foot.children.length) {
    foot.append(el("div", { class: "wrap" },
      el("p", { html: `Ce site est un programme VeriGraph : chaque courbe, schéma, image, son et empreinte y est calculé par des nœuds vérifiés compilés en WebAssembly, et chaque chiffre vient d'une exécution réelle de la chaîne d'outils. <a href="ce-site.html">Comment c'est fait</a>.` }),
      el("p", { html: `<a href="glossaire.html">Glossaire</a> · <a href="utiliser.html">Utiliser VeriGraph</a>` })));
  }
}

// ---- Components ---------------------------------------------------------------------------------------

// Every .reveal eases in the first time it is seen.
function reveals() {
  for (const node of $$(".reveal")) {
    if (reduced) continue;
    node.style.setProperty("--e", 0);
    onFirstView(node, async () => (await motion, play(CURVE.OUT_CUBIC, 900, (e) => node.style.setProperty("--e", e), Number(node.dataset.delay || 0))), 0.1);
  }
}

// <div class="quiz"><p class="q">…</p><ol><li><button class="choice" data-right>…</button></li>…</ol><p class="why" hidden>…</p></div>
function quizzes() {
  for (const quiz of $$(".quiz")) {
    const choices = $$("button.choice", quiz);
    for (const c of choices) {
      c.type = "button";
      c.addEventListener("click", () => {
        c.classList.add(c.hasAttribute("data-right") ? "right" : "wrong");
        for (const r of choices) if (r.hasAttribute("data-right")) r.classList.add("right");
        const why = $(".why", quiz);
        if (why) why.hidden = false;
      });
    }
  }
}

// <div class="tabs"><div class="tablist"><button aria-controls="id">…</button>…</div><div class="tabpanel" id="id">…</div>…</div>
function tabs() {
  for (const box of $$(".tabs")) {
    const list = $(".tablist", box);
    const buttons = $$("button", list);
    list.setAttribute("role", "tablist");
    const pick = (b) => buttons.forEach((x) => {
      const on = x === b;
      x.setAttribute("aria-selected", String(on));
      x.tabIndex = on ? 0 : -1;
      const panel = document.getElementById(x.getAttribute("aria-controls"));
      if (panel) panel.hidden = !on;
    });
    buttons.forEach((b, i) => {
      b.type = "button";
      b.setAttribute("role", "tab");
      b.addEventListener("click", () => pick(b));
      b.addEventListener("keydown", (e) => {
        const k = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
        if (k) e.preventDefault(), buttons[(i + k + buttons.length) % buttons.length].focus(), pick(buttons[(i + k + buttons.length) % buttons.length]);
      });
    });
    for (const p of $$(".tabpanel", box)) p.setAttribute("role", "tabpanel");
    pick(buttons[0]);
  }
}

// VG-Sym colouring for <pre class="code vgs">: wraps tokens of the text nodes in classed spans,
// leaving any markup already there (annotations, highlights) in place. Formatting only.
const TOKENS = /(\/\/[^\n]*)|(@[A-Za-z_][\w]*)|(%[\w]+)|\b(NODE|SPEC|SIGNATURE|EFFECTS|CONTRACT|BODY|PRE|POST|INVARIANT|COST|BUDGET|LEARNABLE|RETURN|module|forall|in|true|false)\b|\b(u8|u16|u32|u64|i8|i16|i32|i64|f16|bf16|f32|f64|Bool|Array|Slice|Tensor|Tuple|Result|Eff|FileIn|FileOut|Clock)\b|\b([A-Z][A-Z0-9_]{1,}(?:\.[a-z]+)?)(?=\()|(-?\b\d[\d_]*(?:\.\d+)?(?:e[+-]?\d+)?(?::[a-z]\d+)?\b)/g;
const CLASSES = ["com", "lab", "port", "kw", "ty", "op", "lit"];
export function highlight(pre) {
  const walker = document.createTreeWalker(pre, NodeFilter.SHOW_TEXT);
  const texts = [];
  while (walker.nextNode()) texts.push(walker.currentNode);
  for (const t of texts) {
    if (t.parentElement.closest(".com, .lab, .port, .kw, .ty, .op, .lit")) continue;
    const s = t.textContent;
    const frag = document.createDocumentFragment();
    let last = 0;
    for (const m of s.matchAll(TOKENS)) {
      const g = m.slice(1).findIndex((x) => x !== undefined);
      frag.append(s.slice(last, m.index), el("span", { class: CLASSES[g], text: m[0] }));
      last = m.index + m[0].length;
    }
    if (last === 0) continue;
    frag.append(s.slice(last));
    t.replaceWith(frag);
  }
}

// <div class="annotated"><pre class="code">… <span data-note="…">…</span> …</pre><aside class="panel annot-panel">…</aside></div>
function annotations() {
  for (const box of $$(".annotated")) {
    const panel = $(".annot-panel", box);
    const p = panel && ($("p", panel) ?? panel.appendChild(el("p")));
    const base = p?.innerHTML ?? "";
    const label = panel && $(".label", panel);
    const baseLabel = label?.textContent ?? "";
    for (const span of $$("[data-note]", box)) {
      span.tabIndex = 0;
      const show = () => {
        $$("[data-note].on", box).forEach((x) => x.classList.remove("on"));
        span.classList.add("on");
        if (p) p.innerHTML = span.dataset.note;
        if (label) label.textContent = span.dataset.title || span.textContent.trim().slice(0, 48);
      };
      span.addEventListener("pointerenter", show);
      span.addEventListener("focus", show);
      span.addEventListener("click", show);
    }
    box.addEventListener("pointerleave", () => {
      if (!p) return;
      $$("[data-note].on", box).forEach((x) => x.classList.remove("on"));
      p.innerHTML = base;
      if (label) label.textContent = baseLabel;
    });
  }
}

// A terminal that replays real commands and their real output (from a toolchain report):
// steps = [{ cmd, out }]. It only keeps time; the text is shown as recorded.
export function terminal(host, steps, { title = "terminal", speed = 22 } = {}) {
  const body = el("div", { class: "term-body", "aria-live": "off" });
  const again = el("button", { type: "button", text: "Rejouer" });
  host.classList.add("term");
  host.replaceChildren(el("div", { class: "term-bar" }, el("i"), el("i"), el("i"), el("span", { text: title }), again), body);
  let run = 0;
  const wait = (ms) => new Promise((r) => setTimeout(r, ms));
  const replay = async () => {
    const me = ++run;
    body.replaceChildren();
    for (const { cmd, out } of steps) {
      const c = el("span", { class: "cmd" });
      const caret = el("span", { class: "caret" });
      body.append(c, caret);
      if (reduced) c.textContent = cmd;
      else for (let i = 1; i <= cmd.length; i++) {
        if (me !== run) return;
        c.textContent = cmd.slice(0, i);
        await wait(speed);
      }
      await wait(reduced ? 0 : 260);
      caret.remove();
      body.append(el("span", { class: "out", text: out }));
      body.scrollTop = body.scrollHeight;
      await wait(reduced ? 0 : 520);
    }
  };
  again.addEventListener("click", replay);
  onFirstView(host, replay, 0.3);
  return { replay };
}

// Scrollytelling: calls onStep(i, stepElement) whenever step i of root's .scrolly-steps crosses the
// middle of the screen. The stage stays sticky beside the steps (css/site.css).
export function scrolly(root, onStep) {
  const steps = $$(".scrolly-steps > *", root);
  let current = -1;
  const io = new IntersectionObserver((entries) => {
    for (const e of entries) {
      if (!e.isIntersecting) continue;
      const i = steps.indexOf(e.target);
      if (i === current) continue;
      current = i;
      steps.forEach((s, j) => s.classList.toggle("on", j === i));
      onStep(i, e.target);
    }
  }, { rootMargin: "-45% 0px -45% 0px" });
  steps.forEach((s) => io.observe(s));
  return steps;
}

// Diagrams (js/diagram.js) ---------------------------------------------------------------------------

// Draws a laid-out diagram in: edges trace themselves, then nodes fade in, eased by @motion.
export async function drawIn(result, ms = 1100) {
  await motion;
  const nodes = [...result.nodes.values()];
  nodes.forEach((n) => n.style.setProperty("--e", reduced ? 1 : 0));
  result.edges.forEach((e) => e.el.style.setProperty("--e", reduced ? 1 : 0));
  if (reduced) return;
  await Promise.all([
    ...nodes.map((n, i) => play(CURVE.OUT_CUBIC, 600, (e) => n.style.setProperty("--e", e), i * 45)),
    ...result.edges.map((ed, i) => play(CURVE.IN_OUT_CUBIC, ms, (e) => ed.el.style.setProperty("--e", e), 200 + i * 35)),
  ]);
}

// Sends a glowing packet along one edge of a diagram (its path, as @layout drew it).
export function pulse(svgEl, edge, ms = 1000, cls = "") {
  if (reduced) return Promise.resolve();
  const dot = svg("circle", { r: 6, class: `dg-packet ${cls}`.trim() });
  dot.style.offsetPath = `path("${edge.d}")`;
  dot.style.offsetRotate = "0deg";
  svgEl.append(dot);
  return play(CURVE.IN_OUT_CUBIC, ms, (e) => (dot.style.offsetDistance = `${e * 100}%`)).then(() => dot.remove());
}

// ---- Start ----------------------------------------------------------------------------------------------

chrome();
reveals();
quizzes();
tabs();
for (const pre of $$("pre.code.vgs")) highlight(pre);
annotations();
