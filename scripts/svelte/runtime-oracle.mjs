#!/usr/bin/env node
/** Isolated SSR/hydration interaction matrix. This uses JSDOM, not a browser. */
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdir, readFile, realpath, symlink, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const fixturePath = path.join(root, 'scripts/svelte/fixtures/runtime/Counter.svelte');
const help = `Usage: node scripts/svelte/runtime-oracle.mjs --app PATH --compiler PATH --out NEW_DIR
Compile Counter.svelte with native Rust and installed Svelte 5.53.11; render each
server artifact and hydrate all four server/client combinations with installed JSDOM.
Checks typed props, state/derived state, click updates, node reuse, and unmount.
Parses emitted client helper references and installed runtime exports with TypeScript.
Requires explicit runtime_compatibility rejection for unsupported client controls.
No app configuration is executed. No browser, application, or performance parity claim.
Output parent must exist; output must be new and outside the application checkout.
Exit 0 = all measured cases pass; 1 = failures; 2 = setup/harness failure.`;
const json = (value) => `${JSON.stringify(value, null, 2)}\n`;
const hash = (data) => createHash('sha256').update(data).digest('hex');
const props = { step: 2, label: 'Count' };
const expected = { initial: 'Count: 0 / 0', afterClick: 'Count: 2 / 4' };

function parseArgs(args) {
  const options = {};
  for (let index = 0; index < args.length; index++) {
    const flag = args[index];
    if (!['--app', '--compiler', '--out'].includes(flag)) throw new Error(`Unknown argument ${flag}`);
    const key = flag.slice(2);
    if (options[key] || !args[index + 1] || args[index + 1].startsWith('--')) throw new Error(`Invalid or duplicate ${flag}`);
    options[key] = path.resolve(args[++index]);
  }
  for (const key of ['app', 'compiler', 'out']) if (!options[key]) throw new Error(`Missing --${key}`);
  return options;
}

function processResult(command, args, input, cwd, timeout = 60_000) {
  const child = spawnSync(command, args, { input: JSON.stringify(input), cwd, encoding: 'utf8', timeout, maxBuffer: 16 * 1024 * 1024 });
  const result = { status: child.status, signal: child.signal, stdout: child.stdout ?? '', stderr: child.stderr ?? '' };
  if (child.error) result.processError = child.error.message;
  try { result.response = JSON.parse(result.stdout); }
  catch { result.parseError = 'stdout is not a JSON response'; }
  result.complete = !result.processError && !result.signal && !result.parseError;
  return result;
}

function parseJavaScript(ts, filename, code) {
  const source = ts.createSourceFile(filename, code, ts.ScriptTarget.ESNext, true, ts.ScriptKind.JS);
  return { source, diagnostics: source.parseDiagnostics.map((diagnostic) => ({
    code: diagnostic.code, start: diagnostic.start ?? null, length: diagnostic.length ?? null,
    message: ts.flattenDiagnosticMessageText(diagnostic.messageText, '\n'),
  })) };
}

function runtimeExportNames(ts, filename, code) {
  const parsed = parseJavaScript(ts, filename, code);
  const names = new Set();
  const unsupported = [];
  for (const statement of parsed.source.statements) {
    if (ts.isExportDeclaration(statement)) {
      if (statement.exportClause && ts.isNamedExports(statement.exportClause)) {
        for (const entry of statement.exportClause.elements) names.add(entry.name.text);
      } else unsupported.push({ start: statement.getStart(parsed.source), reason: 'Export-star or namespace export requires resolution' });
    } else if (statement.modifiers?.some((modifier) => modifier.kind === ts.SyntaxKind.ExportKeyword)) {
      if (statement.name && ts.isIdentifier(statement.name)) names.add(statement.name.text);
      else if (ts.isVariableStatement(statement)) {
        for (const declaration of statement.declarationList.declarations) {
          if (ts.isIdentifier(declaration.name)) names.add(declaration.name.text);
          else unsupported.push({ start: declaration.getStart(parsed.source), reason: 'Destructured exported binding requires resolution' });
        }
      } else unsupported.push({ start: statement.getStart(parsed.source), reason: 'Unknown exported declaration' });
    }
  }
  return { names, diagnostics: parsed.diagnostics, unsupported };
}

function auditClientHelpers(ts, filename, code, runtime) {
  const parsed = parseJavaScript(ts, filename, code);
  const namespaces = new Set();
  const uses = [];
  const unresolved = [];
  const importedNamespaceNodes = new Set();
  for (const statement of parsed.source.statements) {
    if (!ts.isImportDeclaration(statement) || !ts.isStringLiteral(statement.moduleSpecifier)
      || statement.moduleSpecifier.text !== 'svelte/internal/client') continue;
    if (statement.importClause?.name) uses.push({ name: 'default', start: statement.importClause.name.getStart(parsed.source), form: 'default-import' });
    const bindings = statement.importClause?.namedBindings;
    if (bindings && ts.isNamespaceImport(bindings)) {
      namespaces.add(bindings.name.text);
      importedNamespaceNodes.add(bindings.name);
    } else if (bindings && ts.isNamedImports(bindings)) {
      for (const element of bindings.elements) uses.push({ name: (element.propertyName ?? element.name).text, start: element.getStart(parsed.source), form: 'named-import' });
    }
  }
  function visit(node) {
    if (ts.isIdentifier(node) && namespaces.has(node.text) && !importedNamespaceNodes.has(node)) {
      const parent = node.parent;
      if (ts.isPropertyAccessExpression(parent) && parent.expression === node) {
        uses.push({ name: parent.name.text, start: parent.getStart(parsed.source), form: 'namespace-property' });
      } else if (ts.isElementAccessExpression(parent) && parent.expression === node) {
        if (parent.argumentExpression && ts.isStringLiteralLike(parent.argumentExpression)) uses.push({ name: parent.argumentExpression.text, start: parent.getStart(parsed.source), form: 'namespace-element' });
        else unresolved.push({ start: parent.getStart(parsed.source), reason: 'Dynamic runtime helper access' });
      } else {
        unresolved.push({ start: node.getStart(parsed.source), reason: 'Namespace escape or shadowing requires binding analysis' });
      }
    }
    ts.forEachChild(node, visit);
  }
  visit(parsed.source);
  const missing = [...new Set(uses.filter((use) => !runtime.names.has(use.name)).map((use) => use.name))].sort();
  return { ok: parsed.diagnostics.length === 0 && runtime.diagnostics.length === 0 && runtime.unsupported.length === 0 && missing.length === 0 && unresolved.length === 0,
    method: 'TypeScript AST; namespace properties and named imports, fail closed for unresolved accesses; does not validate helper argument ABI',
    namespaces: [...namespaces].sort(), uses, missing, unresolved, parseDiagnostics: parsed.diagnostics };
}

// Every invocation gets a fresh Node process and a fresh Svelte module graph.
const serverProgram = `
import { readFileSync } from 'node:fs';
import { format } from 'node:util';
const request = JSON.parse(readFileSync(0, 'utf8'));
const warnings = [], errors = [], logs = [];
console.warn = (...args) => warnings.push(format(...args));
console.error = (...args) => errors.push(format(...args));
console.log = (...args) => logs.push(format(...args));
const result = { ok: false, warnings, errors, logs };
try {
  const { render } = await import('svelte/server');
  const { default: Component } = await import(request.module);
  const rendered = render(Component, { props: request.props });
  result.body = rendered.body;
  result.head = rendered.head;
  result.ok = warnings.length === 0 && errors.length === 0;
} catch (error) { result.failure = error.stack ?? String(error); }
process.stdout.write(JSON.stringify(result));
process.exitCode = result.ok ? 0 : 1;
`;

const clientProgram = `
import { readFileSync } from 'node:fs';
import { format } from 'node:util';
const request = JSON.parse(readFileSync(0, 'utf8'));
const warnings = [], errors = [], logs = [];
console.warn = (...args) => warnings.push(format(...args));
console.error = (...args) => errors.push(format(...args));
console.log = (...args) => logs.push(format(...args));
const result = { ok: false, environment: 'jsdom', warnings, errors, logs };
let dom, component, svelte;
process.on('unhandledRejection', (error) => errors.push(error?.stack ?? String(error)));
try {
  const { JSDOM, VirtualConsole } = await import('jsdom');
  const virtualConsole = new VirtualConsole();
  virtualConsole.on('jsdomError', (error) => errors.push(error.stack ?? String(error)));
  dom = new JSDOM('<!doctype html><html><head>' + request.head + '</head><body><div id="app">' + request.body + '</div></body></html>', {
    url: 'http://localhost/', pretendToBeVisual: true, virtualConsole
  });
  const window = dom.window;
  // Install DOM globals before importing either Svelte or the compiled client.
  for (const name of ['window', 'document', 'navigator', 'Node', 'Element', 'HTMLElement', 'HTMLButtonElement', 'Text', 'Comment', 'Event', 'MouseEvent', 'CustomEvent', 'Document', 'DocumentFragment', 'SVGElement', 'MutationObserver']) {
    Object.defineProperty(globalThis, name, { value: name === 'window' ? window : window[name], writable: true, configurable: true });
  }
  globalThis.getComputedStyle = window.getComputedStyle.bind(window);
  globalThis.requestAnimationFrame = window.requestAnimationFrame.bind(window);
  globalThis.cancelAnimationFrame = window.cancelAnimationFrame.bind(window);
  window.addEventListener('error', (event) => errors.push(event.error?.stack ?? event.message));
  svelte = await import('svelte');
  const { default: Component } = await import(request.module);
  const target = window.document.getElementById('app');
  const serverButton = target.querySelector('button');
  if (!serverButton) throw new Error('SSR output does not contain the expected button');
  result.serverText = serverButton.textContent;
  component = svelte.hydrate(Component, { target, props: request.props, recover: false });
  svelte.flushSync();
  const button = target.querySelector('button');
  result.reusedServerNode = button === serverButton;
  result.initialText = button?.textContent ?? null;
  if (!result.reusedServerNode) throw new Error('Hydration replaced the SSR button');
  if (result.serverText !== request.expected.initial || result.initialText !== request.expected.initial) throw new Error('Initial SSR/hydrated text differs from expected state');
  button.dispatchEvent(new window.MouseEvent('click', { bubbles: true, cancelable: true }));
  svelte.flushSync();
  result.afterClickText = button.textContent;
  if (result.afterClickText !== request.expected.afterClick) throw new Error('Click did not update state and derived state as expected');
  await svelte.unmount(component);
  component = null;
  svelte.flushSync();
  result.cleanup = { buttonDisconnected: !button.isConnected, elements: target.querySelectorAll('*').length, text: target.textContent, html: target.innerHTML };
  if (!result.cleanup.buttonDisconnected || result.cleanup.elements !== 0 || result.cleanup.text !== '') throw new Error('Unmount left rendered elements or text in the target');
  await new Promise((resolve) => setImmediate(resolve));
  result.ok = warnings.length === 0 && errors.length === 0;
  if (!result.ok) result.failure = 'Runtime emitted warnings or errors; hydration mismatch is not accepted';
} catch (error) { result.failure = error.stack ?? String(error); }
finally {
  if (component && svelte) {
    try { await svelte.unmount(component); } catch (error) { errors.push('Cleanup failure: ' + String(error)); }
  }
  dom?.window.close();
}
if (warnings.length || errors.length) result.ok = false;
process.stdout.write(JSON.stringify(result));
process.exitCode = result.ok ? 0 : 1;
`;

function successful(process) {
  return process.complete && process.status === 0 && process.response?.ok === true;
}

async function main(options) {
  options.app = await realpath(options.app);
  options.compiler = await realpath(options.compiler);
  options.out = path.join(await realpath(path.dirname(options.out)), path.basename(options.out));
  const relative = path.relative(options.app, options.out);
  if (relative === '' || (relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative))) throw new Error('Output must be outside the read-only application');
  await mkdir(options.out);
  const report = { environment: 'JSDOM; no real-browser verification', scope: 'counter SSR/hydration matrix and explicit unsupported-client controls', props, expected, compilation: [], server: {}, hydration: [], unsupportedClientControls: [] };
  const save = () => writeFile(path.join(options.out, 'report.json'), json(report));
  try {
    const sveltePackage = JSON.parse(await readFile(path.join(options.app, 'node_modules/svelte/package.json'), 'utf8'));
    const jsdomPackage = JSON.parse(await readFile(path.join(options.app, 'node_modules/jsdom/package.json'), 'utf8'));
    if (sveltePackage.version !== '5.53.11') throw new Error(`Expected Svelte 5.53.11; found ${sveltePackage.version}`);
    const compilerPath = path.join(options.app, 'node_modules/svelte/src/compiler/index.js');
    const compiler = await import(pathToFileURL(compilerPath).href);
    const tsPath = path.join(options.app, 'node_modules/typescript/lib/typescript.js');
    const ts = createRequire(import.meta.url)(tsPath);
    if (ts.version !== '5.9.3') throw new Error(`Expected TypeScript parser 5.9.3; found ${ts.version}`);
    const clientRuntimePath = path.join(options.app, 'node_modules/svelte/src/internal/client/index.js');
    const clientRuntimeText = await readFile(clientRuntimePath, 'utf8');
    const clientRuntime = runtimeExportNames(ts, clientRuntimePath, clientRuntimeText);
    await writeFile(path.join(options.out, 'svelte-client-index.js'), clientRuntimeText);
    report.runtimeExports = { path: clientRuntimePath, sha256: hash(clientRuntimeText), names: [...clientRuntime.names].sort(), parseDiagnostics: clientRuntime.diagnostics, unsupported: clientRuntime.unsupported };
    if (clientRuntime.diagnostics.length || clientRuntime.unsupported.length) throw new Error('Cannot completely resolve the installed client runtime export index');
    const knownMissing = auditClientHelpers(ts, 'missing-helper.mjs', "import * as runtime from 'svelte/internal/client'; runtime.only_child(node);", clientRuntime);
    const stringOnly = auditClientHelpers(ts, 'string-helper.mjs', "import * as runtime from 'svelte/internal/client'; const text = 'runtime.only_child(node)'; /* runtime.only_child(node) */ runtime.child(node);", clientRuntime);
    report.helperAuditControls = { knownMissing, stringOnly, ok: !knownMissing.ok && knownMissing.missing.includes('only_child') && stringOnly.ok };
    if (!report.helperAuditControls.ok) throw new Error('AST helper audit controls failed');
    const text = await readFile(fixturePath, 'utf8');
    const source = spawnSync('python3', [path.join(root, 'scripts/svelte/source-identity.py')], { cwd: root, encoding: 'utf8', maxBuffer: 8 * 1024 * 1024 });
    if (source.status !== 0) throw new Error(`Source identity failed: ${source.stderr}`);
    await writeFile(path.join(options.out, 'source-identity.json'), source.stdout);
    report.identity = {
      startedAt: new Date().toISOString(), node: process.version, svelte: sveltePackage.version, jsdom: jsdomPackage.version,
      fixture: { path: path.relative(root, fixturePath), sha256: hash(text) },
      nativeCompiler: { path: options.compiler, sha256: hash(await readFile(options.compiler)) },
      oracleCompiler: { path: compilerPath, sha256: hash(await readFile(compilerPath)) },
      auditParser: { version: ts.version, sha256: hash(await readFile(tsPath)) },
      compileOptions: { filename: 'Counter.svelte', runes: true, dev: false, css: 'external' },
    };
    const runtime = path.join(options.out, 'runtime');
    await mkdir(runtime);
    await symlink(path.join(options.app, 'node_modules'), path.join(runtime, 'node_modules'), 'dir');
    const artifacts = new Map();
    for (const side of ['oracle', 'native']) {
      for (const target of ['server', 'client']) {
        const compilation = { side, target, ok: false };
        try {
          let code, map, diagnostics, backend;
          if (side === 'oracle') {
            const artifact = compiler.compile(text, { ...report.identity.compileOptions, generate: target });
            code = artifact.js.code;
            map = artifact.js.map;
            diagnostics = artifact.warnings;
          } else {
            const process = processResult(options.compiler, [], { op: 'compile', document: { uri: 'Counter.svelte', version: 1, text }, target, runes: true, dev: false }, root);
            await writeFile(path.join(options.out, `native-${target}.json`), json(process));
            if (!successful(process)) throw new Error(`Native ${target} compilation failed; see native-${target}.json`);
            code = process.response.result.javascript.code;
            map = process.response.result.javascript.map;
            diagnostics = process.response.result.diagnostics;
            backend = process.response.result.backend;
          }
          if (typeof code !== 'string') throw new Error('Compilation returned no JavaScript');
          const filename = path.join(runtime, `Counter.${side}.${target}.mjs`);
          await writeFile(filename, code);
          await writeFile(`${filename}.map.json`, json(map ?? null));
          Object.assign(compilation, { artifact: path.relative(options.out, filename), sha256: hash(code), diagnostics, backend });
          if (target === 'client') {
            compilation.helperAudit = auditClientHelpers(ts, filename, code, clientRuntime);
            if (!compilation.helperAudit.ok) throw new Error('Emitted client runtime helper audit failed; artifact retained');
          }
          artifacts.set(`${side}/${target}`, filename);
          compilation.ok = true;
        } catch (error) { compilation.failure = error.stack ?? String(error); }
        report.compilation.push(compilation);
        await save();
      }
    }
    for (const control of [
      { name: 'development-client', filename: 'Counter.svelte', source: text, dev: true },
    ]) {
      const entry = { name: control.name, filename: control.filename, sourceSha256: hash(control.source), dev: control.dev, expectedError: 'runtime_compatibility', ok: false };
      try {
        await writeFile(path.join(options.out, `${control.name}.svelte`), control.source);
        const oracle = compiler.compile(control.source, { filename: control.filename, generate: 'client', runes: true, dev: control.dev, css: 'external' });
        const oraclePath = path.join(runtime, `${control.name}.oracle.client.mjs`);
        await writeFile(oraclePath, oracle.js.code);
        await writeFile(`${oraclePath}.map.json`, json(oracle.js.map ?? null));
        entry.oracle = { artifact: path.relative(options.out, oraclePath), sha256: hash(oracle.js.code), diagnostics: oracle.warnings, helperAudit: auditClientHelpers(ts, oraclePath, oracle.js.code, clientRuntime) };
        const nativeProcess = processResult(options.compiler, [], { op: 'compile', document: { uri: control.filename, version: 1, text: control.source }, target: 'client', runes: true, dev: control.dev }, root);
        await writeFile(path.join(options.out, `${control.name}.native.json`), json(nativeProcess));
        entry.native = nativeProcess;
        // Retain and audit a wrongly successful artifact; never execute unsupported controls.
        if (typeof nativeProcess.response?.result?.javascript?.code === 'string') {
          const code = nativeProcess.response.result.javascript.code;
          const nativePath = path.join(runtime, `${control.name}.native.client.mjs`);
          await writeFile(nativePath, code);
          await writeFile(`${nativePath}.map.json`, json(nativeProcess.response.result.javascript.map ?? null));
          entry.unexpectedNativeArtifact = { artifact: path.relative(options.out, nativePath), sha256: hash(code), helperAudit: auditClientHelpers(ts, nativePath, code, clientRuntime) };
        }
        entry.ok = entry.oracle.helperAudit.ok && nativeProcess.complete && nativeProcess.status === 1
          && nativeProcess.response?.ok === false && nativeProcess.response?.error?.code === 'runtime_compatibility'
          && !entry.unexpectedNativeArtifact;
      } catch (error) { entry.failure = error.stack ?? String(error); }
      report.unsupportedClientControls.push(entry);
      await save();
    }
    for (const side of ['oracle', 'native']) {
      const artifact = artifacts.get(`${side}/server`);
      report.server[side] = artifact
        ? processResult(process.execPath, ['--input-type=module', '-e', serverProgram], { module: pathToFileURL(artifact).href, props }, runtime)
        : { complete: false, unrun: 'Server artifact unavailable' };
      await save();
    }
    report.serverAgreement = successful(report.server.oracle) && successful(report.server.native)
      && report.server.oracle.response.body === report.server.native.response.body
      && report.server.oracle.response.head === report.server.native.response.head;
    for (const server of ['oracle', 'native']) {
      for (const client of ['oracle', 'native']) {
        const artifact = artifacts.get(`${client}/client`);
        const serverResult = report.server[server];
        const entry = { server, client, ok: false };
        if (!artifact || !successful(serverResult)) entry.unrun = 'Client artifact or successful SSR result unavailable';
        else {
          entry.process = processResult(process.execPath, ['--conditions=browser', '--input-type=module', '-e', clientProgram], {
            module: pathToFileURL(artifact).href, body: serverResult.response.body, head: serverResult.response.head, props, expected,
          }, runtime);
          entry.ok = successful(entry.process);
        }
        report.hydration.push(entry);
        await save();
      }
    }
    report.summary = {
      compilations: report.compilation.length, compilationFailures: report.compilation.filter((item) => !item.ok).length,
      serverAgreement: report.serverAgreement, hydrationCases: report.hydration.length,
      hydrationPassed: report.hydration.filter((item) => item.ok).length,
      hydrationUnrun: report.hydration.filter((item) => item.unrun).length,
      unsupportedClientControls: report.unsupportedClientControls.length,
      unsupportedClientControlsPassed: report.unsupportedClientControls.filter((item) => item.ok).length,
    };
    report.ok = report.summary.compilationFailures === 0 && report.serverAgreement && report.summary.hydrationPassed === 4
      && report.summary.unsupportedClientControlsPassed === 1;
    process.exitCode = report.ok ? 0 : 1;
  } catch (error) { report.failure = error.stack ?? String(error); report.ok = false; process.exitCode = 2; }
  report.completedAt = new Date().toISOString();
  await save();
  console.log(json({ ok: report.ok, ...report.summary, failure: report.failure, report: path.join(options.out, 'report.json') }));
}

if (process.argv.slice(2).some((arg) => ['--help', 'help', '-h'].includes(arg))) console.log(help);
else {
  try { await main(parseArgs(process.argv.slice(2))); }
  catch (error) { console.error(`runtime-oracle: ${error.message}`); process.exitCode = 2; }
}
