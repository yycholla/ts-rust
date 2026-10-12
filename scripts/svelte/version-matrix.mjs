#!/usr/bin/env node
/** Exact-version diagnostic and explicit-project comparisons, never Kit build claims. */
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, readFile, readdir, realpath, stat, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const fixtures = path.join(root, 'scripts/svelte/fixtures/versions');
const svelteVersions = ['5.51.5', '5.52.0', '5.53.5', '5.53.11', '5.53.13', '5.54.1', '5.55.10', '5.56.10', '5.57.2'];
const kitVersions = ['2.67.0', '2.68.0', '2.69.2', '2.69.3', '2.70.3', '3.0.1', '3.0.0-next.32'];
const help = `Usage: node scripts/svelte/version-matrix.mjs --out NEW_DIR [--binary PATH]
Uses exact installations in target/svelte-versions/{svelte,kit}-VERSION.
--binary defaults to target/release/svelte-tool; project/checker binaries are siblings.
Compiler diagnostics: original fixtures and version syntax, client/server, full messages/ranges.
Checking: valid/invalid TS and JS/JSDoc, comparing TypeScript on the SAME Rust projections.
Kit profiles: dependency detection and explicit inputs only; NO Kit graph/build/LSP claim.
Raw results and before/after source/binary hashes are retained. Output parent must exist.
Exit 0: all measured comparisons/controls match; 1: differences; 2: harness/incomplete run.
Experimental async is disabled; rejected async input is not evidence of async support.`;
const json = value => `${JSON.stringify(value, null, 2)}\n`;
const sha = value => createHash('sha256').update(value).digest('hex');
const equal = (left, right) => JSON.stringify(left) === JSON.stringify(right);
const sorted = values => values.sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b)));
const require = createRequire(import.meta.url);
const tsModules = new Map();

function argumentsOf(args) {
  const options = { binary: path.join(root, 'target/release/svelte-tool') };
  const seen = new Set();
  for (let i = 0; i < args.length; i++) {
    const flag = args[i];
    if (!['--out', '--binary'].includes(flag) || seen.has(flag) || !args[i + 1] || args[i + 1].startsWith('--')) throw new Error(`Invalid argument ${flag}`);
    seen.add(flag);
    options[flag.slice(2)] = path.resolve(args[++i]);
  }
  if (!options.out) throw new Error('Missing --out');
  return options;
}

function sourceIdentity() {
  const run = spawnSync('python3', ['scripts/svelte/source-identity.py'], { cwd: root, encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 });
  if (run.error || run.status !== 0) throw new Error(`Source identity failed: ${run.error?.message ?? run.stderr}`);
  return JSON.parse(run.stdout);
}

function invoke(binary, request) {
  const child = spawnSync(binary, [], { input: JSON.stringify(request), encoding: 'utf8', timeout: 180_000, maxBuffer: 32 * 1024 * 1024 });
  const run = { request, exit: child.status, signal: child.signal, stdout: child.stdout ?? '', stderr: child.stderr ?? '' };
  if (child.error) run.failure = child.error.message;
  try { run.response = JSON.parse(run.stdout); } catch { run.failure ??= 'Non-JSON response'; }
  if (child.signal || ![0, 1].includes(child.status) || typeof run.response?.ok !== 'boolean') run.failure ??= 'Invalid process status/response';
  return run;
}

function diagnostic(item, text, oracle, severity) {
  const offset = position => {
    if (position == null) return null;
    const value = typeof position === 'number' ? position : position.character;
    assert(Number.isInteger(value) && value >= 0 && value <= text.length, 'Invalid oracle offset');
    return Buffer.byteLength(text.slice(0, value));
  };
  const start = oracle ? offset(item.start) : null;
  const end = oracle ? offset(item.end) : null;
  return { code: item.code ?? null, severity: oracle ? severity : item.severity ?? null, message: item.message ?? null,
    bytes: oracle ? (start == null && end == null ? null : { start, end }) : item.bytes ?? null };
}

function compileOracle(compiler, text, options) {
  try {
    const artifact = compiler.compile(text, options);
    return { ok: true, artifact, comparable: { ok: true, runes: artifact.metadata?.runes ?? null,
      diagnostics: sorted(artifact.warnings.map(item => diagnostic(item, text, true, 'warning'))) } };
  } catch (error) {
    if (typeof error.code !== 'string') return { failure: error.stack ?? error.message };
    return { ok: false, error: { ...error, message: error.message, stack: error.stack },
      comparable: { ok: false, runes: null, diagnostics: [diagnostic(error, text, true, 'error')] } };
  }
}

function compilerComparable(run) {
  const response = run.response;
  const body = response?.ok ? response.result : response;
  return { ok: response?.ok ?? null, runes: response?.ok ? body.runes ?? null : null,
    diagnostics: sorted((body?.diagnostics ?? []).map(item => diagnostic(item, '', false))) };
}

function nativeDiagnostic(item) {
  const file = item.fileName || null;
  return { file, start: file ? item.pos : null, end: file ? item.end : null, code: item.code,
    category: item.category, source: item.source ?? '', text: item.text,
    unnecessary: Boolean(item.reportsUnnecessary), deprecated: Boolean(item.reportsDeprecated),
    chain: (item.messageChain ?? []).map(nativeDiagnostic), related: (item.relatedInformation ?? []).map(nativeDiagnostic) };
}

function tsDiagnostic(item) {
  const object = typeof item.messageText === 'object';
  return { file: item.file?.fileName ?? null, start: item.file ? item.start ?? null : null,
    end: item.file && item.start != null ? item.start + (item.length ?? 0) : null,
    code: item.code, category: item.category, source: item.source ?? '', text: object ? item.messageText.messageText : item.messageText,
    unnecessary: Boolean(item.reportsUnnecessary), deprecated: Boolean(item.reportsDeprecated),
    chain: (object ? item.messageText.next ?? [] : item.next ?? []).map(tsDiagnostic), related: (item.relatedInformation ?? []).map(tsDiagnostic) };
}

async function tsCheck(workspace, installation) {
  const entry = path.join(installation, 'node_modules/typescript/lib/typescript.js');
  const digest = sha(await readFile(entry));
  // Reuse only byte-identical oracle implementations; declarations resolve in each workspace.
  if (!tsModules.has(digest)) tsModules.set(digest, require(entry));
  const ts = tsModules.get(digest);
  const config = ts.readConfigFile(path.join(workspace, 'tsconfig.json'), ts.sys.readFile);
  const parsed = ts.parseJsonConfigFileContent(config.config ?? {}, ts.sys, workspace, { noEmit: true, noCheck: false });
  const program = ts.createProgram({ rootNames: parsed.fileNames, options: parsed.options });
  const diagnostics = [...(config.error ? [config.error] : []), ...parsed.errors, ...ts.getPreEmitDiagnostics(program)];
  return { version: ts.version, entrySha256: digest, config: config.config, parsedOptions: parsed.options,
    fileNames: parsed.fileNames, diagnostics: sorted(diagnostics.map(tsDiagnostic)) };
}

async function packageIdentity(installation, name, optional = false) {
  try {
    const bytes = await readFile(path.join(installation, 'node_modules', name, 'package.json'));
    return { version: JSON.parse(bytes).version, sha256: sha(bytes) };
  } catch (error) { if (optional && error.code === 'ENOENT') return null; throw error; }
}

async function main(options) {
  options.binary = await realpath(options.binary);
  options.out = path.join(await realpath(path.dirname(options.out)), path.basename(options.out));
  const installs = path.join(root, 'target/svelte-versions');
  assert(options.out !== installs && !options.out.startsWith(`${installs}${path.sep}`), 'Output must not modify installed oracles');
  await mkdir(options.out); // Never reuse or rewrite earlier evidence.
  const binaries = [options.binary, ...['svelte-project', 'svelte-ts-check'].map(name => path.join(path.dirname(options.binary), name))];
  const binaryIdentities = async () => Promise.all(binaries.map(async file => ({ path: file, sha256: sha(await readFile(file)) })));
  const report = { scope: 'Exact-version compiler diagnostics and explicit-input project checking; TS oracle uses Rust projections. No runtime, Kit graph/build, LSP, or general version-range parity.',
    startedAt: new Date().toISOString(), node: process.version, installations: [], compiler: [], projects: [], controls: [], failures: [] };
  const save = () => writeFile(path.join(options.out, 'report.json'), json(report));
  const raw = async (name, value) => { await writeFile(path.join(options.out, name), json(value)); return name; };
  try {
    report.sourceBefore = sourceIdentity();
    report.binariesBefore = await binaryIdentities();
    const capabilities = invoke(options.binary, { op: 'capabilities' });
    assert(!capabilities.failure && capabilities.exit === 0, 'Capabilities request failed');
    assert.deepEqual(capabilities.response.result.svelteVersions, svelteVersions, 'Every advertised Svelte profile must appear in this matrix');
    assert.deepEqual(capabilities.response.result.kitVersions, kitVersions, 'Every advertised Kit profile must appear in this matrix');
    await raw('capabilities.json', capabilities);
    await save();
    const manifest = JSON.parse(await readFile(path.join(fixtures, 'cases.json'), 'utf8'));
    const oldRoot = path.join(root, 'scripts/svelte/fixtures/compiler');
    const old = (await readdir(oldRoot)).filter(file => file.endsWith('.svelte')).sort().map(file => ({ file, directory: oldRoot, runes: true }));
    const compilerCases = [...old, ...manifest.compiler.map(item => ({ ...item, directory: fixtures }))];
    for (const [kind, versions] of [['svelte', svelteVersions], ['kit', kitVersions]]) for (const version of versions) {
      const id = `${kind}-${version}`;
      try {
        const installation = await realpath(path.join(installs, id));
        const packages = {};
        for (const name of ['svelte', '@sveltejs/kit', 'typescript', 'svelte-check']) packages[name] = await packageIdentity(installation, name, name === '@sveltejs/kit');
        assert.equal(packages[kind === 'svelte' ? 'svelte' : '@sveltejs/kit'].version, version, 'Wrong installed oracle');
        const versions = { svelteVersion: packages.svelte.version, typescriptVersion: packages.typescript.version,
          ...(packages['@sveltejs/kit'] ? { kitVersion: packages['@sveltejs/kit'].version } : {}), allowPrerelease: version.includes('-') };
        const entry = path.join(installation, 'node_modules/svelte/src/compiler/index.js');
        report.installations.push({ id, installation, packages, versions, compilerSha256: sha(await readFile(entry)), lockSha256: sha(await readFile(path.join(installation, 'package-lock.json'))) });
        const profile = invoke(options.binary, { op: 'resolveCompatibility', versions });
        const resolved = profile.response?.result?.compatibility;
        report.controls.push({ id: `${id}:profile`, raw: await raw(`${id}-profile.json`, profile),
          match: !profile.failure && profile.exit === 0 && profile.response?.ok === true && resolved?.svelteVersion === versions.svelteVersion
            && (resolved?.kitVersion ?? null) === (versions.kitVersion ?? null) && resolved?.capabilities?.kitGraph === false && resolved?.capabilities?.kitBuild === false });
        if (kind === 'svelte') {
          const compiler = await import(pathToFileURL(entry).href);
          for (const item of compilerCases) for (const target of ['client', 'server']) {
            const name = `${id}-${item.name ?? item.file}-${target}`;
            try {
              const text = await readFile(path.join(item.directory, item.file), 'utf8');
              const settings = { filename: item.file, generate: target, css: 'external', dev: false,
                ...(Object.hasOwn(item, 'runes') ? { runes: item.runes } : {}) };
              const oracle = compileOracle(compiler, text, settings);
              const native = invoke(options.binary, { op: 'compile', document: { uri: item.file, version: 1, text }, target,
                dev: false, ...(Object.hasOwn(item, 'runes') ? { runes: item.runes } : {}), compatibility: { svelteVersion: version } });
              const comparable = compilerComparable(native);
              const compatibility = native.response?.ok ? native.response.result?.compatibility : native.response?.compatibility;
              const body = native.response?.ok ? native.response.result : native.response;
              const complete = !oracle.failure && !native.failure && Array.isArray(body?.diagnostics)
                && (!native.response.ok || (native.exit === 0 && typeof body.javascript?.code === 'string'));
              const record = { id: name, sourceSha256: sha(text), note: item.note, complete,
                match: complete && compatibility?.svelteVersion === version && equal(comparable, oracle.comparable),
                native: comparable, oracle: oracle.comparable,
                raw: await raw(`${name}.json`, { source: text, settings, native, oracle }) };
              if (!native.response?.ok) record.nativeError = native.response?.error ?? null;
              report.compiler.push(record);
            } catch (error) { report.compiler.push({ id: name, complete: false, match: false, failure: error.stack ?? error.message }); }
            await save();
          }
        }
        for (const item of manifest.projects) {
          const name = `${id}-${item.file}`;
          try {
            const text = await readFile(path.join(fixtures, item.file), 'utf8');
            const workspace = path.join(options.out, `${name}-workspace`);
            const native = invoke(binaries[1], { workspace, dependencies: path.join(installation, 'node_modules'), allowPrerelease: versions.allowPrerelease,
              components: [{ path: 'Case.svelte', document: { uri: 'Case.svelte', version: 1, text }, typescript: item.typescript }] });
            const response = native.response;
            let oracle = null;
            // Even an incomplete native check may have produced a usable workspace: retain the independent result.
            try { await stat(path.join(workspace, 'tsconfig.json')); oracle = await tsCheck(workspace, installation); }
            catch (error) { oracle = { failure: error.stack ?? error.message }; }
            const diagnostics = sorted((response?.checker?.diagnostics ?? []).map(nativeDiagnostic));
            const expected = values => Array.isArray(values) && (item.codes.length ? item.codes.every(code => values.some(d => d.code === code)) : values.length === 0);
            const complete = !native.failure && response?.ok === true && response.checker?.ok === true && response.checker.projectionsVerified === true
              && equal(response.checker.unported, []) && equal(response.checker.serializationFailures, [])
              && Array.isArray(response.checker.diagnostics) && !oracle.failure;
            const compatibility = response?.compatibility;
            const profileMatch = compatibility?.svelteVersion === versions.svelteVersion && (compatibility?.kitVersion ?? null) === (versions.kitVersion ?? null)
              && compatibility?.capabilities?.kitGraph === false && compatibility?.capabilities?.kitBuild === false;
            report.projects.push({ id: name, sourceSha256: sha(text), complete,
              match: complete && profileMatch && expected(diagnostics) && expected(oracle.diagnostics) && equal(diagnostics, oracle.diagnostics)
                && native.exit === (diagnostics.some(d => d.category === 1) ? 1 : 0),
              native: diagnostics, oracle: oracle.diagnostics, compatibility,
              raw: await raw(`${name}.json`, { source: text, native, oracle }) });
          } catch (error) { report.projects.push({ id: name, complete: false, match: false, failure: error.stack ?? error.message }); }
          await save();
        }
        for (const extension of ['js', 'ts']) {
          const name = `${id}-unsupported-rune-${extension}`;
          const workspace = path.join(options.out, `${name}-workspace`);
          const native = invoke(binaries[1], { workspace, dependencies: path.join(installation, 'node_modules'), allowPrerelease: versions.allowPrerelease,
            components: [], files: [{ path: `state.svelte.${extension}`, text: 'export const value = $state({ count: 0 });' }] });
          let workspaceAbsent = false;
          try { await stat(workspace); } catch (error) { if (error.code === 'ENOENT') workspaceAbsent = true; else throw error; }
          report.controls.push({ id: name, unsupported: true, raw: await raw(`${name}.json`, native),
            match: !native.failure && native.exit === 1 && native.response?.error?.code === 'unsupported_rune_module' && workspaceAbsent });
        }
        if (versions.allowPrerelease) {
          const native = invoke(options.binary, { op: 'resolveCompatibility', versions: { ...versions, allowPrerelease: false } });
          report.controls.push({ id: `${id}-prerelease-opt-in`, raw: await raw(`${id}-prerelease-opt-in.json`, native),
            match: !native.failure && native.exit === 1 && native.response?.ok === false });
          const workspace = path.join(options.out, `${id}-prerelease-denied-workspace`);
          const denied = invoke(binaries[1], { workspace, dependencies: path.join(installation, 'node_modules'), components: [] });
          let workspaceAbsent = false;
          try { await stat(workspace); } catch (error) { if (error.code === 'ENOENT') workspaceAbsent = true; else throw error; }
          report.controls.push({ id: `${id}-project-prerelease-opt-in`, raw: await raw(`${id}-project-prerelease-opt-in.json`, denied),
            match: !denied.failure && denied.exit === 1 && denied.response?.error?.code === 'unsupported_version' && workspaceAbsent });
        }
      } catch (error) { report.failures.push({ id, failure: error.stack ?? error.message }); }
      await save();
    }
  } catch (error) { report.failures.push({ failure: error.stack ?? error.message }); }
  finally {
    try {
      report.sourceAfter = sourceIdentity(); report.binariesAfter = await binaryIdentities();
      if (!equal(report.sourceBefore, report.sourceAfter) || !equal(report.binariesBefore, report.binariesAfter)) report.failures.push({ failure: 'Source or binary changed during run' });
    } catch (error) { report.failures.push({ failure: `Final identity: ${error.message}` }); }
    report.completedAt = new Date().toISOString();
    report.summary = Object.fromEntries(['compiler', 'projects', 'controls'].map(kind => [kind, {
      cases: report[kind].length, matched: report[kind].filter(item => item.match).length,
      differences: report[kind].filter(item => !item.match && item.complete !== false).length,
      incomplete: report[kind].filter(item => item.complete === false).length,
    }]));
    report.summary.harnessFailures = report.failures.length;
    await save();
    console.log(json({ ...report.summary, report: path.join(options.out, 'report.json') }));
    process.exitCode = report.failures.length || report.compiler.some(item => !item.complete) || report.projects.some(item => !item.complete) ? 2
      : [...report.compiler, ...report.projects, ...report.controls].some(item => !item.match) ? 1 : 0;
  }
}

if (process.argv.slice(2).some(arg => ['help', '--help', '-h'].includes(arg))) console.log(help);
else { try { await main(argumentsOf(process.argv.slice(2))); } catch (error) { console.error(error.stack ?? error.message); process.exitCode = 2; } }
