import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { createCompiler } from '../compiler.js';
import { createReceiver } from '../compiler-worker-runtime.js';

const bytes = await readFile(new URL('../generated/ts_svelte_compiler_bg.wasm', import.meta.url));
const module = await WebAssembly.compile(bytes);
const binary = fileURLToPath(new URL('../../../target/release/svelte-tool', import.meta.url));
function native(request) {
  const child = spawnSync(binary, { input: JSON.stringify(request), encoding: 'utf8', maxBuffer: 16 * 1024 * 1024 });
  assert.ifError(child.error);
  const response = JSON.parse(child.stdout);
  assert.equal(child.status, response.ok ? 0 : 1);
  return response;
}
const document = text => ({ uri: 'Fixture.svelte', version: 1, text });
const requests = [
  { op: 'capabilities' },
  ...['client', 'server'].map(target => ({ op: 'compile', target, runes: true, document: document('<script lang="ts">let {count}: {count:number} = $props();</script><p>{count}</p>') })),
  { op: 'compile', target: 'client', document: document('😀\r\n<img src="x.png">') },
  { op: 'compile', target: 'server', document: document('<script><script>') },
  { op: 'project', typescript: true, runes: true, document: document('<script lang="ts">let {count}: {count:number} = $props();</script><p>😀{count.toFixed()}</p>') },
  { op: 'project', typescript: false, runes: true, document: document('<script>let value = $state(1);</script><p>{value}</p>') },
  { op: 'compile', target: 'client', document: document('\ufeff<p>x</p>') },
  { op: 'capabilities', unknown: true },
  { op: 'compile', target: 'client', dev: true, document: document('<p>dev</p>') },
  { op: 'compile', target: 'client', document: document('<select defaultValue="x"><option value="x">X</option></select>') },
  { op: 'project', typescript: false, document: document('<script lang="ts">const n: number = 1;</script>{n}') },
  { op: 'project', typescript: true, document: document('<script><script>') },
  ...['client','server'].map(target=>({op:'compileModule',target,document:{uri:'counter.svelte.js',version:1,text:'export function counter(){let count=$state(0);return {get value(){return count},increment(){count++}}}'}})),
];
test('a trapped backend is never re-entered by queued worker messages', async () => {
  const messages = [];
  let calls = 0;
  const receive = createReceiver(async () => {}, () => {
    calls++;
    throw new WebAssembly.RuntimeError('unreachable');
  }, message => messages.push(message));
  await receive({ module });
  await Promise.all([receive({ id: 1, input: '{}' }), receive({ id: 2, input: '{}' })]);
  await receive({ module });
  await receive({ id: 3, input: '{}' });
  assert.equal(calls, 1);
  assert.deepEqual(messages, [{ ready: true }, { fatal: 'unreachable' }]);
});
test('compiler native/WASM protocol and artifacts agree', async t => {
  const compiler = await createCompiler(module);
  t.after(() => compiler.dispose());
  for (const [index, request] of requests.entries()) {
    await t.test(`request ${index}: ${request.op}`, async () => {
      assert.deepEqual(await compiler.request(request), native(request));
    });
  }
});
test('workers isolate compilers and disposal rejects pending and later work', async () => {
  const first = await createCompiler(bytes);
  const second = await createCompiler(module);
  try {
    const pending = first.request({ op: 'capabilities' });
    first.dispose();
    first.dispose();
    await assert.rejects(pending, /disposed/);
    await assert.rejects(first.request({ op: 'capabilities' }), /disposed/);
    assert.equal((await second.request({ op: 'capabilities' })).ok, true);
  } finally { first.dispose(); second.dispose(); }
});
test('invalid host requests do not poison the worker', async t => {
  const compiler = await createCompiler(module);
  t.after(() => compiler.dispose());
  await assert.rejects(compiler.request(undefined), /JSON serializable/);
  await assert.rejects(compiler.request({ value: 1n }), /BigInt/);
  assert.equal((await compiler.request({ op: 'capabilities' })).ok, true);
  assert.equal((await compiler.request({ op: 'compile', target: 'server', document: document('<script>') })).ok, false);
  assert.equal((await compiler.request({ op: 'compile', target: 'server', document: document('<p>valid</p>') })).ok, true);
  assert.equal((await compiler.request({ op: 'compile', target: 'server', document: document('x'.repeat(1024 * 1024 + 1)) })).error.code, 'size_limit');
});
test('disposal during host serialization rejects without leaving pending work', async () => {
  const compiler = await createCompiler(module);
  await assert.rejects(compiler.request({ toJSON() {
    compiler.dispose();
    return { op: 'capabilities' };
  } }), /disposed/);
});
test('invalid WASM rejects creation', async () => {
  await assert.rejects(createCompiler(new Uint8Array([0, 1, 2, 3])), WebAssembly.CompileError);
});

test('script inference and rune-module checking agree natively and in WASM', async t => {
  const compiler = await createCompiler(module);
  t.after(() => compiler.dispose());
  const cases = [
    [{op:'componentInfo',document:document('<script lang="ts">let n:number=1;</script>{n}')},r=>assert.equal(r.result.typescript,true)],
    [{op:'componentInfo',document:document('<script>let n=1;</script>{n}')},r=>assert.equal(r.result.typescript,false)],
    [{op:'checkModule',typescript:true,document:{uri:'model.svelte.ts',version:1,text:'type 名 = number; export function count(){let value:名=$state(1);return ()=>value++;}'}},r=>assert.equal(r.ok,true)],
    [{op:'checkModule',typescript:true,document:{uri:'model.svelte.ts',version:1,text:'export const value=$derived(1);'}},r=>assert.equal(r.diagnostics[0].code,'derived_invalid_export')],
    [{op:'checkModule',typescript:true,document:{uri:'model.svelte.ts',version:1,text:'export class Model {constructor(public name:string){}}'}},r=>assert.equal(r.error.code,'unsupported_module_typescript')],
    [{op:'checkModule',typescript:true,document:{uri:'model.svelte.ts',version:1,text:'const value: = ;'}},r=>assert.equal(r.error.code,'module_typescript_parse')],
  ];
  for(const [request,verify] of cases){const response=await compiler.request(request);assert.deepEqual(response,native(request));verify(response);}
});
