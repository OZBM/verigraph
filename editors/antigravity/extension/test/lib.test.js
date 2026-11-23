// Unit tests of lib.js: node --test editors/antigravity/extension/test
// The JSON results below were printed by vg 0.0.0 at commit f062076 (trimmed to the fields used).
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const lib = require('../lib');

const DRAFT = `module soft_knee_draft;

SPEC @limit {
  SIGNATURE: (x: f32) -> (f32);
  EFFECTS: [];
  CONTRACT {}
  BODY { RETURN(x); }
}

NODE @soft_knee [intent: "Soft-knee limiter"] {
  SIGNATURE: (x: f32, knee: f32) -> (f32);
  EFFECTS: [];
  CONTRACT {
    POST: result <= 1.0;
  }
  BODY { RETURN(x); }
}

NODE @frame {
  SIGNATURE: (io: Eff, n: u64, xs: Array<f32, 3>, flag: Bool, k: Tuple<u8, i64>, r: Result<f64, u32>, h: FileIn) -> (Eff, f32);
  EFFECTS: [];
  CONTRACT {}
  BODY { RETURN(io, 1.0:f32); }
}
`;

test('definitions finds nodes and specs with their lines', () => {
  assert.deepEqual(lib.definitions(DRAFT), [
    { kind: 'spec', label: '@limit', line: 2 },
    { kind: 'node', label: '@soft_knee', line: 9 },
    { kind: 'node', label: '@frame', line: 18 },
  ]);
  assert.equal(lib.nodeAt(DRAFT, 14), '@soft_knee');
  assert.equal(lib.nodeAt(DRAFT, 20), '@frame');
  assert.equal(lib.nodeAt(DRAFT, 1), undefined);
});

test('a parse error keeps its place, 1-based in vg and 0-based in the editor', () => {
  const r = {
    ok: false,
    diagnostics: [
      { code: 'VG-P0001', col: 3, line: 4, message: 'expected `;`, found `EFFECTS`', node: null, op: null, severity: 'error' },
    ],
  };
  assert.deepEqual(lib.fromDiagnostics(r, DRAFT), [
    { line: 3, character: 2, severity: 'error', code: 'VG-P0001', message: 'expected `;`, found `EFFECTS`' },
  ]);
});

test('a type error without a position goes to the line of its node', () => {
  const r = {
    ok: false,
    diagnostics: [
      { code: 'VG-C0106', col: 0, line: 0, message: 'latency_p99_us is a measured resource for BUDGET, not COST', node: '@soft_knee', op: null, severity: 'error' },
      { code: 'VG-W0001', col: 0, line: 0, message: 'w', node: null, op: null, severity: 'warning' },
    ],
  };
  const d = lib.fromDiagnostics(r, DRAFT);
  assert.equal(d[0].line, 9);
  assert.equal(d[0].character, 0);
  assert.equal(d[1].line, 0);
  assert.equal(d[1].severity, 'warning');
});

const VERIFY = {
  ok: false,
  nodes: [{ label: '@soft_knee', verified: false }],
  results: [
    { id: '@soft_knee/NONVACUOUS', message: 'proven', solver: 'z3', verdict: 'proven' },
    {
      code: 'VG-V0001',
      id: '@soft_knee/POST.0',
      message: "refuted by the solver's model",
      report: { inputs: { knee: '0.6191296', x: '-2.510954e38' }, node: '@soft_knee', obligation: '@soft_knee/POST.0', reproduced: 'concrete' },
      solver: 'z3',
      verdict: 'counterexample',
    },
    { code: 'VG-V0004', id: '@soft_knee/POST.1', message: 'timeout', solver: 'z3', verdict: 'unknown' },
  ],
};

test('each obligation that is not proven becomes a diagnostic on its node', () => {
  const d = lib.fromVerify(VERIFY, DRAFT);
  assert.equal(d.length, 2);
  assert.deepEqual(d[0], {
    line: 9,
    character: 0,
    severity: 'error',
    code: 'VG-V0001',
    message:
      "@soft_knee/POST.0: counterexample (VG-V0001), refuted by the solver's model; " +
      'inputs knee = "0.6191296", x = "-2.510954e38", reproduced in vg-interp',
  });
  assert.equal(d[1].severity, 'warning');
  assert.equal(d[1].code, 'VG-V0004');
  assert.equal(lib.describe(VERIFY.results[0]), '@soft_knee/NONVACUOUS: proven');
});

test('verifySummary counts what is not proven and spots a file that does not check', () => {
  assert.equal(lib.verifySummary(VERIFY), '2 of 3 obligations not proven');
  assert.equal(lib.verifySummary({ ok: true, results: [VERIFY.results[0]] }), 'all 1 obligations proven');
  const broken = { ok: false, diagnostics: [{ code: 'VG-P0001', line: 1, col: 1, message: 'x' }] };
  assert.equal(lib.verifySummary(broken), 'the file does not check; see the Problems panel');
});

test('params reads the signature, nested types included', () => {
  assert.deepEqual(lib.params(DRAFT, '@soft_knee'), [
    { name: 'x', type: 'f32' },
    { name: 'knee', type: 'f32' },
  ]);
  assert.deepEqual(lib.splitTop('a: Tuple<u8, u16>, b: {v: u32 | f(v, 1)}'), ['a: Tuple<u8, u16>', 'b: {v: u32 | f(v, 1)}']);
  assert.deepEqual(lib.params(DRAFT, '@missing').length, 0);
});

test('inputTemplate follows the JSON value forms of docs/15 and leaves Eff to vg', () => {
  assert.deepEqual(lib.inputTemplate(lib.params(DRAFT, '@soft_knee')), { x: '0.0', knee: '0.0' });
  assert.deepEqual(lib.inputTemplate(lib.params(DRAFT, '@frame')), {
    n: '0',
    xs: ['0.0', '0.0', '0.0'],
    flag: false,
    k: [0, '0'],
    r: { ok: '0.0' },
    h: null,
  });
});

test('describeRun covers outputs, traps and errors', () => {
  assert.equal(
    lib.describeRun({ ok: true, outputs: ['1.0'], post: true, pre: true }),
    'outputs ["1.0"]; PRE holds, POST holds',
  );
  assert.equal(
    lib.describeRun({ ok: false, pre: true, trap: { op: 3, opcode: 'DIV.checked', message: 'division by zero' } }),
    'trap at 3 (DIV.checked): division by zero; PRE holds',
  );
  assert.equal(lib.describeRun({ ok: false, error: 'missing input knee' }), 'missing input knee');
  assert.equal(lib.describeRun({ ok: true, outputs: [], pre: null, post: false }), 'outputs []; PRE not evaluated, POST fails');
});

test('describeBuild lists artifacts and says when a build is not proven', () => {
  const ok = {
    ok: true,
    verified: true,
    artifacts: [
      { blake3: '7e26', bytes: 37120, linked: true, path: '/out/soft_knee-aarch64-apple-darwin', residual_guards: 0, target: 'aarch64-apple-darwin' },
      { blake3: '00aa', bytes: 900, linked: false, path: '/out/soft_knee-wasm32-wasip1', residual_guards: 2, target: 'wasm32-wasip1' },
    ],
  };
  assert.deepEqual(lib.describeBuild(ok), [
    'aarch64-apple-darwin: /out/soft_knee-aarch64-apple-darwin (executable, 37120 bytes)',
    'wasm32-wasip1: /out/soft_knee-wasm32-wasip1 (object file, 900 bytes, 2 runtime guards)',
  ]);
  assert.equal(lib.describeBuild({ ...ok, verified: false }).pop(), 'not fully proven: built with a runtime check on every partial op');
  assert.deepEqual(lib.describeBuild({ ok: false, error: 'unknown target x' }), ['unknown target x']);
});
