import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { createSemanticSession } from '../semantic-node.js';
import { connectSemanticWorker } from '../semantic-client.js';
import { createSemanticHost } from '../semantic.js';

const wasm = process.env.TS_SVELTE_SEMANTIC_WASM;
const uri = 'file:///project/main.ts';
const good = "import { value } from './value';\nexport const answer: number = value;\n";
const options = () => ({ wasm, root: '/project', project: '/project/tsconfig.json', files: {
  '/project/tsconfig.json': JSON.stringify({ compilerOptions: { strict: true, target: 'ESNext', module: 'ESNext', moduleResolution: 'Bundler', skipLibCheck: true }, include: ['*.ts'] }),
  '/project/main.ts': good,
  '/project/value.ts': 'export const value = 42;\n',
} });
const params = { textDocument: { uri } };
const code = (value, number) => value && typeof value === 'object' &&
  (value.code === number || Object.values(value).some(child => code(child, number)));
const success = reply => { assert.equal(reply.ok, true, JSON.stringify(reply)); return reply.result; };
const open = (session, text = good, version = -7) => session.request('open', {
  textDocument: { uri, languageId: 'typescript', version, text },
}).then(success);
const change = (session, text, version) => session.request('change', {
  textDocument: { uri, version }, contentChanges: [{ text }],
}).then(success);
const actual = { skip: wasm ? false : 'Set TS_SVELTE_SEMANTIC_WASM to the built WASI module', timeout: 120_000 };

test('actual Rust WASI session retains edits, serializes generations and resolves navigation', actual, async t => {
  const session = await createSemanticSession(options());
  t.after(() => session.dispose());
  assert.equal(session.initialized.result.positionEncoding, 'utf-16');
  await open(session);
  success(await session.request('diagnostics', params));
  // Enqueue together: generations must be assigned after prior work completes.
  const answers = await Promise.all([
    session.request('change', { textDocument: { uri, version: -6 }, contentChanges: [{ text: "export const answer: number = 'wrong';\n" }] }),
    session.request('diagnostics', params),
    session.request('change', { textDocument: { uri, version: -5 }, contentChanges: [{ text: good }] }),
    session.request('diagnostics', params),
  ]);
  answers.forEach(success);
  assert.equal(code(answers[1], 2322), true);
  assert.equal(Boolean(code(answers[3], 2322)), false);
  assert.deepEqual(answers.map(answer => answer.generation), Array.from({ length: 4 }, (_, index) => answers[0].generation + index));
  assert.match(JSON.stringify(success(await session.request('hover', { ...params, position: { line: 1, character: 14 } }))), /number/);
  assert.match(JSON.stringify(success(await session.request('definition', { ...params, position: { line: 1, character: 30 } }))), /value\.ts/);
  success(await session.files({ '/project/value.ts': "export const value = 'changed';\n" }));
  assert.equal(code(await session.request('diagnostics', params), 2322), true);
  success(await session.files({ '/project/value.ts': null }));
  assert.equal(code(await session.request('diagnostics', params), 2307), true);
  success(await session.files({ '/project/value.ts': 'export const value = 42;\n' }));
  assert.equal(Boolean(code(await session.request('diagnostics', params), 2307)), false);
  await change(session, 'export function loose(value) { return value; }\n', -4);
  assert.equal(code(await session.request('diagnostics', params), 7006), true);
  success(await session.files({ '/project/tsconfig.json': JSON.stringify({ compilerOptions: { strict: false, skipLibCheck: true }, include: ['*.ts'] }) }));
  assert.equal(Boolean(code(await session.request('diagnostics', params), 7006)), false);
  success(await session.shutdown());
  await assert.rejects(session.request('diagnostics', params), error => error.code === 'closed');
});

test('actual snapshots replay full text and signed versions into a fresh worker', actual, async t => {
  const first = await createSemanticSession(options());
  t.after(() => first.dispose());
  await open(first, '// 😀\r\nexport const answer: number = 1;\n');
  success(await first.request('change', { textDocument: { uri, version: -6 }, contentChanges: [
    { range: { start: { line: 0, character: 3 }, end: { line: 0, character: 5 } }, rangeLength: 2, text: 'changed' },
  ] }));
  success(await first.request('configure', { includeCompletionsForModuleExports: false }));
  const snapshot = await first.snapshot();
  assert.equal(snapshot.documents[0].version, -6);
  assert.equal(snapshot.documents[0].text, '// changed\r\nexport const answer: number = 1;\n');
  const second = await createSemanticSession({ wasm, snapshot });
  t.after(() => second.dispose());
  assert.deepEqual(success(await first.request('diagnostics', params)), success(await second.request('diagnostics', params)));
  await change(second, "export const answer: number = 'error';\n", -5);
  assert.equal(code(await second.request('diagnostics', params), 2322), true);
  assert.equal(Boolean(code(await first.request('diagnostics', params), 2322)), false);
  const rejected = await second.request('change', { textDocument: { uri, version: -5 }, contentChanges: [{ text: good }] });
  assert.equal(rejected.ok, false);
  await assert.rejects(second.snapshot(), error => error.code === 'snapshot_unavailable');
  // Request failure still updates the generation; subsequent reads remain valid.
  success(await second.request('diagnostics', params));
});

class FakeWorker extends EventEmitter {
  messages = [];
  terminated = 0;
  postMessage(message) { this.messages.push(message); }
  terminate() { this.terminated++; }
}
async function fakeConnection() {
  const worker = new FakeWorker();
  const connection = connectSemanticWorker(worker, { epoch: 'current', module: null, params: {} });
  const initial = worker.messages[0];
  worker.emit('message', { epoch: 'old', id: initial.id, type: 'fatal', error: { message: 'stale' } });
  worker.emit('message', { epoch: 'current', id: initial.id, type: 'response', value: { ok: true, result: {} } });
  return { worker, session: await connection };
}
test('transport ignores old epochs and fatal messages reject all queued work', async () => {
  const { worker, session } = await fakeConnection();
  const first = session.request('hover', {});
  const second = session.snapshot();
  const assertions = [assert.rejects(first, /trap/), assert.rejects(second, /trap/)];
  worker.emit('message', { epoch: 'current', type: 'fatal', error: { code: 'engine_trap', message: 'trap' } });
  await Promise.all(assertions);
  assert.equal(worker.terminated, 1);
  await assert.rejects(session.request('diagnostics', {}), error => error.code === 'closed');
});
test('transport disposal rejects pending work and cannot settle a later epoch', async () => {
  const { worker, session } = await fakeConnection();
  const pending = session.request('hover', {});
  const rejected = assert.rejects(pending, /disposed/);
  session.dispose();
  session.dispose();
  worker.emit('message', { epoch: 'current', type: 'response', id: 2, value: { ok: true } });
  await rejected;
  assert.equal(worker.terminated, 1);
});
test('low-level loader rejects modules without the semantic ABI', async () => {
  const empty = new Uint8Array([0, 97, 115, 109, 1, 0, 0, 0]);
  await assert.rejects(createSemanticHost(empty), error => error.code === 'invalid_module');
});
