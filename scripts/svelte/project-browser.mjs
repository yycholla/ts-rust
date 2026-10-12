/** Focused browser-worker correctness. No application server or benchmark. */
import { createServer } from 'node:http';
import { readFile, writeFile, mkdir } from 'node:fs/promises';
import { resolve, extname } from 'node:path';
import { pathToFileURL } from 'node:url';

if (process.argv.length !== 5 || process.argv.includes('--help')) {
  console.log('Usage: node scripts/svelte/project-browser.mjs PLAYWRIGHT_PACKAGE DEPENDENCY_JSON NEW_OUTPUT');
  process.exit(process.argv.includes('--help') ? 0 : 2);
}
const [playwright, dependencies, output] = process.argv.slice(2).map(p => resolve(p));
await mkdir(output);
const webRoot = resolve('npm/svelte');
const { chromium } = await import(pathToFileURL(resolve(playwright, 'index.mjs')));
const server = createServer(async (request, response) => {
  try {
    const pathname = new URL(request.url, 'http://localhost').pathname;
    if (pathname === '/') {
      response.setHeader('content-type', 'text/html');
      response.end('<!doctype html><title>Native WASM project protocol</title>');
      return;
    }
    const path = pathname === '/dependencies.json' ? dependencies : resolve(webRoot, '.' + decodeURIComponent(pathname));
    if (path !== dependencies && !path.startsWith(webRoot + '/')) throw new Error('Outside fixture root');
    response.setHeader('content-type', extname(path) === '.wasm' ? 'application/wasm' : extname(path) === '.js' ? 'text/javascript' : 'application/json');
    response.end(await readFile(path));
  } catch {
    response.writeHead(404); response.end();
  }
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
let browser;
const report = { scope: 'Synthetic browser worker correctness only', tests: [], failures: [] };
try {
  browser = await chromium.launch({
    executablePath: '/nix/store/dypizpwkjf8v2vscas3ksz3945lhirkx-playwright-chromium-headless-shell/chrome-headless-shell-linux64/chrome-headless-shell',
    headless: true, args: ['--no-sandbox'],
  });
  const page = await browser.newPage();
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  report.tests = await page.evaluate(async () => {
    const { createSvelteProjectSession } = await import('/project.js');
    const files = await (await fetch('/dependencies.json')).json();
    const svelteVersion = JSON.parse(files['/project/node_modules/svelte/package.json']).version;
    const uri = 'file:///project/App.svelte';
    const broken = '<script lang="ts">let count: number = "wrong";</script><p>😀 {count}</p>';
    const good = broken.replace('"wrong"', '42');
    const snapshot = (text, version) => ({root:'/project',files,
      components:[{path:'App.svelte',document:{uri,text,version},typescript:true}],
      compatibility:{svelteVersion},runes:true,compilerOptions:{strict:true,skipLibCheck:true,module:'ESNext',target:'ESNext',moduleResolution:'Bundler'}});
    const project = await createSvelteProjectSession({
      toolingWasm: new URL('/generated/ts_svelte_tools_bg.wasm', location.href),
      semanticWasm: new URL('/generated/ts_svelte_semantic.wasm', location.href),
      ...snapshot(broken, -7),
    });
    const passed = [];
    try {
      const report = await project.request('textDocument/diagnostic',{textDocument:{uri}});
      if (!report.items.some(d=>d.code===2322)) throw new Error('Missing native component type error: '+JSON.stringify(report));
      passed.push('browser-rust-component-diagnostics');
      await project.replace(snapshot(good,-6));
      const repaired = await project.request('textDocument/diagnostic',{textDocument:{uri}});
      if (repaired.items.some(d=>d.code===2322)) throw new Error('Stale component diagnostic after edit');
      const hover = await project.request('textDocument/hover',{textDocument:{uri},position:{line:0,character:good.lastIndexOf('count')+1}});
      if (!JSON.stringify(hover).includes('number')) throw new Error('Missing mapped template hover');
      passed.push('browser-persistent-replacement-and-template-hover');
      const prepared = await project.prepared();
      if (!prepared.semanticComplete || prepared.revision !== 2) throw new Error('Incomplete browser checker snapshot');
      await project.shutdown();
      passed.push('browser-worker-shutdown');
    } finally { project.dispose(); }
    return passed;
  });
} catch (error) {
  report.failures.push(String(error.stack ?? error));
} finally {
  await browser?.close();
  await new Promise(resolve => server.close(resolve));
  await writeFile(resolve(output, 'report.json'), JSON.stringify(report, null, 2) + '\n');
}
console.log(JSON.stringify(report, null, 2));
process.exitCode = report.failures.length ? 1 : 0;
