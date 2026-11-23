// Tests of syntaxes/vgsym.tmLanguage.json. The grammar has only `match` rules, so a small tokenizer
// that applies TextMate's rule (earliest match wins, ties go to the first pattern) reproduces it.
'use strict';

const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('fs');
const path = require('path');

const grammar = JSON.parse(fs.readFileSync(path.join(__dirname, '../syntaxes/vgsym.tmLanguage.json'), 'utf8'));

function flatten(patterns) {
  return patterns.flatMap((p) => {
    if (p.include) return flatten([grammar.repository[p.include.slice(1)]]);
    if (p.patterns) return flatten(p.patterns);
    return [{ ...p, re: new RegExp(p.match, 'g') }];
  });
}

const RULES = flatten(grammar.patterns);

/** [text, scope] pairs of one line, whitespace and punctuation without a scope left out. */
function tokenize(line) {
  const out = [];
  let pos = 0;
  while (pos < line.length) {
    let best = null;
    for (const r of RULES) {
      r.re.lastIndex = pos;
      const m = r.re.exec(line);
      if (m && m[0] && (!best || m.index < best.m.index)) best = { r, m };
    }
    if (!best) break;
    const { r, m } = best;
    if (r.captures) {
      m.slice(1).forEach((g, i) => g && r.captures[i + 1] && out.push([g, r.captures[i + 1].name]));
    } else {
      out.push([m[0], r.name]);
    }
    pos = m.index + m[0].length;
  }
  return out;
}

const scope = (line, text) => (tokenize(line).find(([t]) => t === text) || [])[1];

test('every rule compiles', () => {
  assert.ok(RULES.length >= 12);
});

test('declarations: keywords, labels, attributes, types', () => {
  const line = 'NODE @soft_knee [intent: "Soft-knee limiter"] {';
  assert.equal(scope(line, 'NODE'), 'keyword.control.vgsym');
  assert.equal(scope(line, '@soft_knee'), 'entity.name.function.vgsym');
  assert.equal(scope(line, 'intent'), 'keyword.other.vgsym');
  assert.equal(scope(line, '"Soft-knee limiter"'), 'string.quoted.double.vgsym');
  const sig = '  SIGNATURE: (x: f32, xs: Slice<u8>) -> (f32);';
  assert.equal(scope(sig, 'SIGNATURE'), 'keyword.control.vgsym');
  assert.equal(scope(sig, 'Slice'), 'storage.type.vgsym');
  assert.equal(scope(sig, 'u8'), 'storage.type.vgsym');
  assert.equal(scope(sig, '->'), 'keyword.operator.vgsym');
});

test('statements: ports, opcodes with modes, typed literals', () => {
  const line = '    %2: u32 = ADD.checked(acc, 1:u32);';
  assert.equal(scope(line, '%2'), 'variable.other.port.vgsym');
  assert.equal(scope(line, 'ADD'), 'support.function.op.vgsym');
  assert.equal(scope(line, 'checked'), 'support.constant.mode.vgsym');
  assert.equal(scope(line, '1'), 'constant.numeric.vgsym');
  const lit = '    %4 = FMIN(%3, -2.5e3:f32); // clamp';
  assert.equal(scope(lit, '-2.5e3'), 'constant.numeric.vgsym');
  assert.equal(scope(lit, 'f32'), 'storage.type.vgsym');
  assert.equal(scope(lit, '// clamp'), 'comment.line.double-slash.vgsym');
  assert.equal(scope('    RETURN(%2);', 'RETURN'), 'keyword.control.vgsym');
});

test('contracts: clause keywords, spec and fold calls, effects', () => {
  const post = '    POST: forall(i in 0..n) { result >= spec:clamp(x, 0.0) } ==> true;';
  assert.equal(scope(post, 'POST'), 'keyword.other.contract.vgsym');
  assert.equal(scope(post, 'forall'), 'keyword.other.vgsym');
  assert.equal(scope(post, 'spec'), 'keyword.other.vgsym');
  assert.equal(scope(post, '..'), 'keyword.operator.vgsym');
  assert.equal(scope(post, '==>'), 'keyword.operator.vgsym');
  assert.equal(scope(post, 'true'), 'constant.language.vgsym');
  assert.equal(scope('  EFFECTS: [FILE_READ, CLOCK];', 'FILE_READ'), 'constant.other.effect.vgsym');
  assert.equal(scope('    INVARIANT @inv(k, acc): acc <= k;', 'INVARIANT'), 'keyword.other.contract.vgsym');
});

test('in the shipped examples, every uppercase word, label and port has a scope', () => {
  const root = path.join(__dirname, '../../../..');
  for (const f of ['examples/quickstart.vgs', 'examples/soft_knee_draft.vgs']) {
    for (const line of fs.readFileSync(path.join(root, f), 'utf8').split('\n')) {
      const code = line.replace(/\/\/.*$/, '').replace(/"(?:[^"\\]|\\.)*"/g, '');
      const words = code.match(/(?<![A-Za-z0-9_])[A-Z@%][A-Za-z0-9_]*/g) || [];
      const covered = tokenize(line).map(([t]) => t).join(' ');
      for (const w of words) assert.ok(covered.includes(w), `${f}: ${w} has no scope in ${line}`);
    }
  }
});
