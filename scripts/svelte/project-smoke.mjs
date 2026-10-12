#!/usr/bin/env node
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdir, readFile, realpath, stat, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
if (process.argv.includes('help') || process.argv.includes('--help')) {
  console.log('Usage: node scripts/svelte/project-smoke.mjs APP_CHECKOUT NEW_OUTPUT_DIRECTORY');
  process.exit(0);
}
assert.equal(process.argv.length, 4);
const app = await realpath(process.argv[2]);
const out = path.resolve(process.argv[3]);
assert(!out.startsWith(`${app}${path.sep}`) && out !== app);
await mkdir(out);
const binary = path.join(root, 'target/release/svelte-project');
const report = { binarySha256: createHash('sha256').update(await readFile(binary)).digest('hex'), cases: [] };
const source = spawnSync('python3', ['scripts/svelte/source-identity.py'], { cwd: root, encoding: 'utf8' });
assert.equal(source.status, 0);
await writeFile(path.join(out, 'source.json'), source.stdout);
const fixture = name => readFile(path.join(root, 'scripts/svelte/fixtures/checker', `${name}.svelte`), 'utf8');
const child = await fixture('Child');
const cases = [
  { name: 'typescript-valid', source: await fixture('valid-props'), typescript: true, code: null },
  { name: 'typescript-wrong-props', source: await fixture('wrong-props'), typescript: true, code: 2322 },
  { name: 'typescript-emoji', source: await fixture('emoji'), typescript: true, code: 2304 },
  { name: 'javascript-valid', source: '<script>let value = $state(1);</script><p>{value.toFixed()}</p>', typescript: false, code: null },
  { name: 'javascript-jsdoc-error', source: '<script>/** @type {number} */ let value = \'bad\';</script><p>{value}</p>', typescript: false, code: 2322 },
  { name: 'javascript-template-error', source: '<script>let value = $state(1);</script><p>{value.notARealMember()}</p>', typescript: false, code: 2339 },
  { name: 'literal-directive-preserved', source: `<script lang="ts">const directive = '///<reference types="svelte" />';</script><p>{directive}</p>`, typescript: true, code: null },
  ...['ts', 'js'].map(extension => ({ name: `unsupported-rune-${extension}`, source: '<p>valid</p>', typescript: true, files: [{ path: `state.svelte.${extension}`, text: 'let state = $state(1);' }], errorCode: 'unsupported_rune_module' })),
  { name: 'compiler-error', source: '<script>let count = $derived();</script>{count}', typescript: false, compilerCode: 'rune_invalid_arguments_length' },
];
function invoke(request) {
  const child = spawnSync(binary, { input: JSON.stringify(request), encoding: 'utf8', timeout: 180_000, maxBuffer: 32 * 1024 * 1024 });
  assert.ifError(child.error);
  assert.equal(child.signal, null);
  return { exit: child.status, stderr: child.stderr, response: JSON.parse(child.stdout) };
}
for (const item of cases) {
  const request = {
    files: item.files ?? [],
    workspace: path.join(out, item.name), dependencies: path.join(app, 'node_modules'),
    components: [
      { path: 'Child.svelte', document: { uri: 'file:///Child.svelte', version: 1, text: child }, typescript: true },
      { path: 'Case.svelte', document: { uri: 'file:///Case.svelte', version: 1, text: item.source }, typescript: item.typescript },
    ],
  };
  const result = { name: item.name, expectedCode: item.code, passed: false };
  try {
    result.run = invoke(request);
    const response = result.run.response;
    if (item.errorCode) {
      assert.equal(response.ok, false);
      assert.equal(result.run.exit, 1);
      assert.equal(response.error.code, item.errorCode);
      await assert.rejects(stat(request.workspace), { code: 'ENOENT' });
      result.passed = true;
      report.cases.push(result);
      await writeFile(path.join(out, 'report.json'), `${JSON.stringify(report, null, 2)}\n`);
      continue;
    }
    if (item.compilerCode) {
      assert.equal(response.ok, false);
      assert.equal(result.run.exit, 1);
      assert(response.diagnostics.some(diagnostic => diagnostic.code === item.compilerCode));
      result.passed = true;
      report.cases.push(result);
      await writeFile(path.join(out, 'report.json'), `${JSON.stringify(report, null, 2)}\n`);
      continue;
    }
    assert.equal(response.ok, true, JSON.stringify(response.error ?? response.failure ?? response.checker?.error));
    assert.equal(response.checker.projectionsVerified, true);
    assert.deepEqual(response.checker.unported, []);
    if (item.code === null) {
      assert.equal(response.checker.status, 0);
      assert.deepEqual(response.checker.diagnostics, []);
      assert.equal(result.run.exit, 0);
    } else {
      assert(response.checker.diagnostics.some(diagnostic => diagnostic.code === item.code));
      assert.equal(result.run.exit, 1);
      if (item.name === 'typescript-emoji') {
        const error = response.checker.diagnostics.find(diagnostic => diagnostic.code === 2304);
        assert.equal(error.original.status, 'mapped');
        assert.equal(Buffer.from(item.source).subarray(error.original.bytes.start, error.original.bytes.end).toString(), 'missingAfterEmoji');
      }
    }
    result.reusedWorkspace = invoke(request);
    assert.equal(result.reusedWorkspace.response.ok, false);
    result.passed = true;
  } catch (error) { result.failure = error.message; }
  report.cases.push(result);
  await writeFile(path.join(out, 'report.json'), `${JSON.stringify(report, null, 2)}\n`);
}
report.summary = { cases: report.cases.length, passed: report.cases.filter(item => item.passed).length };
await writeFile(path.join(out, 'report.json'), `${JSON.stringify(report, null, 2)}\n`);
console.log(JSON.stringify(report.summary));
process.exitCode = report.summary.passed === report.summary.cases ? 0 : 1;
