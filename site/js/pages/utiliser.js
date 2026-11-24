import {
  $, $$, el, fmt, getJSON, proofTag, terminal,
  section, drawIn, pulse, reduced
} from "../page.js";
import { diagram } from "../diagram.js";

const pretty = (obj) => JSON.stringify(obj, null, 2);

function wireCopyButtons() {
  for (const btn of $$(".copy-btn")) {
    btn.addEventListener("click", async () => {
      const code = btn.parentElement?.querySelector("pre.code")?.textContent?.trim() ?? "";
      if (!code) return;
      try {
        await navigator.clipboard.writeText(code);
        btn.textContent = "Copié";
        btn.classList.add("copied");
        setTimeout(() => {
          btn.textContent = "Copier";
          btn.classList.remove("copied");
        }, 1600);
      } catch {
        btn.textContent = "Sélectionnez";
      }
    });
  }
}

async function wait(ms) {
  if (reduced) return;
  return new Promise((r) => setTimeout(r, ms));
}

async function main() {
  wireCopyButtons();
  const data = await getJSON("data/utiliser.json");

  await section("#premier-noeud", async () => {
    const c = data.counts;
    const statsHost = $("#quickstart-stats");
    statsHost.replaceChildren(
      el("div", { class: "stat" }, el("b", { text: fmt(c.quickstart_nodes) }), el("span", { text: "nœuds dans examples/quickstart.vgs" })),
      el("div", { class: "stat" }, el("b", { text: `${fmt(c.quickstart_proven)} / ${fmt(c.quickstart_obligations)}` }), el("span", { text: "obligations prouvées (vg verify --mode edit)" })),
      el("div", { class: "stat" }, el("b", { text: `${fmt(c.build_bytes)} o` }), el("span", { text: `binaire ${c.build_target}` })),
      el("div", { class: "stat" }, el("b", { text: fmt(c.build_guards) }), el("span", { text: "garde à l'exécution (residual_guards)" })),
    );

    const quickSteps = [
      {
        cmd: "vg check examples/quickstart.vgs",
        out: pretty(data.check_quick),
      },
      {
        cmd: "vg verify examples/quickstart.vgs --mode edit --detail 1",
        out: pretty({
          ok: data.verify_quick.ok,
          nodes: data.verify_quick.nodes,
          results_count: data.verify_quick.results.length,
          sample: data.verify_quick.results.slice(0, 3),
        }),
      },
      {
        cmd: "vg run examples/quickstart.vgs @add_sat '{\"a\": 200, \"b\": 100}'",
        out: pretty(data.run_add_sat),
      },
      {
        cmd: "vg run examples/quickstart.vgs @soft_knee '{\"x\": \"3e38\", \"knee\": \"0.5\"}'",
        out: pretty(data.run_soft_knee),
      },
      {
        cmd: "vg build examples/quickstart.vgs @soft_knee",
        out: pretty(data.build_soft_knee),
      },
    ];
    terminal($("#term-quickstart"), quickSteps, { title: "examples/quickstart.vgs — check, verify, run, build", speed: 16 });

    const bugSteps = [
      {
        cmd: "vg verify examples/soft_knee_draft.vgs --mode edit --detail 2",
        out: `${pretty({
          ok: data.verify_draft.ok,
          nodes: data.verify_draft.nodes,
          first_refuted: data.verify_draft.results.find((r) => r.verdict === "counterexample"),
        })}\n[exit ${data.exit_codes.verify_draft}]`,
      },
      {
        cmd: "vg explain examples/soft_knee_draft.vgs @soft_knee/POST.0",
        out: pretty(data.explain_draft),
      },
      {
        cmd: "vg run examples/soft_knee_draft.vgs @soft_knee '{\"x\": \"3e38\", \"knee\": \"0.5\"}'",
        out: pretty(data.run_draft),
      },
    ];
    terminal($("#term-bug"), bugSteps, { title: "examples/soft_knee_draft.vgs — contre-exemple concret", speed: 16 });
  });

  await section("#fig-workflow-wrap", async () => {
    const svgEl = $("#fig-workflow");
    const d = await diagram(svgEl, {
      nodes: [
        { id: "edit", label: "Éditer .vgs", sub: "VG-Sym", kind: "input" },
        { id: "check", label: "vg check", sub: "types & hash", kind: "op" },
        { id: "verify", label: "vg verify", sub: "solveurs SMT", kind: "op" },
        { id: "explain", label: "vg explain", sub: "tranche & diff", kind: "warn" },
        { id: "fix", label: "Corriger", sub: "corps ou PRE", kind: "ai" },
        { id: "build", label: "vg build", sub: "LLVM IR → clang", kind: "ok" },
        { id: "bin", label: "Binaire", sub: "0 garde résiduelle", kind: "out" },
      ],
      edges: [
        ["edit", "check"],
        ["check", "verify"],
        ["verify", "explain"],
        ["explain", "fix"],
        ["verify", "build"],
        ["build", "bin"],
      ],
      back: [["fix", "edit"]],
      dx: 175,
      dy: 84,
      bw: 144,
      bh: 52,
    });
    $("#fig-workflow-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
    await drawIn(d);

    const edgeMap = new Map(d.edges.map((e) => [`${e.from}->${e.to}`, e]));
    const status = $("#loop-status");
    const btn = $("#btn-sim-loop");

    const clearClasses = () => {
      for (const n of d.nodes.values()) n.classList.remove("on", "bad", "dim");
      for (const e of d.edges) e.el.classList.remove("on", "bad", "dim");
    };

    btn.addEventListener("click", async () => {
      btn.disabled = true;
      clearClasses();
      status.textContent = "Tour 1 : vérification réfutée sur @soft_knee/POST.0…";
      for (const id of ["edit", "check", "verify"]) {
        d.nodes.get(id).classList.add("on");
      }
      await pulse(svgEl, edgeMap.get("edit->check"), 520, "");
      await pulse(svgEl, edgeMap.get("check->verify"), 520, "");
      d.nodes.get("verify").classList.replace("on", "bad");
      d.nodes.get("explain").classList.add("bad");
      await pulse(svgEl, edgeMap.get("verify->explain"), 550, "bad");
      status.textContent = "Contre-exemple reproduit → correction du brouillon…";
      await pulse(svgEl, edgeMap.get("explain->fix"), 520, "ai");
      d.nodes.get("fix").classList.add("on");
      await pulse(svgEl, edgeMap.get("fix->edit"), 680, "ai");

      await wait(240);
      clearClasses();
      status.textContent = "Tour 2 : toutes les obligations prouvées → compilation sans garde.";
      for (const [a, b] of [["edit", "check"], ["check", "verify"], ["verify", "build"], ["build", "bin"]]) {
        d.nodes.get(a).classList.add("on");
        await pulse(svgEl, edgeMap.get(`${a}->${b}`), 500, "");
        d.nodes.get(b).classList.add("on");
      }
      status.textContent = "Terminé : 0 garde résiduelle.";
      btn.disabled = false;
    });
  });

  await section("#fig-ide-arch-wrap", async () => {
    const svgEl = $("#fig-ide-arch");
    const d = await diagram(svgEl, {
      nodes: [
        { id: "vgs", label: "Fichier .vgs", sub: "éditeur VS Code / Agy", kind: "input" },
        { id: "ext", label: "Extension", sub: "verigraph.verigraph", kind: "op" },
        { id: "agent", label: "Agent IA", sub: "SKILL.md", kind: "ai" },
        { id: "mcp", label: "vg mcp", sub: "JSON-RPC stdio", kind: "op" },
        { id: "vg", label: "Chaîne vg", sub: "check · verify · build", kind: "ok" },
        { id: "out", label: "Diagnostics & Bin", sub: "problèmes + exécutable", kind: "out" },
      ],
      edges: [
        ["vgs", "ext"],
        ["ext", "vg"],
        ["agent", "mcp"],
        ["mcp", "vg"],
        ["vg", "out"],
      ],
      dx: 190,
      dy: 88,
      bw: 156,
      bh: 52,
    });
    $("#fig-ide-arch-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
    await drawIn(d);
  });

  await section("#cli", async () => {
    const tbody = $("#cli-table");
    const rows = data.commands.map((c) =>
      el("tr", {},
        el("td", {}, el("code", { text: c.cmd })),
        el("td", { text: c.summary || "—" }),
      )
    );
    tbody.replaceChildren(...rows);
  });
}

main();
