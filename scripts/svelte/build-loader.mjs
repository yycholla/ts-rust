// Version-pinned host hook. Both compile calls must be awaited; no JS fallback.
import { readFile } from 'node:fs/promises';
const entries = new Map([
  ['utils/compile.js', ['svelte.compile(finalCode,', 'await svelte.compile(finalCode,']],
  ['plugins/compile-module.js', ['svelteCompiler.compileModule(code,', 'await svelteCompiler.compileModule(code,']]
]);
function entry(url) { return [...entries.keys()].find(p => url.endsWith('/@sveltejs/vite-plugin-svelte/src/' + p)); }
export async function resolve(specifier,context,nextResolve){
  if(specifier==='svelte/compiler' && entry(context.parentURL||'')){
    return {url:new URL('../../npm/svelte-kit/native-compiler.mjs',import.meta.url).href,shortCircuit:true};
  }
  if(specifier==='svelte/compiler' && (context.parentURL||'').includes('/@sveltejs/vite-plugin-svelte/')) {
    const original=await nextResolve(specifier,context);
    const source=`export * from ${JSON.stringify(original.url)}; export function compile(){throw new Error('Unregistered native component compiler callsite');} export function compileModule(){throw new Error('Unregistered native module compiler callsite');}`;
    return {url:'data:text/javascript,'+encodeURIComponent(source),shortCircuit:true};
  }
  return nextResolve(specifier,context);
}
export async function load(url,context,nextLoad){
  const name=entry(url);
  if(!name)return nextLoad(url,context);
  const packageUrl=new URL('../'.repeat(name.split('/').length)+'package.json',url);
  const pkg=JSON.parse(await readFile(packageUrl,'utf8'));
  if(pkg.version!=='6.2.4')throw new Error(`Unvalidated Vite Svelte plugin version: ${pkg.version}`);
  const source=await readFile(new URL(url),'utf8');const [before,after]=entries.get(name);
  if(source.split(before).length!==2)throw new Error(`Unsupported compiler call layout: ${url}`);
  return {format:'module',source:source.replace(before,after),shortCircuit:true};
}
