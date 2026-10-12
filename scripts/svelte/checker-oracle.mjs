#!/usr/bin/env node
/** Isolated full-projection checking against the installed TypeScript oracle. */
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdir, readFile, realpath, symlink, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const fixtureRoot = path.join(root, 'scripts/svelte/fixtures/checker');
const help = `Usage: node scripts/svelte/checker-oracle.mjs --app PATH --compiler PATH --checker PATH --out NEW_DIR
Create isolated TSX overlays; compare native JSON checks with installed TypeScript 5.9.3.
Requires installed Svelte 5.53.11 and svelte-check shims. Does not load app config.
Output parent must exist; output directory must be new and outside the application.
Exit 0: bounded cases match and assertions pass; 1: differences/assertion failures;
2: incomplete checks or harness failure. No SvelteKit or general parity claim.`;
const json = (value) => `${JSON.stringify(value, null, 2)}\n`;
const sha = (bytes) => createHash('sha256').update(bytes).digest('hex');
const cases = [
  { name: 'valid-props', codes: [] },
  { name: 'wrong-props', codes: [2322] },
  { name: 'missing-props', codes: [2741, 2345] },
  { name: 'template-member', codes: [2339], mappedText: 'notARealMember' },
  { name: 'undefined-name', codes: [2304], mappedText: 'missingTemplateName' },
  { name: 'missing-import', codes: [2307] },
  { name: 'emoji', codes: [2304], mappedText: 'missingAfterEmoji' },
  { name: 'no-check-override', source: 'undefined-name', noCheck: true, codes: [2304], mappedText: 'missingTemplateName' },
  { name: 'javascript-jsdoc-valid', language: 'javascript', codes: [] },
  { name: 'javascript-jsdoc-mismatch', language: 'javascript', codes: [2322], mappedText: 'value' },
];

function argumentsOf(args) {
  const options = {};
  for (let index = 0; index < args.length; index++) {
    const flag = args[index];
    if (!['--app', '--compiler', '--checker', '--out'].includes(flag)) throw new Error(`Unknown argument ${flag}`);
    const name = flag.slice(2);
    if (options[name] || !args[index + 1] || args[index + 1].startsWith('--')) throw new Error(`Invalid or duplicate ${flag}`);
    options[name] = path.resolve(args[++index]);
  }
  for (const name of ['app', 'compiler', 'checker', 'out']) if (!options[name]) throw new Error(`Missing --${name}`);
  return options;
}

function processJson(binary, request) {
  const child = spawnSync(binary, [], { input: JSON.stringify(request), encoding: 'utf8', timeout: 180_000, maxBuffer: 32 * 1024 * 1024 });
  const result = { status: child.status, signal: child.signal, stderr: child.stderr ?? '', stdout: child.stdout ?? '' };
  if (child.error) result.processError = child.error.message;
  try { result.response = JSON.parse(result.stdout); }
  catch { result.parseError = 'Process stdout is not JSON'; }
  return result;
}

function maskAscii(text) {
  if (Buffer.byteLength(text) !== text.length) throw new Error('Declaration mask unexpectedly contains non-ASCII text');
  return text.replace(/[^\r\n]/g, ' ');
}

function maskedDeclarations(text) {
  const marker = "declare module '*.svelte' {";
  const start = text.indexOf(marker);
  if (start < 0 || text.indexOf(marker, start + marker.length) >= 0) throw new Error('Expected exactly one Svelte wildcard declaration');
  const closing = text.indexOf('\n}', start);
  if (closing < 0) throw new Error('Unterminated Svelte wildcard declaration');
  const end = closing + 2;
  return text.slice(0, start) + maskAscii(text.slice(start, end)) + text.slice(end);
}

function maskTypeReferences(code) {
  return code.replace(/^\/\/\/\s*<reference\s+types=["']svelte["']\s*\/>/, maskAscii);
}

function sorted(diagnostics) {
  return diagnostics.sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b)));
}

function filenameOf(file, directory, app) {
  if (!file) return null;
  if (file.startsWith('bundled:///libs/')) return `typescript-lib/${path.basename(file)}`;
  const relative = path.relative(directory, file);
  if (relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative)) return relative.split(path.sep).join('/');
  const library = path.join(app, 'node_modules/typescript/lib');
  if (path.dirname(file) === library) return `typescript-lib/${path.basename(file)}`;
  const appRelative = path.relative(app, file);
  if (appRelative !== '..' && !appRelative.startsWith(`..${path.sep}`) && !path.isAbsolute(appRelative)) return `app/${appRelative.split(path.sep).join('/')}`;
  return file;
}

function normalizeNative(diagnostic, directory, app) {
  const file = diagnostic.fileName || null;
  return {
    file: filenameOf(file, directory, app), start: file ? diagnostic.pos : null, end: file ? diagnostic.end : null,
    code: diagnostic.code, category: diagnostic.category, source: diagnostic.source ?? '', text: diagnostic.text,
    unnecessary: Boolean(diagnostic.reportsUnnecessary), deprecated: Boolean(diagnostic.reportsDeprecated),
    chain: (diagnostic.messageChain ?? []).map((item) => normalizeNative(item, directory, app)),
    related: (diagnostic.relatedInformation ?? []).map((item) => normalizeNative(item, directory, app)),
  };
}

function normalizeTs(diagnostic, directory, app) {
  const message = diagnostic.messageText;
  const messageObject = typeof message === 'object';
  return {
    file: filenameOf(diagnostic.file?.fileName, directory, app),
    start: diagnostic.file && diagnostic.start != null ? diagnostic.start : null,
    end: diagnostic.file && diagnostic.start != null ? diagnostic.start + (diagnostic.length ?? 0) : null,
    code: diagnostic.code, category: diagnostic.category, source: diagnostic.source ?? '',
    text: messageObject ? message.messageText : message,
    unnecessary: Boolean(diagnostic.reportsUnnecessary), deprecated: Boolean(diagnostic.reportsDeprecated),
    chain: (messageObject ? message.next ?? [] : diagnostic.next ?? []).map((item) => normalizeTs(item, directory, app)),
    related: (diagnostic.relatedInformation ?? []).map((item) => normalizeTs(item, directory, app)),
  };
}

function utf16ToByte(text, offset) {
  if (!Number.isInteger(offset) || offset < 0 || offset > text.length) return null;
  // A position between a surrogate pair cannot represent an exact UTF-8 boundary.
  if (offset > 0 && offset < text.length && /[\uD800-\uDBFF]/.test(text[offset - 1]) && /[\uDC00-\uDFFF]/.test(text[offset])) return null;
  return Buffer.byteLength(text.slice(0, offset));
}

function sourcePosition(text, byteOffset) {
  const bytes = Buffer.from(text);
  if (byteOffset < 0 || byteOffset > bytes.length) return null;
  if (bytes[byteOffset - 1] === 13 && bytes[byteOffset] === 10) return null;
  const prefix = bytes.subarray(0, byteOffset).toString('utf8');
  if (Buffer.byteLength(prefix) !== byteOffset) return null;
  const lines = prefix.split(/\r\n|\r|\n/);
  return { offset: prefix.length, line: lines.length - 1, character: lines.at(-1).length };
}

function withOriginal(diagnostic, projections) {
  let original = { status: diagnostic.file ? 'not-component' : 'project' };
  const projection = projections.get(diagnostic.file);
  if (projection) {
    const start = utf16ToByte(projection.code, diagnostic.start);
    const end = utf16ToByte(projection.code, diagnostic.end);
    const segment = start == null || end == null ? null : projection.mappings.find((mapping) => start >= mapping.generated.start && end <= mapping.generated.end);
    original = { status: 'unmapped', generatedBytes: start == null || end == null ? null : { start, end } };
    if (segment && segment.source.end - segment.source.start === segment.generated.end - segment.generated.start) {
      const sourceStart = segment.source.start + start - segment.generated.start;
      const sourceEnd = sourceStart + end - start;
      const startPosition = sourcePosition(projection.source, sourceStart);
      const endPosition = sourcePosition(projection.source, sourceEnd);
      if (startPosition && endPosition) original = { status: 'exact', file: projection.filename, bytes: { start: sourceStart, end: sourceEnd }, start: startPosition, end: endPosition };
    }
  }
  return { ...diagnostic, original, chain: diagnostic.chain.map((item) => withOriginal(item, projections)), related: diagnostic.related.map((item) => withOriginal(item, projections)) };
}

async function setupTypes(directory, app) {
  await symlink(path.join(app, 'node_modules'), path.join(directory, 'node_modules'), 'dir');
  const originalTypes = await readFile(path.join(app, 'node_modules/svelte/types/index.d.ts'), 'utf8');
  const copiedTypes = maskedDeclarations(originalTypes);
  await writeFile(path.join(directory, 'svelte-types.d.ts'), copiedTypes);
  const shim = await readFile(path.join(app, 'node_modules/svelte-check/dist/src/svelte-shims-v4.d.ts'));
  await writeFile(path.join(directory, 'svelte-shims-v4.d.ts'), shim);
  await mkdir(path.join(directory, 'type-roots/svelte'), { recursive: true });
  await writeFile(path.join(directory, 'type-roots/svelte/package.json'), json({ name: 'svelte', version: '0.0.0', types: './index.d.ts' }));
  await writeFile(path.join(directory, 'type-roots/svelte/index.d.ts'), '// Actual Svelte declarations are supplied explicitly by svelte-types.d.ts.\n');
  const paths = {};
  for (const match of originalTypes.matchAll(/declare module ['"]([^'"]+)['"]\s*\{/g)) {
    if (!match[1].includes('*')) paths[match[1]] = ['./svelte-types.d.ts'];
  }
  return {
    paths,
    files: ['svelte-types.d.ts', 'svelte-shims-v4.d.ts', path.join(app, 'node_modules/svelte/svelte-html.d.ts')],
    hashes: { originalSvelteTypes: sha(originalTypes), maskedSvelteTypes: sha(copiedTypes), shim: sha(shim), html: sha(await readFile(path.join(app, 'node_modules/svelte/svelte-html.d.ts'))) },
  };
}

async function runCase(test, options, ts) {
  const directory = path.join(options.out, test.name);
  await mkdir(directory);
  const result = { name: test.name, language: test.language ?? 'typescript', expectedCodes: test.codes, complete: false, match: false };
  const caseExtension = test.language === 'javascript' ? '.jsx' : '.tsx';
  const caseGenerated = `Case.svelte${caseExtension}`;
  try {
    const types = await setupTypes(directory, options.app);
    result.declarations = types.hashes;
    const projections = new Map();
    const files = [...types.files];
    for (const sourceName of ['Child.svelte', `${test.source ?? test.name}.svelte`]) {
      const text = await readFile(path.join(fixtureRoot, sourceName), 'utf8');
      const filename = sourceName === 'Child.svelte' ? sourceName : 'Case.svelte';
      const typescript = filename === 'Child.svelte' || test.language !== 'javascript';
      const projectionProcess = processJson(options.compiler, { op: 'project', document: { uri: filename, version: 1, text }, typescript, runes: true });
      await writeFile(path.join(directory, `${filename}.projection.json`), json(projectionProcess));
      const projection = projectionProcess.response;
      if (projectionProcess.status !== 0 || projectionProcess.processError || projection?.ok !== true || typeof projection.result?.code !== 'string' || !Array.isArray(projection.result.mappings)) throw new Error(`Projection incomplete for ${filename}`);
      const code = maskTypeReferences(projection.result.code);
      const generated = `${filename}${typescript ? '.tsx' : '.jsx'}`;
      await writeFile(path.join(directory, generated), code);
      const bridge = `export { default } from "./${generated}";\nexport * from "./${generated}";\n`;
      await writeFile(path.join(directory, `${filename}.d.ts`), bridge);
      await writeFile(path.join(directory, filename.replace(/\.svelte$/, '.d.svelte.ts')), bridge);
      files.push(generated);
      projections.set(generated, { filename, source: text, code, mappings: projection.result.mappings });
      (result.sources ??= {})[filename] = { language: typescript ? 'typescript' : 'javascript', generated, sha256: sha(text), generatedSha256: sha(code), backend: projection.result.backend };
    }
    const config = {
      compilerOptions: {
        strict: true, noEmit: true, target: 'ESNext', module: 'ESNext', moduleResolution: 'Bundler', jsx: 'preserve',
        allowArbitraryExtensions: true, allowImportingTsExtensions: true, types: [],
        allowJs: test.language === 'javascript', checkJs: test.language === 'javascript',
        noCheck: test.noCheck ?? false,
        typeRoots: [path.join(directory, 'type-roots'), path.join(options.app, 'node_modules/@types')],
        paths: types.paths,
      }, files,
    };
    const project = path.join(directory, 'tsconfig.json');
    await writeFile(project, json(config));
    const nativeRequest = { project, projections: [...projections].map(([file, projection]) => ({
      fileName: path.join(directory, file),
      document: { uri: projection.filename, version: 1, text: projection.source },
      code: projection.code, mappings: projection.mappings,
    })) };
    const nativeProcess = processJson(options.checker, nativeRequest);
    await writeFile(path.join(directory, 'native-raw.json'), json(nativeProcess));
    const native = nativeProcess.response;
    const nativeComplete = !nativeProcess.processError && !nativeProcess.signal && [0, 1].includes(nativeProcess.status)
      && native?.ok === true && Array.isArray(native.diagnostics) && Array.isArray(native.serializationFailures)
      && native.serializationFailures.length === 0 && Array.isArray(native.unported) && native.unported.length === 0;
    const parsed = ts.getParsedCommandLineOfConfigFile(project, { noCheck: false }, {
      ...ts.sys, onUnRecoverableConfigFileDiagnostic: (diagnostic) => { throw new Error(ts.flattenDiagnosticMessageText(diagnostic.messageText, '\n')); },
    });
    if (!parsed) throw new Error('TypeScript could not parse the overlay configuration');
    const program = ts.createProgram({ rootNames: parsed.fileNames, options: parsed.options, projectReferences: parsed.projectReferences });
    const oracleDiagnostics = [...parsed.errors, ...ts.getPreEmitDiagnostics(program)];
    result.oracle = sorted(oracleDiagnostics.map((item) => normalizeTs(item, directory, options.app)));
    result.native = Array.isArray(native?.diagnostics) ? sorted(native.diagnostics.map((item) => normalizeNative(item, directory, options.app))) : null;
    result.nativeExecution = { processStatus: nativeProcess.status, compilerStatus: native?.status, complete: nativeComplete, output: native?.output ?? null, serializationFailures: native?.serializationFailures ?? null, unported: native?.unported ?? null };
    result.complete = nativeComplete;
    const assertions = (diagnostics) => {
      if (!Array.isArray(diagnostics)) return false;
      return test.codes.length === 0 ? diagnostics.length === 0 : diagnostics.some((item) => test.codes.includes(item.code) && item.file === caseGenerated);
    };
    result.assertions = { oracle: assertions(result.oracle), native: assertions(result.native) };
    result.match = nativeComplete && result.assertions.oracle && result.assertions.native && JSON.stringify(result.oracle) === JSON.stringify(result.native);
    result.mappedOracle = result.oracle.map((item) => withOriginal(item, projections));
    result.mappedNative = result.native?.map((item) => withOriginal(item, projections)) ?? null;
    if (test.mappedText) {
      const source = Buffer.from(projections.get(caseGenerated).source);
      const mapsExpectedText = (diagnostics) => (diagnostics ?? []).some((item) => test.codes.includes(item.code)
        && item.original.status === 'exact' && item.original.file === 'Case.svelte'
        && source.subarray(item.original.bytes.start, item.original.bytes.end).toString('utf8') === test.mappedText);
      result.mappingAssertions = { expectedText: test.mappedText, oracle: mapsExpectedText(result.mappedOracle), native: mapsExpectedText(result.mappedNative) };
      result.mappingAssertions.rust = (native?.diagnostics ?? []).some(item => test.codes.includes(item.code)
        && item.original?.status === 'mapped' && item.original.uri === 'Case.svelte'
        && source.subarray(item.original.bytes.start, item.original.bytes.end).toString('utf8') === test.mappedText);
      if (!result.mappingAssertions.oracle || !result.mappingAssertions.native || !result.mappingAssertions.rust) result.match = false;
    }
    result.mappingSummary = {
      oracle: result.mappedOracle.reduce((counts, item) => { counts[item.original.status] = (counts[item.original.status] ?? 0) + 1; return counts; }, {}),
      native: (result.mappedNative ?? []).reduce((counts, item) => { counts[item.original.status] = (counts[item.original.status] ?? 0) + 1; return counts; }, {}),
    };
    if (test.name === 'valid-props') {
      const stale = structuredClone(nativeRequest);
      stale.projections[0].code += '\n';
      const staleProcess = processJson(options.checker, stale);
      await writeFile(path.join(directory, 'stale-projection-raw.json'), json(staleProcess));
      result.staleProjectionRejected = staleProcess.response?.ok === false
        && staleProcess.response?.error?.code === 'incomplete_check'
        && staleProcess.response?.serializationFailures?.some(message => message.includes('does not match loaded checker source'));
      if (!result.staleProjectionRejected) result.match = false;
    }
  } catch (error) { result.complete = false; result.match = false; result.failure = error.message; }
  await writeFile(path.join(directory, 'report.json'), json(result));
  return result;
}

async function main(options) {
  options.app = await realpath(options.app);
  options.compiler = await realpath(options.compiler);
  options.checker = await realpath(options.checker);
  options.out = path.join(await realpath(path.dirname(options.out)), path.basename(options.out));
  const relative = path.relative(options.app, options.out);
  if (relative === '' || (relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative))) throw new Error('Output must be outside the read-only application');
  await mkdir(options.out);
  const report = { scope: 'TypeScript checks of full Rust Svelte projections; no app config, SvelteKit, editor, or general parity claim', cases: [] };
  try {
    const require = createRequire(import.meta.url);
    const tsPath = path.join(options.app, 'node_modules/typescript/lib/typescript.js');
    const ts = require(tsPath);
    const sveltePackage = JSON.parse(await readFile(path.join(options.app, 'node_modules/svelte/package.json'), 'utf8'));
    if (ts.version !== '5.9.3' || sveltePackage.version !== '5.53.11') throw new Error(`Expected TypeScript 5.9.3/Svelte 5.53.11; found ${ts.version}/${sveltePackage.version}`);
    const source = spawnSync('python3', [path.join(root, 'scripts/svelte/source-identity.py')], { cwd: root, encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 });
    if (source.status !== 0) throw new Error(`Source identity failed: ${source.stderr}`);
    await writeFile(path.join(options.out, 'source-identity.json'), source.stdout);
    report.identity = {
      startedAt: new Date().toISOString(), node: process.version, typescript: ts.version, svelte: sveltePackage.version,
      compiler: { path: options.compiler, sha256: sha(await readFile(options.compiler)) },
      checker: { path: options.checker, sha256: sha(await readFile(options.checker)) },
      typescriptEntrySha256: sha(await readFile(tsPath)),
    };
    for (const test of cases) {
      report.cases.push(await runCase(test, options, ts));
      await writeFile(path.join(options.out, 'report.json'), json(report));
    }
    report.summary = { cases: report.cases.length, matched: report.cases.filter((item) => item.match).length, incomplete: report.cases.filter((item) => !item.complete).length, differences: report.cases.filter((item) => item.complete && !item.match).length };
    report.completedAt = new Date().toISOString();
    process.exitCode = report.summary.incomplete ? 2 : report.summary.differences ? 1 : 0;
  } catch (error) { report.failure = error.message; process.exitCode = 2; }
  await writeFile(path.join(options.out, 'report.json'), json(report));
  console.log(json({ ...report.summary, failure: report.failure, report: path.join(options.out, 'report.json') }));
}

if (process.argv.slice(2).some((item) => ['--help', 'help', '-h'].includes(item))) console.log(help);
else {
  try { await main(argumentsOf(process.argv.slice(2))); }
  catch (error) { console.error(`checker-oracle: ${error.message}`); process.exitCode = 2; }
}
