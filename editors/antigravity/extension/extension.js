// VeriGraph for Antigravity IDE and other VS Code-compatible editors: runs the vg toolchain on .vgs
// files and shows its results as diagnostics, output lines and notifications.
'use strict';

const vscode = require('vscode');
const { execFile } = require('child_process');
const fs = require('fs');
const os = require('os');
const path = require('path');
const lib = require('./lib');

const TARGETS = ['aarch64-apple-darwin', 'x86_64-unknown-linux-gnu', 'wasm32-wasip1'];
const UNVERIFIED = 'Build with runtime checks';
let diagnostics;
let output;

function activate(context) {
  diagnostics = vscode.languages.createDiagnosticCollection('verigraph');
  output = vscode.window.createOutputChannel('VeriGraph');
  const command = (name, fn) => vscode.commands.registerCommand(name, () => onEditor(fn));
  context.subscriptions.push(
    diagnostics,
    output,
    command('verigraph.check', check),
    command('verigraph.verify', verify),
    command('verigraph.run', run),
    command('verigraph.build', build),
    vscode.workspace.onDidOpenTextDocument((d) => isVgs(d) && check(d)),
    vscode.workspace.onDidSaveTextDocument(onSave),
    vscode.workspace.onDidCloseTextDocument((d) => diagnostics.delete(d.uri)),
  );
  vscode.workspace.textDocuments.filter(isVgs).forEach(check);
}

function deactivate() {}

function isVgs(doc) {
  return doc.languageId === 'vgsym' && doc.uri.scheme === 'file';
}

function config() {
  return vscode.workspace.getConfiguration('verigraph');
}

function onSave(doc) {
  if (!isVgs(doc)) return;
  if (config().get('verifyOnSave')) verify(doc);
  else if (config().get('checkOnSave')) check(doc);
}

/** The vg executable: the setting, else the one `cargo install` put in ~/.cargo/bin, else PATH. */
function vgPath() {
  const set = config().get('vgPath');
  if (set) return set;
  const installed = path.join(process.env.CARGO_HOME || path.join(os.homedir(), '.cargo'), 'bin', 'vg');
  return fs.existsSync(installed) ? installed : 'vg';
}

/** Runs vg and parses its JSON; vg exits 1 with a JSON result when `ok` is false. */
function vg(args, doc) {
  return new Promise((resolve, reject) => {
    const opts = { cwd: path.dirname(doc.fileName), maxBuffer: 64 << 20 };
    execFile(vgPath(), args, opts, (err, stdout, stderr) => {
      try {
        resolve(JSON.parse(stdout));
      } catch {
        reject(new Error((stderr || (err && err.message) || 'vg printed no JSON').trim()));
      }
    });
  });
}

function progress(title, task) {
  return vscode.window.withProgress(
    { location: vscode.ProgressLocation.Notification, title: `VeriGraph: ${title}` },
    task,
  );
}

function fail(e) {
  output.appendLine(`error: ${e.message}`);
  const hint = /ENOENT/.test(e.message)
    ? ' Install vg with editors/antigravity/install.sh or set verigraph.vgPath.'
    : '';
  vscode.window.showErrorMessage(`VeriGraph: ${e.message}.${hint}`);
}

/** Runs a command on the active .vgs editor, saved first since vg reads the file. */
async function onEditor(fn) {
  const editor = vscode.window.activeTextEditor;
  if (!editor || !isVgs(editor.document)) {
    vscode.window.showWarningMessage('VeriGraph: open a .vgs file first.');
    return;
  }
  if (editor.document.isDirty) await editor.document.save();
  try {
    await fn(editor.document, editor);
  } catch (e) {
    fail(e);
  }
}

function show(doc, items) {
  diagnostics.set(
    doc.uri,
    items.map((d) => {
      const line = doc.lineAt(Math.min(d.line, doc.lineCount - 1));
      const start = new vscode.Position(line.lineNumber, Math.min(d.character, line.text.length));
      const severity =
        d.severity === 'warning' ? vscode.DiagnosticSeverity.Warning : vscode.DiagnosticSeverity.Error;
      const diag = new vscode.Diagnostic(new vscode.Range(start, line.range.end), d.message, severity);
      diag.code = d.code;
      diag.source = 'vg';
      return diag;
    }),
  );
}

async function check(doc) {
  try {
    show(doc, lib.fromDiagnostics(await vg(['check', doc.fileName], doc), doc.getText()));
  } catch (e) {
    fail(e);
  }
}

async function verify(doc) {
  const timeout = String(config().get('verifyTimeoutMs'));
  const name = path.basename(doc.fileName);
  try {
    const r = await progress(`verifying ${name}`, () => vg(['verify', doc.fileName, '--timeout', timeout], doc));
    show(doc, lib.fromVerify(r, doc.getText()));
    output.appendLine(`verify ${doc.fileName}`);
    for (const x of r.results || []) output.appendLine(`  ${lib.describe(x)}`);
    report(r.ok, `${name}: ${lib.verifySummary(r)}.`);
  } catch (e) {
    fail(e);
  }
}

function report(ok, message) {
  if (ok) {
    vscode.window.showInformationMessage(`VeriGraph ${message}`);
    return;
  }
  vscode.window.showWarningMessage(`VeriGraph ${message}`, 'Show output').then((c) => c && output.show(true));
}

/** Asks for a node, the one under the cursor first. */
async function pickNode(doc, editor) {
  const text = doc.getText();
  const nodes = lib.definitions(text).filter((d) => d.kind === 'node').map((d) => d.label);
  if (!nodes.length) throw new Error('this file has no NODE');
  const here = editor && lib.nodeAt(text, editor.selection.active.line);
  const items = here ? [here, ...nodes.filter((n) => n !== here)] : nodes;
  return items.length === 1 ? items[0] : vscode.window.showQuickPick(items, { title: 'VeriGraph: node' });
}

async function run(doc, editor) {
  const label = await pickNode(doc, editor);
  if (!label) return;
  const template = JSON.stringify(lib.inputTemplate(lib.params(doc.getText(), label)));
  const inputs = await vscode.window.showInputBox({
    title: `VeriGraph: inputs of ${label}`,
    prompt: 'A JSON object mapping parameter names to values; floats and 64-bit integers are strings.',
    value: template,
    validateInput: (s) => {
      try {
        JSON.parse(s);
        return null;
      } catch {
        return 'not valid JSON';
      }
    },
  });
  if (inputs === undefined) return;
  const r = await vg(['run', doc.fileName, label, inputs], doc);
  const line = lib.describeRun(r);
  output.appendLine(`run ${label} ${inputs}`);
  output.appendLine(`  ${line}`);
  report(r.ok && r.pre !== false && r.post !== false, `${label}: ${line}`);
}

async function build(doc, editor) {
  const label = await pickNode(doc, editor);
  if (!label) return;
  const picked = await vscode.window.showQuickPick(
    TARGETS.map((t, i) => ({ label: t, picked: i === 0 })),
    { title: 'VeriGraph: targets', canPickMany: true },
  );
  if (!picked || !picked.length) return;
  const folder = vscode.workspace.getWorkspaceFolder(doc.uri);
  const base = folder ? folder.uri.fsPath : path.dirname(doc.fileName);
  const out = path.resolve(base, config().get('buildOutputDir'));
  const args = ['build', doc.fileName, label, ...picked.flatMap((t) => ['--target', t.label]), '-o', out];
  let r = await progress(`building ${label}`, () => vg(args, doc));
  if (!r.ok && r.verify) {
    show(doc, lib.fromVerify(r.verify, doc.getText()));
    const choice = await vscode.window.showWarningMessage(
      `VeriGraph: ${label} is not fully proven (${lib.verifySummary(r.verify)}).`,
      UNVERIFIED,
    );
    if (choice !== UNVERIFIED) return;
    r = await progress(`building ${label} with runtime checks`, () => vg([...args, '--unverified'], doc));
  }
  output.appendLine(`build ${label}`);
  for (const line of lib.describeBuild(r)) output.appendLine(`  ${line}`);
  report(r.ok, `${label}: ${r.ok ? `built in ${out}` : lib.describeBuild(r).join('; ')}`);
}

module.exports = { activate, deactivate };
