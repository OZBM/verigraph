import {
  $, el, fmt, getJSON, proofTag, terminal,
  section, drawIn, pulse, reduced
} from "../page.js";
import { diagram } from "../diagram.js";

const pretty = (obj) => JSON.stringify(obj, null, 2);

async function wait(ms) {
  if (reduced) return;
  return new Promise((r) => setTimeout(r, ms));
}

async function main() {
  const data = await getJSON("data/agent.json");

  await section("#fig-agent-loop-wrap", async () => {
    const svgEl = $("#fig-agent-loop");
    const d = await diagram(svgEl, {
      nodes: [
        { id: "prompt", label: "Demande", sub: "carte de contrat", kind: "input" },
        { id: "mesh", label: "vg_mesh_query", sub: "réutiliser d'abord", kind: "op" },
        { id: "draft", label: "Brouillon", sub: "VG-Sym @label", kind: "ai" },
        { id: "check", label: "vg_check", sub: "types · effets · hash", kind: "op" },
        { id: "verify", label: "vg_verify", sub: "solveurs SMT", kind: "op" },
        { id: "cex", label: "Contre-exemple", sub: "tranche + diff", kind: "warn" },
        { id: "repair", label: "Réparer", sub: "max 5 tours", kind: "ai" },
        { id: "ship", label: "vg_build", sub: "binaire prouvé", kind: "ok" },
      ],
      edges: [
        ["prompt", "mesh"],
        ["mesh", "draft"],
        ["draft", "check"],
        ["check", "verify"],
        ["verify", "cex"],
        ["cex", "repair"],
        ["verify", "ship"],
      ],
      back: [["repair", "draft"]],
      dx: 170,
      dy: 84,
      bw: 140,
      bh: 52,
    });
    $("#fig-agent-loop-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
    await drawIn(d);

    const edgeMap = new Map(d.edges.map((e) => [`${e.from}->${e.to}`, e]));
    const status = $("#agent-loop-status");
    const btnFail = $("#btn-round-fail");
    const btnPass = $("#btn-round-pass");

    const reset = () => {
      for (const n of d.nodes.values()) n.classList.remove("on", "bad", "dim");
      for (const e of d.edges) e.el.classList.remove("on", "bad", "dim");
    };

    btnFail.addEventListener("click", async () => {
      btnFail.disabled = btnPass.disabled = true;
      reset();
      status.textContent = "Tour 1 : brouillon → vg_check → vg_verify…";
      for (const [a, b] of [["prompt", "mesh"], ["mesh", "draft"], ["draft", "check"], ["check", "verify"]]) {
        d.nodes.get(a).classList.add("on");
        await pulse(svgEl, edgeMap.get(`${a}->${b}`), 420, a === "draft" ? "ai" : "");
        d.nodes.get(b).classList.add("on");
      }
      await wait(160);
      d.nodes.get("verify").classList.replace("on", "bad");
      d.nodes.get("cex").classList.add("bad");
      status.textContent = "Obligation réfutée : contre-exemple concret + tranche causale.";
      await pulse(svgEl, edgeMap.get("verify->cex"), 520, "bad");
      await pulse(svgEl, edgeMap.get("cex->repair"), 460, "ai");
      d.nodes.get("repair").classList.add("on");
      await pulse(svgEl, edgeMap.get("repair->draft"), 640, "ai");
      status.textContent = "Réparation appliquée sur le brouillon ; prêt pour le tour 2.";
      btnFail.disabled = btnPass.disabled = false;
    });

    btnPass.addEventListener("click", async () => {
      btnFail.disabled = btnPass.disabled = true;
      reset();
      status.textContent = "Tour 2 : vérification du nœud corrigé…";
      for (const [a, b] of [["draft", "check"], ["check", "verify"], ["verify", "ship"]]) {
        d.nodes.get(a).classList.add("on");
        await pulse(svgEl, edgeMap.get(`${a}->${b}`), 480, "");
        d.nodes.get(b).classList.add("on");
      }
      status.textContent = "Toutes les obligations sont prouvées : livraison via vg_build.";
      btnFail.disabled = btnPass.disabled = false;
    });
  });

  await section("#sec-mcp", async () => {
    const host = $("#mcp-tools");
    const cards = data.mcp.tools.map((t) => {
      const req = new Set(t.inputSchema?.required ?? []);
      const props = Object.keys(t.inputSchema?.properties ?? {});
      const list = el("ul", { class: "schema-list" },
        ...props.map((p) => el("li", {}, el("code", { class: req.has(p) ? "req" : "", text: req.has(p) ? `${p}*` : p })))
      );
      return el("div", { class: "card mcp-card" },
        el("span", { class: "label", text: `MCP · ${data.mcp.protocolVersion}` }),
        el("h3", {}, el("code", { text: t.name })),
        el("p", { text: t.description }),
        list,
      );
    });
    host.replaceChildren(...cards);

    $("#mcp-sample-json").textContent = pretty({
      request: {
        jsonrpc: "2.0",
        id: 3,
        method: "tools/call",
        params: {
          name: "vg_run",
          arguments: {
            path: "examples/quickstart.vgs",
            node: "@soft_knee",
            inputs: { x: "3e38", knee: "0.5" },
          },
        },
      },
      structuredContent: data.mcp.sample_call,
    });
  });

  await section("#sec-loop", async () => {
    const tbody = $("#loop-table");
    tbody.replaceChildren(
      ...data.loop_results.tasks.map((t) =>
        el("tr", {},
          el("td", {}, el("code", { text: t.id })),
          el("td", { text: fmt(t.rounds) }),
          el("td", { text: t.verified ? "oui" : "non" }),
          el("td", { text: t.reference_contract ? "oui" : "non" }),
          el("td", { text: t.tests_passed ? "oui" : "non" }),
          el("td", { text: t.success ? "oui" : "non" }),
        )
      )
    );

    const steps = [
      {
        cmd: "vg loop target/site/agent/tasks --replay target/site/agent/script.json -o target/site/agent/loop_out",
        out: pretty(data.loop_results),
      },
      {
        cmd: "# Tour 1 de a_solved : réponse de vg_verify renvoyée au modèle",
        out: pretty({
          ok: data.loop_first_verify.ok,
          nodes: data.loop_first_verify.nodes,
          counterexamples: data.loop_first_verify.results.filter((r) => r.verdict === "counterexample"),
        }),
      },
    ];
    terminal($("#term-loop"), steps, { title: "vg loop --replay — trois tâches évaluées par le correcteur", speed: 16 });
  });

  await section("#fig-decoding-tiers-wrap", async () => {
    const svgEl = $("#fig-decoding-tiers");
    const d = await diagram(svgEl, {
      nodes: [
        { id: "llm", label: "Modèle local", sub: "logits R-01", kind: "input" },
        { id: "t1", label: "Palier 1", sub: "grammaire GBNF/Lark", kind: "op" },
        { id: "t2", label: "Palier 2", sub: "vg-decode::Prefix (23 µs)", kind: "ok" },
        { id: "t3", label: "Palier 3", sub: "SMT aux liaisons [R-01]", kind: "warn" },
        { id: "out", label: "Module valide", sub: "vg_verify final décide", kind: "out" },
      ],
      edges: [
        ["llm", "t1"],
        ["t1", "t2"],
        ["t2", "t3"],
        ["t3", "out"],
      ],
      dx: 195,
      dy: 84,
      bw: 164,
      bh: 54,
    });
    $("#fig-decoding-tiers-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
    await drawIn(d);
  });

  await section("#sec-m1", async () => {
    const m1 = data.m1;
    $("#m1-stats").replaceChildren(
      el("div", { class: "stat" }, el("b", { text: fmt(m1.resolve.bound[0].objects) }), el("span", { text: "objets Mesh épinglés pour @sha256" })),
      el("div", { class: "stat" }, el("b", { text: `${fmt(m1.verify.proven_obligations)} / ${fmt(m1.verify.total_obligations)}` }), el("span", { text: "obligations prouvées dans audio_limiter.vgs" })),
      el("div", { class: "stat" }, el("b", { text: m1.resolve.bound[0].evidence }), el("span", { text: "niveau de preuve du SHA-256 lié" })),
    );
    $("#m1-explain").textContent = pretty(data.draft_explain.report);
  });
}

main();
