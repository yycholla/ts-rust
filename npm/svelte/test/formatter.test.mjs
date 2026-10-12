/**
 * Native CLI / Rust-WASM worker contract tests. Build both artifacts first:
 * node --test npm/svelte/test/formatter.test.mjs
 * TS_SVELTE_FORMAT_BIN and TS_SVELTE_FORMAT_WASM override the artifact paths.
 * Missing artifacts fail the suite; no JavaScript formatter fallback is used.
 */
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { createFormatter } from '../formatter.js';

const binary = process.env.TS_SVELTE_FORMAT_BIN
  ?? fileURLToPath(new URL('../../../target/release/svelte-format', import.meta.url));
const wasmPath = process.env.TS_SVELTE_FORMAT_WASM
  ?? new URL('../generated/ts_svelte_format_bg.wasm', import.meta.url);
const bytes = await readFile(wasmPath);
const module = await WebAssembly.compile(bytes);

function native(requests) {
  const child = spawnSync(binary, ['--serve'], {
    input: requests.map(request => JSON.stringify(request)).join('\n') + '\n',
    encoding: 'utf8',
    timeout: 60_000,
    maxBuffer: 8 * 1024 * 1024,
  });
  assert.ifError(child.error);
  assert.equal(child.status, 0, child.stderr);
  const replies = child.stdout.trimEnd().split('\n').map(line => JSON.parse(line));
  assert.equal(replies.length, requests.length, 'one reply per persistent request');
  return replies;
}

function request(filename, text, options = {}) {
  return { op: 'format', document: { uri: `file:///fixtures/${filename}`, version: 7, text }, options };
}

async function compare(formatter, requests) {
  const expected = native(requests);
  // Multiple outstanding worker requests must retain their own response IDs.
  const actual = await Promise.all(requests.map(value => formatter.request(value)));
  assert.deepEqual(actual, expected);
  return actual;
}

function success(reply) {
  assert.equal(reply.ok, true, JSON.stringify(reply));
  return reply.result;
}

function assertReplacement(result, document) {
  assert.equal(result.uri, document.uri);
  assert.equal(result.version, document.version);
  assert.equal(result.changed, true);
  assert.equal(result.edits.length, 1);
  const lines = document.text.split(/\r\n|\r|\n/);
  assert.deepEqual(result.edits[0], {
    range: {
      start: { line: 0, character: 0 },
      // JS string length is UTF-16, distinct from the byte count below.
      end: { line: lines.length - 1, character: lines.at(-1).length },
    },
    bytes: { start: 0, end: Buffer.byteLength(document.text, 'utf8') },
    newText: result.code,
  });
}

test('native and WASM format actual supported languages and agree on idempotency', async t => {
  const formatter = await createFormatter(module);
  t.after(() => formatter.dispose());
  const cases = [
    ['Component.svelte', '<script lang="ts">let count:number=0;</script><button onclick={()=>count++}>{count}</button><style>button{color:red}</style>', 'svelte', /onclick/],
    ['module.js', 'const value={name:"🙂",count:1};', 'javascript', /🙂/],
    ['counter.svelte.ts', 'export function counter(){let value:number=$state(0);return ()=>value++;}', 'typescript', /\$state\(0\)/],
    ['element.tsx', "const element=<div title='ok'>hello</div>;", 'typescript', /hello/],
    ['style.css', 'a{color:red;margin:0 1px}', 'css', /color: red/],
    ['value.json', '{"message":"🙂","items":[1,2,3]}', 'json', /🙂/],
    ['value.jsonc', '// retain comment\n{"message":"ok",}', 'json', /retain comment/],
  ];
  const requests = cases.map(([filename, source]) => request(filename, source));
  const replies = await compare(formatter, requests);
  for (const [index, reply] of replies.entries()) {
    const result = success(reply);
    assert.equal(result.language, cases[index][2]);
    assert.match(result.code, cases[index][3]);
    assertReplacement(result, requests[index].document);
  }
  assert.deepEqual(JSON.parse(replies[5].result.code), { message: '🙂', items: [1, 2, 3] });
  const repeated = requests.map((value, index) => ({
    ...value, document: { ...value.document, version: 8, text: replies[index].result.code },
  }));
  const second = await compare(formatter, repeated);
  for (const [index, reply] of second.entries()) {
    const result = success(reply);
    assert.equal(result.code, replies[index].result.code);
    assert.equal(result.version, 8);
    assert.equal(result.changed, false);
    assert.deepEqual(result.edits, []);
  }
});

test('resolved options and original BOM/CRLF/UTF-16 coordinates agree', async t => {
  const formatter = await createFormatter(module);
  t.after(() => formatter.dispose());
  const requests = [
    request('options.ts', 'const value = "hello";', { singleQuote: true, semi: false }),
    request('auto.ts', 'const x=1;\r\n', { endOfLine: 'auto' }),
    request('Unicode.svelte', '\ufeff<p>first</p>\r\n<p title=\'ok\'>🙂</p>', { endOfLine: 'crlf' }),
    { ...request('document', 'const value:number=1;'), filename: 'actual.ts' },
  ];
  const replies = await compare(formatter, requests);
  assert.equal(success(replies[0]).code, "const value = 'hello'\n");
  assert.equal(success(replies[1]).code, 'const x = 1;\r\n');
  const unicode = success(replies[2]);
  assertReplacement(unicode, requests[2].document);
  assert.equal(unicode.code[0], '\ufeff');
  assert.match(unicode.code, /🙂/);
  assert.ok(unicode.code.endsWith('\r\n'));
  assert.equal(unicode.code.replaceAll('\r\n', '').includes('\n'), false);
  assert.equal(success(replies[3]).filename, 'actual.ts');
  assert.equal(replies[3].result.language, 'typescript');
});

test('parse, option and protocol errors agree and do not poison the formatter', async t => {
  const formatter = await createFormatter(module);
  t.after(() => formatter.dispose());
  const cases = [
    [request('broken.js', 'const = ;'), 'format_error'],
    [request('broken.json', '{broken'), 'format_error'],
    [request('broken.svelte', '<script>const = ;</script>'), 'format_error'],
    [request('Style.svelte', '<style lang="sass">p\n  color: red</style>'), 'unsupported_style_language'],
    [request('unsupported.md', '# Hello'), 'unsupported_language'],
    [request('invalid.ts', 'const x=1;', { plugins: ['prettier-plugin-svelte'] }), 'invalid_request'],
    [request('invalid.ts', 'const x=1;', { trailingComma: 'maybe' }), 'invalid_request'],
    [request('invalid.ts', 'const x=1;', { svelteSortOrder: 'scripts-only' }), 'invalid_options'],
    [{ op: 'capabilities', unknown: true }, 'invalid_request'],
    ['not a protocol object', 'invalid_request'],
  ];
  const requests = [...cases.map(([value]) => value), request('recovered.js', 'const x=1;'), { op: 'capabilities' }];
  const replies = await compare(formatter, requests);
  for (const [index, [, code]] of cases.entries()) {
    assert.equal(replies[index].ok, false);
    assert.equal(replies[index].error.code, code);
    assert.equal(typeof replies[index].error.message, 'string');
    assert.ok(replies[index].error.message.length > 0);
    assert.equal('result' in replies[index], false, 'errors never return replacement edits');
  }
  assert.equal(success(replies.at(-2)).code, 'const x = 1;\n');
  const capabilities = success(replies.at(-1));
  assert.equal(capabilities.native, true);
  assert.equal(capabilities.wasm, true);
  assert.equal(capabilities.backend.name, 'rsvelte_formatter');
  for (const extension of ['svelte', 'js', 'ts', 'css', 'json']) {
    assert.ok(capabilities.extensions.includes(extension));
  }
});

test('formatter workers isolate requests and disposal rejects queued and later work', async () => {
  const first = await createFormatter(bytes);
  const second = await createFormatter(module);
  try {
    await assert.rejects(first.request(undefined), /JSON serializable/);
    const pending = first.request({ op: 'capabilities' });
    first.dispose();
    first.dispose();
    await assert.rejects(pending, /disposed/);
    await assert.rejects(first.request({ op: 'capabilities' }), /disposed/);
    const [reply] = await compare(second, [request('alive.ts', 'const value=2;')]);
    assert.equal(success(reply).code, 'const value = 2;\n');
  } finally {
    first.dispose();
    second.dispose();
  }
});
