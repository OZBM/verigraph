// Pure helpers of the VeriGraph extension. They read vg's JSON results (docs/15) and VG-Sym text and
// never touch the editor API, so `node --test` runs them without an editor.
'use strict';

const DEF = /^\s*(NODE|SPEC|@MESH_QUERY)\s+(@[a-z_][A-Za-z0-9_]*)/;

/** Every NODE, SPEC and @MESH_QUERY of a module, with its 0-based line. */
function definitions(text) {
  const defs = [];
  text.split('\n').forEach((line, i) => {
    const m = DEF.exec(line);
    if (m) {
      const kind = { NODE: 'node', SPEC: 'spec' }[m[1]] || 'mesh';
      defs.push({ kind, label: m[2], line: i });
    }
  });
  return defs;
}

/** The 0-based line where `label` is defined, or 0 when the file does not define it. */
function lineOf(text, label) {
  const d = definitions(text).find((x) => x.label === label);
  return d ? d.line : 0;
}

/** The node whose definition starts at or above `line`: the one the cursor is in. */
function nodeAt(text, line) {
  const above = definitions(text).filter((d) => d.kind === 'node' && d.line <= line);
  return above.length ? above[above.length - 1].label : undefined;
}

/** vg reports 1-based lines and columns, or line 0 with the node when only the node is known. */
function place(d, text) {
  if (d.line > 0) return { line: d.line - 1, character: Math.max(d.col - 1, 0) };
  return { line: d.node ? lineOf(text, d.node) : 0, character: 0 };
}

/** The `diagnostics` of any vg result, as editor-neutral diagnostics. */
function fromDiagnostics(result, text) {
  return (result.diagnostics || []).map((d) => ({
    ...place(d, text),
    severity: d.severity === 'warning' ? 'warning' : 'error',
    code: d.code,
    message: d.message,
  }));
}

const SEVERITY = { unknown: 'warning', vacuous: 'warning' };

/** One line per obligation result, as the output channel and the diagnostics print it. */
function describe(r) {
  let s = `${r.id}: ${r.verdict}${r.code ? ` (${r.code})` : ''}`;
  if (r.verdict !== 'proven' && r.message) s += `, ${r.message}`;
  const rep = r.report;
  if (rep && rep.inputs) {
    const inputs = Object.entries(rep.inputs).map(([k, v]) => `${k} = ${JSON.stringify(v)}`);
    s += `; inputs ${inputs.join(', ')}`;
    if (rep.reproduced === 'concrete') s += ', reproduced in vg-interp';
  }
  return s;
}

/** A vg verify result: its diagnostics, plus one per obligation that is not proven, on its node. */
function fromVerify(result, text) {
  const out = fromDiagnostics(result, text);
  for (const r of result.results || []) {
    if (r.verdict === 'proven') continue;
    out.push({
      line: lineOf(text, r.id.split('/')[0]),
      character: 0,
      severity: SEVERITY[r.verdict] || 'error',
      code: r.code || r.verdict,
      message: describe(r),
    });
  }
  return out;
}

/** One sentence on a vg verify result. */
function verifySummary(result) {
  const results = result.results || [];
  if (!results.length && (result.diagnostics || []).length) {
    return 'the file does not check; see the Problems panel';
  }
  const bad = results.filter((r) => r.verdict !== 'proven').length;
  return bad
    ? `${bad} of ${results.length} obligations not proven`
    : `all ${results.length} obligations proven`;
}

/** Splits on commas that are not nested in <>, (), [] or {}. */
function splitTop(s) {
  const parts = [];
  let depth = 0;
  let cur = '';
  for (const c of s) {
    if ('<([{'.includes(c)) depth++;
    if ('>)]}'.includes(c)) depth--;
    if (c === ',' && depth === 0) {
      parts.push(cur.trim());
      cur = '';
    } else {
      cur += c;
    }
  }
  if (cur.trim()) parts.push(cur.trim());
  return parts;
}

/** The parameters of a node, read from the first SIGNATURE after its definition. */
function params(text, label) {
  const def = definitions(text).find((d) => d.label === label);
  if (!def) return [];
  const lines = text.split('\n');
  for (let i = def.line; i < lines.length; i++) {
    const m = /SIGNATURE\s*:\s*\((.*)\)\s*->/.exec(lines[i]);
    if (m) {
      return splitTop(m[1]).map((p) => {
        const k = p.indexOf(':');
        return { name: p.slice(0, k).trim(), type: p.slice(k + 1).trim() };
      });
    }
  }
  return [];
}

/** A placeholder value in the JSON form of docs/15: floats and 64-bit integers are strings. */
function sample(type) {
  if (/^(f16|bf16|f32|f64)$/.test(type)) return '0.0';
  if (type === 'u64' || type === 'i64') return '0';
  if (/^[ui](8|16|32)$/.test(type)) return 0;
  if (type === 'Bool') return false;
  const arr = /^Array<(.+),\s*(\d+)>$/.exec(type);
  if (arr) return Array.from({ length: Number(arr[2]) }, () => sample(arr[1].trim()));
  if (/^(Array|Tensor|Slice)</.test(type)) return [];
  const tuple = /^Tuple<(.+)>$/.exec(type);
  if (tuple) return splitTop(tuple[1]).map(sample);
  const res = /^Result<(.+)>$/.exec(type);
  if (res) return { ok: sample(splitTop(res[1])[0]) };
  return null;
}

/** The inputs object `vg run` expects, with a placeholder per parameter; Eff is supplied by vg. */
function inputTemplate(ps) {
  const o = {};
  for (const p of ps) if (p.type !== 'Eff') o[p.name] = sample(p.type);
  return o;
}

function holds(v) {
  return v === true ? 'holds' : v === false ? 'fails' : 'not evaluated';
}

/** One line on a vg run result. */
function describeRun(r) {
  if (r.ok) return `outputs ${JSON.stringify(r.outputs)}; PRE ${holds(r.pre)}, POST ${holds(r.post)}`;
  if (r.trap) return `trap at ${r.trap.op} (${r.trap.opcode}): ${r.trap.message}; PRE ${holds(r.pre)}`;
  if (r.error) return r.error;
  return (r.diagnostics || []).map((d) => `${d.code}: ${d.message}`).join('; ');
}

/** The lines of a vg build result. */
function describeBuild(r) {
  if (!r.ok) {
    if (r.error) return [r.error];
    return (r.diagnostics || []).map((d) => `${d.code}: ${d.message}`);
  }
  const lines = r.artifacts.map((a) => {
    const kind = a.linked ? 'executable' : 'object file';
    const guards = a.residual_guards ? `, ${a.residual_guards} runtime guards` : '';
    return `${a.target}: ${a.path} (${kind}, ${a.bytes} bytes${guards})`;
  });
  if (!r.verified) lines.push('not fully proven: built with a runtime check on every partial op');
  return lines;
}

module.exports = {
  definitions,
  lineOf,
  nodeAt,
  fromDiagnostics,
  fromVerify,
  describe,
  verifySummary,
  splitTop,
  params,
  inputTemplate,
  describeRun,
  describeBuild,
};
