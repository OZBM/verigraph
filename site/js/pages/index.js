// The home page: the @aurora sky, the six-step tour, the site's own proof count, the map of the
// crates and the chapter path. Values come from wasm/*.wasm and the build's reports.
import { $, $$, el, svg, fmt, motion, play, CURVE, onFirstView, section, proofTag, siteReports, drawIn, pulse, reduced, CHAPTERS } from "../page.js";
import { diagram } from "../diagram.js";

const narrow = matchMedia("(max-width: 720px)").matches;

// ---- Hero sky: one @aurora frame per message, computed off the main thread ---------------------

await section(".hero", async () => {
  const ctx = $("#sky").getContext("2d");
  const hud = $("#hud-line");
  const worker = new Worker(new URL("../sky.js", import.meta.url), { type: "module" });
  const PERIOD = 40000;
  let busy = true, visible = true, shown = 0;
  worker.onmessage = ({ data }) => {
    if (data === "ready") return (busy = false), reduced && (busy = true, worker.postMessage(0.18));
    if (data.error) return (hud.textContent = data.error), hud.classList.add("miss");
    ctx.putImageData(new ImageData(new Uint8ClampedArray(data.px.buffer), 320, 180), 0, 0);
    if (performance.now() - shown > 500) {
      shown = performance.now();
      hud.textContent = `320 × 180 px · ${data.ms.toFixed(1).replace(".", ",")} ms par image en WebAssembly`;
    }
    busy = reduced;
  };
  new IntersectionObserver(([e]) => (visible = e.isIntersecting)).observe($(".hero"));
  const tick = (now) => {
    if (!busy && visible) busy = true, worker.postMessage((now % PERIOD) / PERIOD);
    requestAnimationFrame(tick);
  };
  if (!reduced) requestAnimationFrame(tick);
  const { bin } = await siteReports();
  $("#hud-proof").textContent = `∀ pixel : opaque · ${fmt(bin["@aurora"].proven)} obligations prouvées`;
});

// ---- The tour -------------------------------------------------------------------------------------

await section("#tour-fig", async () => {
  const fig = $("#tour-fig");
  const d = await diagram(fig, {
    nodes: [
      { id: "req", label: "Demande", sub: "langage naturel", kind: "input" },
      { id: "model", label: "Modèle d'IA", sub: "écrit du VG-Sym", kind: "ai" },
      { id: "check", label: "vg check", sub: "forme, types, effets", kind: "op" },
      { id: "verify", label: "vg verify", sub: "preuves SMT", kind: "ok" },
      { id: "build", label: "vg build", sub: "LLVM, sans gardes", kind: "op" },
      { id: "x86", label: "x86_64", sub: "Linux", kind: "out", key: 0 },
      { id: "arm", label: "ARM64", sub: "macOS", kind: "out", key: 1 },
      { id: "wasm", label: "WASM", sub: "navigateur, WASI", kind: "out", key: 2 },
    ],
    edges: [["req", "model"], ["model", "check"], ["check", "verify"], ["verify", "build"], ["build", "x86"], ["build", "arm"], ["build", "wasm"]],
    back: [["verify", "model"]],
    vertical: narrow,
    ...(narrow ? { dx: 170, dy: 96, bw: 150, bh: 56 } : {}),
  });
  $("#tour-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
  const steps = $$("#tour-steps li");
  const edgeInto = (id) => d.edges.find((e) => e.to === id && !e.back);
  const backEdge = d.edges.find((e) => e.back);
  const outs = d.edges.filter((e) => e.from === "build");
  let current = -1, timer = null, auto = !reduced;

  const show = (i) => {
    current = i;
    steps.forEach((s, j) => s.classList.toggle("on", j === i));
    const on = steps[i].dataset.on;
    for (const [id, n] of d.nodes) n.classList.toggle("on", id === on || (on === "back" && id === "model") || (on === "build" && ["x86", "arm", "wasm"].includes(id)));
    for (const e of d.edges) e.el.classList.toggle("on", e === edgeInto(on) || (on === "back" && e.back) || (on === "build" && outs.includes(e)));
    d.nodes.get("verify").classList.toggle("bad", on === "back");
    if (on === "back") pulse(fig, backEdge, 1300, "bad");
    else if (on === "build") outs.forEach((e) => pulse(fig, e, 1000));
    else if (edgeInto(on)) pulse(fig, edgeInto(on), 900, on === "model" ? "ai" : "");
  };
  const next = () => show((current + 1) % steps.length);
  steps.forEach((s, i) => {
    const pick = () => (auto = false, clearInterval(timer), show(i));
    s.addEventListener("click", pick);
    s.addEventListener("keydown", (e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), pick()));
  });
  onFirstView(fig, async () => {
    await drawIn(d);
    show(0);
    if (auto) timer = setInterval(() => auto && next(), 2600);
  });
});

// ---- The site in numbers ---------------------------------------------------------------------------

await section("#stats", async () => {
  const { proofs, builds } = await siteReports();
  const set = (id, n) => {
    const b = $(id);
    b.style.setProperty("--to", n);
    b.setAttribute("aria-label", fmt(n));
  };
  set("#st-proven", proofs.proven);
  set("#st-nodes", proofs.nodes);
  set("#st-bins", builds.binaries.length);
  set("#st-guards", builds.residual_guards);
  $("#self-proof").innerHTML = `${fmt(proofs.proven)} obligations prouvées sur ${fmt(proofs.total)}, dans ${proofs.modules.length} modules · solveurs : ${proofs.by_solver.filter((s) => s.solver !== "none").map((s) => `${s.solver} (${fmt(s.n)})`).join(", ")}, et ${fmt(proofs.by_solver.find((s) => s.solver === "none")?.n ?? 0)} réglées par le pré-criblage · source : <code>site/build.sh</code>`;
  await motion;
  onFirstView($("#stats"), () => play(CURVE.OUT_QUART, 1800, (e) => $$("#stats .count").forEach((b) => b.style.setProperty("--e", e))));
  if (!reduced) $$("#stats .count").forEach((b) => b.style.setProperty("--e", 0));
});

// ---- The map of the crates -------------------------------------------------------------------------

await section("#map-fig", async () => {
  const CRATES = [
    ["vg-core", "IR, forme canonique, hash", "graphe.html", 0],
    ["vg-store", "objets et vg.lock", "graphe.html", 1],
    ["vg-decode", "grammaires, préfixes", "vg-sym.html", 2],
    ["vg-mesh", "registre de nœuds", "mesh.html", 3],
    ["vg-types", "types et effets", "types.html", 0],
    ["vg-interp", "sémantique de référence", "contre-exemple.html", 0],
    ["vg-smt", "obligations, solveurs", "preuves.html", 1],
    ["vg-telemetry", "tranches, diagnostics", "contre-exemple.html", 0],
    ["vg-codegen", "LLVM IR, trois cibles", "compilation.html", 2],
    ["vg-rt", "runtime C, bac à sable", "compilation.html", 3],
    ["vg-omnilift", "relève du C et du Rust", "omnilift.html", 2],
    ["vg-cli", "vg et vg mcp", "agent.html", 0],
  ];
  const fig = $("#map-fig");
  const d = await diagram(fig, {
    nodes: CRATES.map(([id, sub, , key]) => ({ id, label: id, sub, key, kind: id === "vg-cli" ? "ai" : id === "vg-smt" ? "ok" : "op" })),
    edges: [["vg-core", "vg-store"], ["vg-core", "vg-types"], ["vg-types", "vg-interp"], ["vg-types", "vg-smt"], ["vg-interp", "vg-smt"],
      ["vg-smt", "vg-codegen"], ["vg-codegen", "vg-rt"], ["vg-core", "vg-decode"], ["vg-interp", "vg-telemetry"], ["vg-smt", "vg-telemetry"],
      ["vg-core", "vg-mesh"], ["vg-codegen", "vg-omnilift"], ["vg-smt", "vg-cli"], ["vg-codegen", "vg-cli"], ["vg-mesh", "vg-cli"],
      ["vg-decode", "vg-cli"], ["vg-telemetry", "vg-cli"], ["vg-omnilift", "vg-cli"]],
    vertical: narrow,
    ...(narrow ? { dx: 172, dy: 92, bw: 158, bh: 56 } : { dx: 210, dy: 84, bw: 172, bh: 54 }),
  });
  // Each box becomes a link to its chapter.
  for (const [id, sub, href] of CRATES) {
    const box = d.nodes.get(id);
    const a = svg("a", { href, "aria-label": `${id}, ${sub} : ouvrir le chapitre` });
    box.replaceWith(a);
    a.append(box);
    a.addEventListener("pointerenter", () => {
      for (const e of d.edges) e.el.classList.toggle("on", e.from === id || e.to === id);
    });
    a.addEventListener("pointerleave", () => d.edges.forEach((e) => e.el.classList.remove("on")));
  }
  $("#map-proof").innerHTML = await proofTag("@layout", "disposé en couches, du socle à la ligne de commande");
  onFirstView(fig, () => drawIn(d, 1400));
});

// ---- The chapter path ----------------------------------------------------------------------------

await section("#path", async () => {
  const box = $("#path");
  for (const [file, n, title, sub] of CHAPTERS.slice(1)) {
    box.append(el("a", { href: file, class: "reveal" },
      el("span", { class: "n", text: n ? `Chapitre ${n}` : "Référence" }),
      el("b", { text: title }),
      el("small", { text: sub })));
  }
  for (const [i, a] of [...box.children].entries()) {
    if (reduced) continue;
    a.style.setProperty("--e", 0);
    onFirstView(a, async () => (await motion, play(CURVE.OUT_CUBIC, 800, (e) => a.style.setProperty("--e", e), (i % 3) * 70)), 0.1);
  }
});
