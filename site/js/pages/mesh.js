import {
  $, $$, el, fmt, getJSON, load, encode, hex, proofTag, terminal,
  section, drawIn, pulse, reduced
} from "../page.js";
import { diagram } from "../diagram.js";

const pretty = (obj) => JSON.stringify(obj, null, 2);

async function wait(ms) {
  if (reduced) return;
  return new Promise((r) => setTimeout(r, ms));
}

async function main() {
  const data = await getJSON("data/mesh.json");

  await section("#fig-mesh-pipeline-wrap", async () => {
    const svgEl = $("#fig-mesh-pipeline");
    const d = await diagram(svgEl, {
      nodes: [
        { id: "q", label: "MeshContractQuery", sub: "types · effets · POST", kind: "input" },
        { id: "spec", label: "1. Spec exacte", sub: "spec_hash (~µs)", kind: "op" },
        { id: "mask", label: "2. Filtre structurel", sub: "types & masque d'effets", kind: "op" },
        { id: "sem", label: "3. Rang sémantique", sub: "cosinus sur intent", kind: "ai" },
        { id: "beh", label: "4. Sous-typage SMT", sub: "PRE_q ⇒ PRE_c & POST", kind: "warn" },
        { id: "rank", label: "5. Coût & bench", sub: "p99 = 421 µs (G7)", kind: "ok" },
        { id: "res", label: "vg.lock", sub: "empreinte épinglée", kind: "out" },
      ],
      edges: [
        ["q", "spec"],
        ["spec", "mask"],
        ["mask", "sem"],
        ["sem", "beh"],
        ["beh", "rank"],
        ["rank", "res"],
      ],
      dx: 174,
      dy: 84,
      bw: 148,
      bh: 54,
    });
    $("#fig-mesh-pipeline-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
    await drawIn(d);
  });

  await section("#fig-merkle-log-wrap", async () => {
    const svgEl = $("#fig-merkle-log");
    const d = await diagram(svgEl, {
      nodes: [
        { id: "sub", label: "Soumission signée", sub: "6 contrôles d'ingestion", kind: "input" },
        { id: "leaf", label: "leaf_hash", sub: "SHA-256(0x00 || data)", kind: "op" },
        { id: "node", label: "node_hash", sub: "SHA-256(0x01 || L || R)", kind: "op" },
        { id: "root", label: "merkle_root", sub: "empty_root = SHA-256(\"\")", kind: "ok" },
        { id: "ckpt", label: "Checkpoint C2SP", sub: "note signée Ed25519", kind: "out" },
        { id: "tile", label: "Tuiles statiques", sub: "hauteur 8 · largeur 256", kind: "out" },
      ],
      edges: [
        ["sub", "leaf"],
        ["leaf", "node"],
        ["node", "root"],
        ["root", "ckpt"],
        ["root", "tile"],
      ],
      vertical: true,
      dx: 220,
      dy: 78,
      bw: 192,
      bh: 50,
    });
    $("#fig-merkle-log-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
    await drawIn(d);

    const edgeMap = new Map(d.edges.map((e) => [`${e.from}->${e.to}`, e]));
    const btn = $("#btn-merkle-pulse");
    const status = $("#merkle-status");
    btn.addEventListener("click", async () => {
      btn.disabled = true;
      for (const n of d.nodes.values()) n.classList.remove("on", "bad", "dim");
      const seq = [
        ["sub", "leaf", "Calcul de leaf_hash = SHA-256(0x00 || data)…"],
        ["leaf", "node", "Combinaison interne SHA-256(0x01 || left || right)…"],
        ["node", "root", "Mise à jour de la racine Merkle en ajout seul…"],
        ["root", "ckpt", "Signature du checkpoint C2SP par Ed25519."],
      ];
      for (const [a, b, msg] of seq) {
        status.textContent = msg;
        d.nodes.get(a).classList.add("on");
        await pulse(svgEl, edgeMap.get(`${a}->${b}`), 440, "");
        d.nodes.get(b).classList.add("on");
        await wait(100);
      }
      await pulse(svgEl, edgeMap.get("root->tile"), 420, "");
      d.nodes.get("tile").classList.add("on");
      btn.disabled = false;
    });
  });

  await section("#sec-pratique", async () => {
    const c = data.counts;
    $("#mesh-stats").replaceChildren(
      el("div", { class: "stat" }, el("b", { text: fmt(c.accepted) }), el("span", { text: "nœuds publiés depuis examples/sha256.vgs" })),
      el("div", { class: "stat" }, el("b", { text: fmt(c.candidates) }), el("span", { text: "candidat retourné pour spec: sha256" })),
      el("div", { class: "stat" }, el("b", { text: `${fmt(c.limiter_proven)} / ${fmt(c.limiter_obligations)}` }), el("span", { text: "obligations prouvées dans audio_limiter.vgs" })),
      el("div", { class: "stat" }, el("b", { text: data.query.candidates[0].evidence }), el("span", { text: `coût déclaré : ${data.query.candidates[0].cost}` })),
    );

    const steps = [
      {
        cmd: "vg mesh publish examples/sha256.vgs --root target/site/mesh",
        out: pretty({
          ok: data.publish.ok,
          accepted_count: data.publish.accepted.length,
          root_sha256_impl: data.publish.accepted[data.publish.accepted.length - 1],
          rejected: data.publish.rejected,
        }),
      },
      {
        cmd: "vg mesh query target/site/mesh/sha256_query.json --root target/site/mesh",
        out: pretty(data.query),
      },
      {
        cmd: "vg mesh resolve examples/audio_limiter.vgs --root target/site/mesh",
        out: pretty(data.resolve),
      },
      {
        cmd: "vg verify examples/audio_limiter.vgs --mode edit --root target/site/mesh --detail 0",
        out: pretty(data.limiter_verify),
      },
    ];
    terminal($("#term-mesh"), steps, { title: "vg mesh — publish, query, resolve, verify", speed: 16 });
    $("#vg-lock-out").textContent = data.vg_lock.trim();
  });

  await section("#demo-sha", async () => {
    const runSha = await load("wasm/sha256.wasm");
    const input = $("#sha-input");
    const got = $("#sha-got");
    const enc = new TextEncoder();

    const compute = async () => {
      const bytes = enc.encode(input.value);
      const out = await runSha(encode([["slice", bytes]]));
      got.textContent = hex(out);
    };

    $("#sha-run-btn").addEventListener("click", compute);
    input.addEventListener("input", compute);
    for (const b of $$(".sha-preset")) {
      b.addEventListener("click", () => {
        input.value = b.dataset.val ?? "";
        compute();
      });
    }
    await compute();
  });
}

main();
