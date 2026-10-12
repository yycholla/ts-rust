import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createSvelteProjectSession } from '../project.js';

// Inputs are standalone WASM artifacts and a caller-supplied virtual dependency
// snapshot. No application checkout, configuration execution or filesystem
// resolver is involved in the API under test.
const toolingPath = process.env.TS_SVELTE_TOOLING_WASM;
const semanticWasm = process.env.TS_SVELTE_SEMANTIC_WASM;
const dependenciesPath = process.env.TS_SVELTE_PROJECT_FILES;
const available = Boolean(toolingPath && semanticWasm);
const actual = { skip: available ? false : 'Set TS_SVELTE_TOOLING_WASM and TS_SVELTE_SEMANTIC_WASM', timeout: 180_000 };
const componentActual = { ...actual, skip: available && dependenciesPath ? false : 'Also set TS_SVELTE_PROJECT_FILES to a JSON map of /project/node_modules files' };
const uri = 'file:///project/main.ts';
const componentUri = 'file:///project/src/App.svelte';
const baseOptions = { strict: true, target: 'ESNext', module: 'ESNext', moduleResolution: 'Bundler', skipLibCheck: true };
const binaries = async () => ({ toolingWasm: await readFile(toolingPath), semanticWasm });
const position = (source, offset) => {
  const before = source.slice(0, offset).split('\n');
  return { line: before.length - 1, character: before.at(-1).length };
};
const offset = (source, point) => source.split('\n').slice(0, point.line).reduce((length, line) => length + line.length + 1, 0) + point.character;
const textAt = (source, range) => source.slice(offset(source, range.start), offset(source, range.end));
const at = (uri, source, offset) => ({ textDocument: { uri }, position: position(source, offset) });
const hasCode = (value, code) => Boolean(value && typeof value === 'object' &&
  (value.code === code || Object.values(value).some(child => hasCode(child, code))));
const plain = (text, version = -7) => ({
  root: '/project', files: { '/project/main.ts': text, '/project/value.ts': 'export const value = 42;\n' },
  documents: [{ uri, text, version }], rootFiles: ['main.ts'], compilerOptions: baseOptions,
});

test('actual composed project maps ordinary semantics and serializes transactional replacements', actual, async t => {
  const broken = "import { value } from './value';\nconst face = '😀'; export const answer: number = 'wrong';\n";
  const good = broken.replace("= 'wrong'", '= value');
  const project = await createSvelteProjectSession({ ...await binaries(), ...plain(broken) });
  t.after(() => project.dispose());
  assert.equal(project.revision, 1);
  const report = await project.request('textDocument/diagnostic', { textDocument: { uri } });
  const diagnostic = report.items.find(item => item.code === 2322);
  assert.ok(diagnostic, JSON.stringify(report));
  assert.equal(textAt(broken, diagnostic.range), 'answer');
  assert.deepEqual(diagnostic.range.start, position(broken, broken.indexOf('answer')));

  const edit = await project.request('textDocument/rename', { ...at(uri, broken, broken.indexOf('answer') + 1), newName: 'result' });
  assert.equal(edit.documentChanges[0].textDocument.uri, uri);
  assert.equal(edit.documentChanges[0].textDocument.version, -7);
  assert.equal(textAt(broken, edit.documentChanges[0].edits[0].range), 'answer');
  await assert.rejects(project.replace(plain(good, -7)), /version/i);
  assert.equal(project.revision, 1);
  assert.equal(hasCode(await project.request('textDocument/diagnostic', { textDocument: { uri } }), 2322), true);

  const pending = [
    project.replace(plain(good, -6)),
    project.request('textDocument/hover', at(uri, good, good.indexOf('answer') + 2)),
    project.request('textDocument/diagnostic', { textDocument: { uri } }),
  ];
  const [committed, hover, repaired] = await Promise.all(pending);
  assert.equal(committed.revision, 2);
  assert.equal(committed.complete, true);
  assert.match(JSON.stringify(hover), /number/);
  assert.equal(hasCode(repaired, 2322), false);
  const definition = await project.request('textDocument/definition', at(uri, good, good.lastIndexOf('value') + 1));
  assert.match(JSON.stringify(definition), /file:\/\/\/project\/value\.ts/);
  const workspace = await project.request('workspace/diagnostic');
  assert.equal(workspace.complete, true);
  assert.equal(workspace.documents.find(item => item.uri === uri).version, -6);
  const result = await project.shutdown();
  assert.equal(result.ok, true);
  await assert.rejects(project.request('textDocument/hover', at(uri, good, 5)), error => error.code === 'closed');
});

test('actual composed project captures inputs and disposal rejects queued work', actual, async t => {
  const text = 'export const answer: number = 42;\n';
  const project = await createSvelteProjectSession({ ...await binaries(), ...plain(text) });
  t.after(() => project.dispose());
  const replacement = plain(text.replace('42', '43'), -6);
  const queued = project.replace(replacement);
  replacement.documents[0].text = 'this is not valid';
  replacement.files['/project/main.ts'] = 'this is not valid';
  assert.equal((await queued).revision, 2);
  assert.equal((await project.prepared()).initialize.files['/project/main.ts'], text.replace('42', '43'));
  const first = project.request('textDocument/diagnostic', { textDocument: { uri } });
  const second = project.prepared();
  const failed = [assert.rejects(first, error => error.code === 'closed'), assert.rejects(second, error => error.code === 'closed')];
  project.dispose();
  project.dispose();
  await Promise.all(failed);
});

test('actual Rust component projection maps TS diagnostics, UTF-16, hover, definition and edits', componentActual, async t => {
  const dependencies = JSON.parse(await readFile(dependenciesPath, 'utf8'));
  const svelteVersion = JSON.parse(dependencies['/project/node_modules/svelte/package.json']).version;
  const broken = '<script lang="ts">\nimport { value } from "./value";\nconst face = "😀"; let count: number = "wrong";\n</script>\n<p>{count} {value} {face}</p>\n';
  const good = broken.replace('= "wrong"', '= value');
  const snapshot = (text, version) => ({
    root: '/project',
    files: { ...dependencies, '/project/src/App.svelte': text, '/project/src/value.ts': 'export const value = 42;\n' },
    components: [{ path: 'src/App.svelte', typescript: true, document: { uri: componentUri, text, version } }],
    compilerOptions: baseOptions, compatibility: { svelteVersion }, runes: true,
  });
  const project = await createSvelteProjectSession({ ...await binaries(), ...snapshot(broken, -9) });
  t.after(() => project.dispose());
  const report = await project.request('textDocument/diagnostic', { textDocument: { uri: componentUri } });
  const diagnostic = report.items.find(item => item.code === 2322);
  assert.ok(diagnostic, JSON.stringify(report));
  assert.equal(textAt(broken, diagnostic.range), 'count');
  // The astral symbol occurs before the mapped identifier on the same line.
  assert.deepEqual(diagnostic.range.start, position(broken, broken.indexOf('count')));
  const hover = await project.request('textDocument/hover', at(componentUri, broken, broken.lastIndexOf('count') + 2));
  assert.match(JSON.stringify(hover), /number/);
  assert.equal(textAt(broken, hover.range), 'count');
  const definition = await project.request('textDocument/definition', at(componentUri, broken, broken.lastIndexOf('value') + 2));
  assert.match(JSON.stringify(definition), /file:\/\/\/project\/src\/value\.ts/);
  const edit = await project.request('textDocument/rename', { ...at(componentUri, broken, broken.lastIndexOf('count') + 2), newName: 'total' });
  const changed = edit.documentChanges.find(item => item.textDocument?.uri === componentUri);
  assert.ok(changed, JSON.stringify(edit));
  assert.equal(changed.textDocument.version, -9);
  assert.ok(changed.edits.length >= 2);
  for (const change of changed.edits) {
    assert.equal(textAt(broken, change.range), 'count');
    assert.equal(change.newText, 'total');
  }
  await project.replace(snapshot(good, -8));
  const repaired = await project.request('workspace/diagnostic');
  assert.equal(repaired.complete, true);
  const original = repaired.documents.find(item => item.uri === componentUri);
  assert.ok(original, JSON.stringify(repaired));
  assert.equal(original.version, -8);
  assert.equal(hasCode(original.report, 2322), false);
  await assert.rejects(project.replace({ ...snapshot(good, -8), preprocess: { unsupported: true } }), /preprocess/i);
  assert.equal(project.revision, 2);
  assert.match(JSON.stringify(await project.request('textDocument/hover', at(componentUri, good, good.lastIndexOf('count') + 1))), /number/);
});
