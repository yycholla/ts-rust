#!/usr/bin/env node
/** Compare a native Rust Svelte compiler with the application's pinned compiler. */
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdir, readFile, readdir, realpath, stat, symlink, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const help = `Usage: node scripts/svelte/compiler-oracle.mjs --app PATH --binary PATH --out NEW_DIR [--project-compile]
Compare fixture client/server diagnostics and isolated SSR body/head outputs.
--project-compile additionally compiles every app src/**/*.svelte read-only.
Requires the application's installed Svelte 5.53.11. Never loads app config.
Output directory must not exist. Exit 0 = measured cases match, 1 = differences,
2 = harness/setup failure. This is bounded compiler evidence, not Svelte parity.`;
const hash = (data) => createHash('sha256').update(data).digest('hex');
const json = (value) => `${JSON.stringify(value, null, 2)}\n`;

function parseArgs(args) {
  const options = {};
  for (let index = 0; index < args.length; index++) {
    const key = args[index];
    if (key === '--project-compile') options.projectCompile = true;
    else if (['--app', '--binary', '--out'].includes(key)) {
      if (!args[index + 1] || args[index + 1].startsWith('--')) throw new Error(`Missing value for ${key}`);
      if (options[key.slice(2)]) throw new Error(`Duplicate ${key}`);
      options[key.slice(2)] = path.resolve(args[++index]);
    } else throw new Error(`Unknown argument ${key}`);
  }
  for (const key of ['app', 'binary', 'out']) if (!options[key]) throw new Error(`Missing --${key}`);
  return options;
}

async function filesUnder(directory, extension = null) {
  const result = [];
  for (const entry of (await readdir(directory, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
    if (['node_modules', '.git', '__pycache__'].includes(entry.name)) continue;
    const filename = path.join(directory, entry.name);
    if (entry.isDirectory()) result.push(...await filesUnder(filename, extension));
    else if (entry.isFile() && (!extension || filename.endsWith(extension))) result.push(filename);
  }
  return result;
}

async function sourceIdentity() {
  const identity = spawnSync('python3', ['scripts/svelte/source-identity.py'], { cwd: root, encoding: 'utf8' });
  if (identity.error || identity.status !== 0) throw new Error(`Cannot bind compiler source identity: ${identity.error?.message ?? identity.stderr}`);
  return JSON.parse(identity.stdout);
}

function oracleDiagnostic(diagnostic, severity, text) {
  const offset = (position) => {
    if (position == null) return null;
    const index = typeof position === 'number' ? position : position.character;
    if (!Number.isInteger(index) || index < 0 || index > text.length) throw new Error('Oracle returned an invalid source offset');
    return Buffer.byteLength(text.slice(0, index), 'utf8');
  };
  const start = offset(diagnostic.start);
  const end = offset(diagnostic.end);
  return {
    code: diagnostic.code ?? null,
    severity,
    message: diagnostic.message ?? null,
    bytes: start == null && end == null ? null : { start, end },
  };
}

function sortDiagnostics(diagnostics) {
  return diagnostics.sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b)));
}

function compileOracle(compiler, text, filename, target) {
  try {
    const artifact = compiler.compile(text, { filename, generate: target, runes: true, dev: false, css: 'external' });
    return {
      ok: true,
      diagnostics: sortDiagnostics((artifact.warnings ?? []).map((warning) => oracleDiagnostic(warning, 'warning', text))),
      javascript: artifact.js.code,
      css: artifact.css?.code ?? null,
      runes: artifact.metadata?.runes ?? null,
    };
  } catch (error) {
    // A compiler parse/analysis error is a result; an unexpected exception is a harness failure.
    if (typeof error.code !== 'string') throw error;
    return { ok: false, diagnostics: [oracleDiagnostic(error, 'error', text)] };
  }
}

function compileNative(binary, text, filename, target) {
  const request = { op: 'compile', document: { uri: filename, version: 1, text }, target, runes: true, dev: false };
  const child = spawnSync(binary, [], {
    input: JSON.stringify(request), encoding: 'utf8', timeout: 120_000, maxBuffer: 32 * 1024 * 1024,
  });
  if (child.error || child.signal) throw new Error(`Native compiler process failure: ${child.error?.message ?? child.signal}`);
  let response;
  try { response = JSON.parse(child.stdout); }
  catch { throw new Error(`Native compiler returned non-JSON output (exit ${child.status})`); }
  if (typeof response.ok !== 'boolean') throw new Error('Native compiler response lacks boolean ok');
  if (response.ok && child.status !== 0) throw new Error(`Native success response exited ${child.status}`);
  const body = response.ok ? response.result : response;
  if (!Array.isArray(body?.diagnostics)) throw new Error(`Native response has no diagnostics array: ${JSON.stringify(response.error)}`);
  const diagnostics = sortDiagnostics(body.diagnostics.map((diagnostic) => ({
    code: diagnostic.code ?? null,
    severity: diagnostic.severity ?? null,
    message: diagnostic.message ?? null,
    bytes: diagnostic.bytes ?? null,
  })));
  if (response.ok && typeof body.javascript?.code !== 'string') throw new Error('Native success response has no JavaScript');
  return {
    ok: response.ok, diagnostics, backend: body.backend ?? null,
    javascript: body.javascript?.code, css: body.css?.code ?? null, runes: body.runes ?? null,
    stderr: child.stderr, exitCode: child.status,
  };
}

const renderProgram = `
const { render } = await import('svelte/server');
const { default: Component } = await import(process.argv[1]);
const output = render(Component, { props: JSON.parse(process.argv[2]) });
console.log(JSON.stringify({ body: output.body, head: output.head }));
`;

function renderModule(filename, props, runtime) {
  const child = spawnSync(process.execPath, ['--input-type=module', '-e', renderProgram, pathToFileURL(filename).href, JSON.stringify(props)], {
    cwd: runtime, encoding: 'utf8', timeout: 30_000, maxBuffer: 4 * 1024 * 1024,
  });
  if (child.error || child.signal || child.status !== 0) {
    return { ok: false, error: child.error?.message ?? child.signal ?? `exit ${child.status}`, stderr: child.stderr };
  }
  try { return { ok: true, result: JSON.parse(child.stdout) }; }
  catch { return { ok: false, error: 'SSR stdout was not a JSON render result', stdout: child.stdout }; }
}

async function main(options) {
  const app = await realpath(options.app);
  const outputParent = await realpath(path.dirname(options.out));
  options.out = path.join(outputParent, path.basename(options.out));
  const relativeOutput = path.relative(app, options.out);
  if (relativeOutput === '' || (!relativeOutput.startsWith(`..${path.sep}`) && relativeOutput !== '..' && !path.isAbsolute(relativeOutput))) {
    throw new Error('Output must be outside the read-only application checkout');
  }
  const binary = await realpath(options.binary);
  const sveltePackagePath = path.join(app, 'node_modules/svelte/package.json');
  const sveltePackage = JSON.parse(await readFile(sveltePackagePath, 'utf8'));
  if (sveltePackage.version !== '5.53.11') throw new Error(`Expected Svelte 5.53.11; found ${sveltePackage.version}`);
  const compilerPath = path.join(app, 'node_modules/svelte/src/compiler/index.js');
  const compiler = await import(pathToFileURL(compilerPath).href);
  await mkdir(options.out); // Atomic refusal of any existing output directory.
  const runtime = path.join(options.out, 'runtime');
  await mkdir(runtime);
  await symlink(path.join(app, 'node_modules'), path.join(runtime, 'node_modules'), 'dir');
  const identity = {
    startedAt: new Date().toISOString(), node: process.version, app,
    svelte: { version: sveltePackage.version, packageHash: hash(await readFile(sveltePackagePath)), compilerEntryHash: hash(await readFile(compilerPath)) },
    binary: { path: binary, sha256: hash(await readFile(binary)), bytes: (await stat(binary)).size },
    source: await sourceIdentity(), options: { runes: true, dev: false, css: 'external', targets: ['client', 'server'] },
  };
  for (const lockfile of ['bun.lock', 'bun.lockb', 'package-lock.json', 'pnpm-lock.yaml', 'yarn.lock']) {
    try { (identity.appLockfiles ??= {})[lockfile] = hash(await readFile(path.join(app, lockfile))); }
    catch (error) { if (error.code !== 'ENOENT') throw error; }
  }
  await writeFile(path.join(options.out, 'identity.json'), json(identity));
  const report = { scope: 'bounded compiler diagnostics and SSR fixtures; not type checking, hydration, SvelteKit, or full parity', cases: [], project: null, differences: 0, failures: 0 };
  const fixtureDirectory = path.join(root, 'scripts/svelte/fixtures/compiler');
  const fixtures = (await filesUnder(fixtureDirectory, '.svelte')).map((file) => ({ file, name: path.basename(file), fixture: true }));
  const project = options.projectCompile
    ? (await filesUnder(path.join(app, 'src'), '.svelte')).map((file) => ({ file, name: path.relative(app, file), fixture: false })) : [];
  const projectHashes = [];
  for (const item of [...fixtures, ...project]) {
    const text = await readFile(item.file, 'utf8');
    const sourceHash = hash(text);
    if (!item.fixture) projectHashes.push([item.name, sourceHash]);
    for (const target of ['client', 'server']) {
      const record = { file: item.name, fixture: item.fixture, sourceHash, target };
      try {
        const oracle = compileOracle(compiler, text, item.name, target);
        const native = compileNative(binary, text, item.name, target);
        record.oracle = { ok: oracle.ok, diagnostics: oracle.diagnostics, runes: oracle.runes ?? null };
        record.native = { ok: native.ok, diagnostics: native.diagnostics, runes: native.runes ?? null, backend: native.backend };
        record.match = oracle.ok === native.ok && JSON.stringify(oracle.diagnostics) === JSON.stringify(native.diagnostics);
        if (oracle.ok && native.ok) {
          record.outputHashes = { oracle: { js: hash(oracle.javascript), css: hash(oracle.css ?? '') }, native: { js: hash(native.javascript), css: hash(native.css ?? '') } };
          if (oracle.runes !== native.runes) record.match = false;
          if (item.fixture && target === 'server' && ['typed-props.svelte', 'snippets.svelte'].includes(item.name)) {
            const props = item.name === 'typed-props.svelte' ? { count: 7, name: 'Ada & Lin' } : { label: 'hello <world>' };
            const sides = {};
            for (const [side, result] of [['oracle', oracle], ['native', native]]) {
              const modulePath = path.join(runtime, `${item.name}.${side}.mjs`);
              await writeFile(modulePath, result.javascript);
              sides[side] = renderModule(modulePath, props, runtime);
            }
            record.ssr = { props, ...sides, match: sides.oracle.ok && sides.native.ok && JSON.stringify(sides.oracle.result) === JSON.stringify(sides.native.result) };
            if (!record.ssr.match) record.match = false;
          }
        }
        if (item.fixture && target === 'server' && ['typed-props.svelte', 'snippets.svelte'].includes(item.name) && !record.ssr) {
          record.ssr = { match: false, unrun: 'One or both compilers did not produce JavaScript' };
          record.match = false;
        }
        if (native.stderr) record.native.stderr = native.stderr;
        if (!record.match) report.differences++;
      } catch (error) {
        record.match = false;
        record.harnessFailure = error.message;
        report.failures++;
      }
      // Full diagnostics are retained even on matching cases; no warning/span filtering.
      report.cases.push(record);
      await writeFile(path.join(options.out, 'report.json'), json(report));
    }
  }
  if (options.projectCompile) report.project = { files: projectHashes.length, sourceFingerprint: hash(JSON.stringify(projectHashes)), sources: projectHashes, configExecuted: false, preprocessing: false };
  report.completedAt = new Date().toISOString();
  report.summary = { cases: report.cases.length, matched: report.cases.filter((item) => item.match).length, differences: report.differences, harnessFailures: report.failures };
  await writeFile(path.join(options.out, 'report.json'), json(report));
  console.log(json({ ...report.summary, report: path.join(options.out, 'report.json') }));
  process.exitCode = report.failures ? 2 : report.differences ? 1 : 0;
}

if (process.argv.slice(2).some((arg) => ['--help', 'help', '-h'].includes(arg))) console.log(help);
else {
  try { await main(parseArgs(process.argv.slice(2))); }
  catch (error) { console.error(`compiler-oracle: ${error.message}`); process.exitCode = 2; }
}
