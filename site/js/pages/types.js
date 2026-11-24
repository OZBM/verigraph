import {
  $,
  el,
  fmt,
  getJSON,
  load,
  encode,
  f32s,
  section,
  proofTag,
  highlight,
  onFirstView,
  drawIn,
  pulse,
} from "../page.js";
import { diagram } from "../diagram.js";

section("#fig-eff", async () => {
  const svgEl = $("#fig-eff");
  const d = await diagram(svgEl, {
    nodes: [
      { id: "e0", label: "e0 : Eff", sub: "jeton d'entrée", kind: "input", min: 0 },
      { id: "fin", label: "fin : FileIn", sub: "poignée lecture", kind: "input", min: 0 },
      { id: "read", label: "@read_header", sub: "FILE_READ(e0, fin)", kind: "op" },
      { id: "fout", label: "fout : FileOut", sub: "poignée écriture", kind: "input", min: 1 },
      { id: "slice", label: "%905 : Slice", sub: "[65, 66]", kind: "op", min: 1 },
      { id: "write", label: "@write_payload", sub: "FILE_WRITE(%901, fout)", kind: "op" },
      { id: "ret", label: "RETURN(%902, %904)", sub: "jeton final + Result", kind: "out" },
    ],
    edges: [
      ["e0", "read"],
      ["fin", "read"],
      ["read", "write"],
      ["fout", "write"],
      ["slice", "write"],
      ["write", "ret"],
    ],
    dx: 196,
    dy: 78,
    bw: 158,
    bh: 52,
  });

  const effPath = [
    ["e0", "read"],
    ["read", "write"],
    ["write", "ret"],
  ];

  const playEff = async () => {
    for (const n of d.nodes.values()) n.classList.remove("on");
    for (const e of d.edges) e.el.classList.remove("on");
    d.nodes.get("e0").classList.add("on");
    for (const [a, b] of effPath) {
      const edge = d.edges.find((e) => e.from === a && e.to === b);
      edge.el.classList.add("on");
      await pulse(svgEl, edge, 520, "");
      d.nodes.get(b).classList.add("on");
    }
  };

  $("#btn-eff").addEventListener("click", playEff);
  onFirstView(svgEl, async () => {
    await drawIn(d);
    playEff();
  });
  $("#fig-eff-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
});

section("#fig-sandbox", async () => {
  const svgEl = $("#fig-sandbox");
  const d = await diagram(svgEl, {
    nodes: [
      { id: "man", label: "manifest.toml", sub: "liaisons argv", kind: "input" },
      { id: "open", label: "vg-rt open()", sub: "descripteurs seuls", kind: "op" },
      { id: "sb", label: "vg_rt_sandbox()", sub: "sandbox_init / Landlock", kind: "warn" },
      { id: "root", label: "@main(Eff, …)", sub: "nœud racine vérifié", kind: "ok" },
      { id: "deny", label: "autre open/socket", sub: "refusé par l'OS (EPERM)", kind: "out" },
    ],
    edges: [
      ["man", "open"],
      ["open", "sb"],
      ["sb", "root"],
      ["sb", "deny"],
    ],
    dx: 198,
    dy: 84,
    bw: 162,
    bh: 52,
  });

  onFirstView(svgEl, () => drawIn(d));
  $("#fig-sb-proof").innerHTML = await proofTag("@layout", "chaque boîte reste dans le cadre");
});

section("#fig-err-lab", async () => {
  const data = await getJSON("data/types.json");
  $("#err-count").textContent = fmt(data.errors_corpus_count);

  const meta = {
    t0103: {
      tab: "1 · LT.u sur i32",
      answer: 0,
      choices: [
        "VG-T0103 — le mode .u ne correspond pas au type i32 des opérandes",
        "VG-S0002 — le port %0 est utilisé avant sa définition",
        "VG-E0101 — il manque un bit d'effet dans EFFECTS",
      ],
      explain: "LT.u demande des entiers non signés, alors que a et b sont de type i32 (signé).",
      fix: "Correction : remplacer LT.u(a, b) par LT.s(a, b).",
    },
    t0115: {
      tab: "2 · OK sans annotation",
      answer: 1,
      choices: [
        "VG-T0108 — RETURN ne correspond pas à SIGNATURE",
        "VG-T0115 — OK exige une annotation de type sur son port de destination",
        "VG-P0003 — le littéral x ne tient pas dans u32",
      ],
      explain: "OK(x) connaît T = u32 mais ne peut pas deviner le type d'erreur E de Result<u32, u8> (ADR 0004).",
      fix: "Correction : écrire %0: Result<u32, u8> = OK(x);",
    },
    e0101: {
      tab: "3 · CLOCK_READ en nœud pur",
      answer: 2,
      choices: [
        "VG-S0103 — mauvais nombre d'arguments pour CLOCK_READ",
        "VG-T0101 — Clock doit précéder Eff dans SIGNATURE",
        "VG-E0101 / VG-E0104 — un nœud avec EFFECTS: [] ne peut ni recevoir Eff/Clock ni exécuter CLOCK_READ",
      ],
      explain: "EFFECTS: [] déclare un nœud pur : vg-types signale à la fois la poignée Clock sans bit (VG-E0104), Eff interdit en nœud pur (VG-T0119) et CLOCK_READ sans effet CLOCK (VG-E0101).",
      fix: "Correction : déclarer EFFECTS: [CLOCK];",
    },
    e0102: {
      tab: "4 · Eff utilisé deux fois",
      answer: 0,
      choices: [
        "VG-E0102 — le jeton linéaire e est consommé deux fois au lieu d'être chaîné",
        "VG-E0106 — CLOCK_READ exige aussi le bit ARENA",
        "VG-T0119 — Clock est utilisé dans deux opérations",
      ],
      explain: "Les deux CLOCK_READ lisent le même jeton e, alors que Eff est linéaire et doit être utilisé exactement une fois.",
      fix: "Correction : passer %0 (le jeton retourné par le premier CLOCK_READ) au second appel : CLOCK_READ(%0, c).",
    },
    e0104: {
      tab: "5 · FileIn sans FILE_READ",
      answer: 1,
      choices: [
        "VG-T0108 — 0:u32 n'est pas valide dans RETURN",
        "VG-E0104 — une poignée FileIn en paramètre impose FILE_READ dans EFFECTS",
        "VG-E0103 — Eff doit être le second paramètre",
      ],
      explain: "Même sans appeler FILE_READ dans le corps, déclarer une poignée FileIn sans le bit FILE_READ est une erreur de capacité.",
      fix: "Correction : inclure FILE_READ dans EFFECTS ou retirer le paramètre fin.",
    },
    t0120: {
      tab: "6 · Vue sur tableau local",
      answer: 2,
      choices: [
        "VG-T0112 — les bornes de SLICE doivent être i64",
        "VG-E0106 — UPDATE d'un Array exige ARENA",
        "VG-T0120 — la tranche retournée pointe vers %0, qui vit dans la trame locale du nœud",
      ],
      explain: "%0 est un Array<u32, 4> local qui disparaît au retour du nœud ; en retourner une vue SLICE produirait une référence invalide (ADR 0003).",
      fix: "Correction : retourner directement un type à taille fixe (Array<u32, 2> ou Array<u32, 4>), ou accepter un Slice<u32> en entrée.",
    },
  };

  const tabsEl = $("#fig-err-lab .err-tabs");
  const codeEl = $("#err-code");
  const choicesEl = $("#err-choices");
  const diagBox = $("#err-diag");
  const verdictEl = $("#err-verdict");
  const explainEl = $("#err-explain");
  const jsonEl = $("#err-json");
  const fixEl = $("#err-fix");

  const buttons = data.exercises.map((ex) => {
    const b = el("button", { class: "btn small", type: "button", text: meta[ex.id].tab, "data-id": ex.id });
    tabsEl.append(b);
    return b;
  });

  const showExercise = (id) => {
    const ex = data.exercises.find((x) => x.id === id) ?? data.exercises[0];
    const m = meta[ex.id];
    buttons.forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.id === ex.id)));
    codeEl.setAttribute("data-file", ex.file);
    codeEl.replaceChildren(el("code", { text: ex.source_without_expect }));
    highlight(codeEl);

    diagBox.hidden = true;
    choicesEl.replaceChildren();
    const choiceButtons = m.choices.map((label, idx) => {
      const btn = el("button", { class: "choice", type: "button", text: label });
      btn.addEventListener("click", () => {
        choiceButtons.forEach((cb, j) => {
          cb.classList.remove("right", "wrong");
          if (j === m.answer) cb.classList.add("right");
        });
        if (idx !== m.answer) btn.classList.add("wrong");
        verdictEl.textContent = idx === m.answer ? "Exact · sortie réelle de vg check" : "Pas tout à fait · voici la sortie réelle de vg check";
        explainEl.textContent = m.explain;
        jsonEl.textContent = JSON.stringify(ex.check, null, 2);
        fixEl.textContent = m.fix;
        diagBox.hidden = false;
      });
      choicesEl.append(el("li", {}, btn));
      return btn;
    });
  };

  buttons.forEach((b) => b.addEventListener("click", () => showExercise(b.dataset.id)));
  showExercise(data.exercises[0].id);
});

section("#entry-demo", async () => {
  const runZoom = await load("wasm/zoom.wasm");
  const cxEl = $("#in-cx");
  const cyEl = $("#in-cy");
  const scaleEl = $("#in-scale");
  const pxEl = $("#in-px");
  const pyEl = $("#in-py");
  const zinEl = $("#in-zin");
  const outEl = $("#zoom-out");

  const execZoom = async () => {
    const cx = Number(cxEl.value);
    const cy = Number(cyEl.value);
    const scale = Number(scaleEl.value);
    const px = Number(pxEl.value);
    const py = Number(pyEl.value);
    const zin = zinEl.value === "1";

    outEl.className = "entry-out";
    if (!Number.isFinite(cx) || !Number.isFinite(cy) || !Number.isFinite(scale) || !Number.isInteger(px) || !Number.isInteger(py) || px < 0 || py < 0) {
      outEl.classList.add("is-fail");
      outEl.textContent = "Entrée non encodable en f32 / u32 : saisissez des nombres valides et des pixels entiers >= 0.";
      return;
    }

    try {
      const bytes = await runZoom(
        encode([
          ["f32", [cx, cy, scale]],
          ["u32", [px, py]],
          ["bool", zin],
        ])
      );
      const [nx, ny, ns] = f32s(bytes, 0, 3);
      outEl.classList.add("is-ok");
      outEl.textContent = `OK (code 0) · @zoom -> Tuple(${nx}, ${ny}, ${ns})`;
    } catch (e) {
      outEl.classList.add("is-fail");
      outEl.textContent = `PRE refusée à la frontière de confiance · ${e.message.trim()}`;
    }
  };

  const applyPreset = (vals) => {
    cxEl.value = vals.cx;
    cyEl.value = vals.cy;
    scaleEl.value = vals.scale;
    pxEl.value = vals.px;
    pyEl.value = vals.py;
    zinEl.value = vals.zin;
    execZoom();
  };

  $("#pre-ok").addEventListener("click", () => applyPreset({ cx: "-0.5", cy: "0", scale: "0.0125", px: "128", py: "96", zin: "1" }));
  $("#pre-scale").addEventListener("click", () => applyPreset({ cx: "-0.5", cy: "0", scale: "0.05", px: "1", py: "1", zin: "1" }));
  $("#pre-px").addEventListener("click", () => applyPreset({ cx: "-0.5", cy: "0", scale: "0.0125", px: "300", py: "96", zin: "1" }));
  $("#run-zoom").addEventListener("click", execZoom);

  $("#fig-zoom-proof").innerHTML = await proofTag("@zoom", "PRE contrôlée à la frontière de confiance");
});
