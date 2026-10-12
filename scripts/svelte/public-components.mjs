#!/usr/bin/env node
/** Usage: node scripts/svelte/public-components.mjs PROJECT NEW_OUTPUT
 * Compare every src component's client/server diagnostics with its installed
 * compiler and our supported Svelte 5.53.11. No preprocessing/config execution,
 * warning filtering, bundling, type checking, or emitted-code equivalence claim.
 */
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';

if (process.argv.slice(2).some(x => ['help', '--help'].includes(x))) {
  console.log('Usage: node scripts/svelte/public-components.mjs PROJECT NEW_OUTPUT');
  process.exit(0);
}
if (process.argv.length !== 4) throw new Error('Expected PROJECT NEW_OUTPUT');
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const project = path.resolve(process.argv[2]);
const out = path.resolve(process.argv[3]);
const supported = '/home/yycholla/Documents/MoncuraHR';
const hash = x => createHash('sha256').update(x).digest('hex');
const json = x => JSON.stringify(x, null, 2) + '\n';
await mkdir(out);
async function walk(dir) {
  const files = [];
  for (const entry of (await readdir(dir, {withFileTypes:true})).sort((a,b)=>a.name.localeCompare(b.name))) {
    const p = path.join(dir, entry.name);
    if (entry.isDirectory() && !['node_modules', '.svelte-kit'].includes(entry.name)) files.push(...await walk(p));
    else if (entry.isFile() && p.endsWith('.svelte')) files.push(p);
  }
  return files;
}
async function compiler(base) {
  try {
    const pkg = JSON.parse(await readFile(path.join(base, 'node_modules/svelte/package.json'), 'utf8'));
    return {version:pkg.version, module:await import(pathToFileURL(path.join(base, 'node_modules/svelte/src/compiler/index.js')))};
  } catch (error) {
    if (error.code !== 'ENOENT' && error.code !== 'ERR_MODULE_NOT_FOUND') throw error;
    return null;
  }
}
const oracleProject = await compiler(project);
const oracleSupported = await compiler(supported);
if (oracleSupported?.version !== '5.53.11') throw new Error('Missing supported 5.53.11 oracle');
// RealWorld explicitly sets runes=true. The two libraries infer mode per file.
const runes = path.basename(project).startsWith('realworld') ? true : undefined;
const options = {dev:false, css:'external', ...(runes === undefined ? {} : {runes})};
const sort = xs => xs.sort((a,b)=>JSON.stringify(a).localeCompare(JSON.stringify(b)));
function diagnostic(d, severity, text) {
  const offset = p => p == null ? null : Buffer.byteLength(text.slice(0, typeof p === 'number' ? p : p.character));
  return {code:d.code, severity, message:d.message,
    bytes:d.start == null && d.end == null ? null : {start:offset(d.start), end:offset(d.end)}};
}
function compileOracle(compiler, text, filename, target) {
  if (!compiler) return {unrun:'Project dependencies unavailable'};
  const start = performance.now();
  try {
    const result = compiler.module.compile(text, {filename, generate:target, ...options});
    return {ok:true, diagnostics:sort(result.warnings.map(d=>diagnostic(d,'warning',text))), runes:result.metadata.runes,
      jsSha256:hash(result.js.code), cssSha256:hash(result.css?.code ?? ''), milliseconds:performance.now()-start};
  } catch (error) {
    if (!error.code) return {harnessFailure:String(error), milliseconds:performance.now()-start};
    return {ok:false, diagnostics:[diagnostic(error,'error',text)], milliseconds:performance.now()-start};
  }
}
function native(text, filename, target) {
  const start = performance.now();
  const child = spawnSync(path.join(root,'target/release/svelte-tool'), [], {
    input:JSON.stringify({op:'compile',document:{uri:filename,version:1,text},target,dev:false,...(runes===undefined?{}:{runes})}),
    encoding:'utf8',timeout:30000,maxBuffer:32*1024*1024});
  const milliseconds=performance.now()-start;
  if (child.error || child.signal) return {harnessFailure:child.error?.message ?? child.signal,milliseconds};
  let response;
  try {response=JSON.parse(child.stdout);} catch {return {harnessFailure:'Invalid JSON',stderr:child.stderr,milliseconds};}
  const body = response.ok ? response.result : response;
  const result = {ok:response.ok,error:response.error,exitCode:child.status,stderr:child.stderr,milliseconds};
  if (typeof response.ok !== 'boolean' || !body || (!Array.isArray(body.diagnostics) && (response.ok || typeof response.error?.code !== 'string'))) {
    return {...result,harnessFailure:'Invalid native response contract'};
  }
  if (Array.isArray(body.diagnostics)) result.diagnostics=sort(body.diagnostics.map(d=>({code:d.code,severity:d.severity,message:d.message,bytes:d.bytes})));
  if (response.ok) {
    if (child.status!==0 || typeof body.javascript?.code!=='string') result.harnessFailure='Invalid success contract';
    result.runes=body.runes;
    result.jsSha256=hash(body.javascript?.code ?? '');
    result.cssSha256=hash(body.css?.code ?? '');
  }
  return result;
}
function compare(a,b) {
  if (a.unrun) return 'unrun';
  if (a.harnessFailure || b.harnessFailure) return 'harnessFailure';
  return a.ok===b.ok && JSON.stringify(a.diagnostics)===JSON.stringify(b.diagnostics) && (!a.ok || a.runes===b.runes) ? 'match' : 'difference';
}
const report={scope:'Raw component compile diagnostics, no project build/check/lint or runtime equivalence',project,
  projectVersion:oracleProject?.version ?? null,supportedVersion:oracleSupported.version,options,
  runtimeGuard:'Native emitted client helpers are guarded for 5.53.11 even when comparing another compiler version',
  timing:'Oracle milliseconds exclude process startup; native milliseconds include one process per component. Not comparable performance.',
  files:[],cases:[]};
for (const filename of await walk(path.join(project,'src'))) {
  const relative=path.relative(project,filename);
  const text=await readFile(filename,'utf8');
  report.files.push({file:relative,sha256:hash(text)});
  for (const target of ['client','server']) {
    const result={file:relative,target,official:compileOracle(oracleProject,text,relative,target),
      supported:compileOracle(oracleSupported,text,relative,target),native:native(text,relative,target)};
    result.projectComparison=compare(result.official,result.native);
    result.supportedComparison=compare(result.supported,result.native);
    report.cases.push(result);
  }
}
report.summary={files:report.files.length,cases:report.cases.length};
for (const field of ['projectComparison','supportedComparison']) {
  report.summary[field]=Object.fromEntries(['match','difference','harnessFailure','unrun'].map(status=>[status,report.cases.filter(c=>c[field]===status).length]));
}
for (const field of ['official','supported','native']) {
  report.summary[field]={success:report.cases.filter(c=>c[field].ok===true).length,errors:report.cases.filter(c=>c[field].ok===false).length};
}
await writeFile(path.join(out,'report.json'),json(report));
console.log(json(report.summary));
process.exitCode=report.cases.some(c=>c.supportedComparison!=='match' || c.projectComparison==='difference' || c.projectComparison==='harnessFailure') ? 1 : 0;
