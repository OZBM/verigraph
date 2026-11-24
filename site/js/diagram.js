import { load, encode, f32s } from "./vg.js";

const SVG_NS = "http://www.w3.org/2000/svg";
let runnerPromise = null;

function getRunner() {
  if (!runnerPromise) {
    runnerPromise = load(new URL("../wasm/layout.wasm", import.meta.url)).catch((err) => {
      runnerPromise = null;
      throw err;
    });
  }
  return runnerPromise;
}

function el(doc, tag, attrs = {}, text = null) {
  const node = doc.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (v !== undefined && v !== null) node.setAttribute(k, String(v));
  }
  if (text !== null) node.textContent = text;
  return node;
}

export async function diagram(svg, spec) {
  if (!svg) throw new Error("diagram: svg element is required");
  if (!spec || !Array.isArray(spec.nodes)) throw new Error("diagram: spec.nodes must be an array");

  const nodes = spec.nodes;
  const fwd = spec.edges ?? [];
  const back = spec.back ?? [];
  if (nodes.length < 1 || nodes.length > 32) {
    throw new Error(`diagram: expected 1..32 nodes, got ${nodes.length}`);
  }
  if (fwd.length + back.length > 64) {
    throw new Error(`diagram: expected at most 64 edges, got ${fwd.length + back.length}`);
  }

  const indexOf = new Map();
  const key = new Array(32).fill(0);
  const minl = new Array(32).fill(0);
  nodes.forEach((node, i) => {
    if (!node || node.id === undefined || node.id === null) {
      throw new Error(`diagram: node at index ${i} is missing an id`);
    }
    if (indexOf.has(node.id)) {
      throw new Error(`diagram: duplicate node id "${node.id}"`);
    }
    indexOf.set(node.id, i);
    key[i] = node.key ?? i;
    minl[i] = node.min ?? 0;
  });

  const es = new Array(64).fill(0xffffffff);
  const edgeMeta = [];
  const packEdge = ([fromId, toId], isBack) => {
    if (!indexOf.has(fromId)) throw new Error(`diagram: unknown source node id "${fromId}"`);
    if (!indexOf.has(toId)) throw new Error(`diagram: unknown target node id "${toId}"`);
    const slot = edgeMeta.length;
    const src = indexOf.get(fromId);
    const dst = indexOf.get(toId);
    es[slot] = ((isBack ? 1 : 0) << 16) | (src << 8) | dst;
    edgeMeta.push({ from: fromId, to: toId, back: isBack, slot });
  };
  for (const edge of fwd) packEdge(edge, false);
  for (const edge of back) packEdge(edge, true);

  const dx = spec.dx ?? 200;
  const dy = spec.dy ?? 84;
  const bw = spec.bw ?? 156;
  const bh = spec.bh ?? 52;
  const vert = Boolean(spec.vertical);

  const input = encode([
    ["u32", nodes.length],
    ["u32", es],
    ["f32", key],
    ["u32", minl],
    ["f32", [dx, dy, bw, bh]],
    ["bool", vert],
  ]);

  const runLayout = await getRunner();
  const out = await runLayout(input);

  const pos = f32s(out, 0, 64);
  const paths = f32s(out, 256, 512);
  const size = f32s(out, 2304, 2);
  const [W, H] = size;

  const doc = svg.ownerDocument;
  svg.replaceChildren();
  svg.setAttribute("viewBox", `0 0 ${W.toFixed(2)} ${H.toFixed(2)}`);

  const edgesGroup = el(doc, "g", { class: "dg-edges" });
  const nodesGroup = el(doc, "g", { class: "dg-nodes" });
  svg.appendChild(edgesGroup);
  svg.appendChild(nodesGroup);

  const edgesOut = edgeMeta.map(({ from, to, back: isBack, slot }) => {
    const base = slot * 8;
    const f = (offset) => paths[base + offset].toFixed(2);
    const d = `M ${f(0)} ${f(1)} C ${f(2)} ${f(3)}, ${f(4)} ${f(5)}, ${f(6)} ${f(7)}`;
    const pathEl = el(doc, "path", {
      class: isBack ? "dg-edge dg-back" : "dg-edge",
      d,
      pathLength: "1",
      "data-from": from,
      "data-to": to,
    });
    edgesGroup.appendChild(pathEl);
    return { from, to, back: isBack, el: pathEl, d };
  });

  const nodesOut = new Map();
  nodes.forEach((node, i) => {
    const label = node.label ?? String(node.id);
    const hasSub = node.sub !== undefined && node.sub !== null && node.sub !== "";
    const cls = node.kind ? `dg-node dg-${node.kind}` : "dg-node";
    const nodeSvg = el(doc, "svg", {
      class: cls,
      "data-id": node.id,
      x: pos[2 * i].toFixed(2),
      y: pos[2 * i + 1].toFixed(2),
      width: bw,
      height: bh,
      overflow: "visible",
    });
    nodeSvg.appendChild(el(doc, "title", {}, hasSub ? `${label} — ${node.sub}` : String(label)));
    nodeSvg.appendChild(el(doc, "rect", { width: "100%", height: "100%", rx: "10" }));
    nodeSvg.appendChild(
      el(
        doc,
        "text",
        {
          class: "dg-label",
          x: "50%",
          y: hasSub ? "38%" : "50%",
          "text-anchor": "middle",
          "dominant-baseline": "central",
        },
        String(label)
      )
    );
    if (hasSub) {
      nodeSvg.appendChild(
        el(
          doc,
          "text",
          {
            class: "dg-sub",
            x: "50%",
            y: "70%",
            "text-anchor": "middle",
            "dominant-baseline": "central",
          },
          String(node.sub)
        )
      );
    }
    nodesGroup.appendChild(nodeSvg);
    nodesOut.set(node.id, nodeSvg);
  });

  return { nodes: nodesOut, edges: edgesOut, size };
}
