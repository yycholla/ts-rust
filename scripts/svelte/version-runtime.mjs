#!/usr/bin/env node
/** Usage: node scripts/svelte/version-runtime.mjs NEW_OUTPUT
 * Cross-compiler SSR/hydration checks on every exact Svelte runtime profile.
 * Includes counter state updates, DOM reuse and cleanup; every profile also exercises select mount/reset behavior; newest profile
 * exercises development instrumentation. JSDOM only.
 */
import {readFile,writeFile,mkdir,symlink} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import path from 'node:path';
if(process.argv.includes('help')||process.argv.includes('--help')){console.log('Usage: node scripts/svelte/version-runtime.mjs NEW_OUTPUT');process.exit(0);}
if(process.argv.length!==3)throw new Error('Expected new output directory');
const out=path.resolve(process.argv[2]);await mkdir(out);
const versions=['5.51.5','5.52.0','5.53.5','5.53.11','5.53.13','5.54.1','5.55.10','5.56.10','5.57.2'];
const binary=path.resolve('target/release/svelte-tool');
const hash=x=>createHash('sha256').update(x).digest('hex');
const json=x=>JSON.stringify(x,null,2)+'\n';
const source=spawnSync('python3',['scripts/svelte/source-identity.py'],{encoding:'utf8',maxBuffer:8*1024*1024});
if(source.status!==0)throw new Error('Source identity failed');
await writeFile(path.join(out,'source.json'),source.stdout);
const report={scope:'Bounded JSDOM runtime behavior, not browser/full application conformance',binarySha256:hash(await readFile(binary)),installations:[],cases:[]};
const save=()=>writeFile(path.join(out,'report.json'),json(report));
const server=`
import {render} from 'svelte/server';
let input='';for await(const chunk of process.stdin)input+=chunk;
const r=JSON.parse(input);const {default:C}=await import(r.module);
const result=render(C,{props:r.props});console.log(JSON.stringify({body:result.body,head:result.head}));
`;
const client=`
import {JSDOM,VirtualConsole} from 'jsdom';
let input='';for await(const chunk of process.stdin)input+=chunk;
const r=JSON.parse(input);const errors=[];const warnings=[];
const vc=new VirtualConsole();vc.on('jsdomError',e=>errors.push(String(e)));vc.on('warn',x=>warnings.push(String(x)));
console.warn=(...xs)=>warnings.push(xs.join(' '));console.error=(...xs)=>errors.push(xs.join(' '));
const dom=new JSDOM('<!doctype html><head>'+r.head+'</head><body><form><main>'+(r.mount?'':r.body)+'</main></form></body>',{url:'http://localhost',pretendToBeVisual:true,virtualConsole:vc});
const w=dom.window;
for(const n of ['window','document','navigator','Node','Element','HTMLElement','HTMLButtonElement','HTMLSelectElement','HTMLOptionElement','HTMLInputElement','Text','Comment','Event','MouseEvent','CustomEvent','Document','DocumentFragment','SVGElement','MutationObserver'])Object.defineProperty(globalThis,n,{value:n==='window'?w:w[n],configurable:true});
globalThis.getComputedStyle=w.getComputedStyle.bind(w);globalThis.requestAnimationFrame=w.requestAnimationFrame.bind(w);globalThis.cancelAnimationFrame=w.cancelAnimationFrame.bind(w);
w.addEventListener('error',e=>errors.push(e.error?.stack||e.message));
const s=await import('svelte');const {default:C}=await import(r.module);const target=w.document.querySelector('main');const selector=r.kind==='select'?'select':'button';const before=target.querySelector(selector);
let instance;const result={ok:false};
try{
 instance=r.mount?s.mount(C,{target,props:r.props}):s.hydrate(C,{target,props:r.props,recover:false});s.flushSync();
 const el=target.querySelector(selector);result.reused=el===before;
 if(!r.mount&&!result.reused)throw new Error('Hydration replaced server element');
 if(r.kind==='select'){
  result.value=el.value;result.defaultSelected=[...el.options].map(o=>o.defaultSelected);
  el.value=el.value==='first'?'second':'first';const chosen=el.value;el.dispatchEvent(new w.Event('change',{bubbles:true}));s.flushSync();
  result.after=el.value;if(result.after!==chosen)throw new Error('Select reset after change');
  target.closest('form').reset();await Promise.resolve();s.flushSync();result.reset=el.value;
 }else if(r.kind==='rest'||r.kind==='whole'){
  const expected=r.kind==='rest'?'Named:Title:false:data-probe,title:':'Named:Title:true:data-probe,named,title:';
  result.initial=el.textContent;result.title=el.title;result.attribute=el.getAttribute('data-probe');
  if(result.initial!==expected+'0'||result.title!=='Title'||result.attribute!=='spread')throw new Error('Rest props read/enumeration/spread failed');
  el.dispatchEvent(new w.MouseEvent('click',{bubbles:true}));s.flushSync();result.after=el.textContent;
  if(result.after!==expected+'1')throw new Error('Rest props update failed');
 }else{
  result.initial=el.textContent;if(result.initial!=='Count: 0 / 0')throw new Error('Wrong initial counter');
  el.dispatchEvent(new w.MouseEvent('click',{bubbles:true}));s.flushSync();result.after=el.textContent;
  if(result.after!=='Count: 2 / 4')throw new Error('Counter/derived update failed');
 }
 await s.unmount(instance);instance=null;s.flushSync();result.cleaned=target.querySelectorAll('*').length===0&&target.textContent==='';
 if(!result.cleaned)throw new Error('Unmount left content');
 await new Promise(resolve=>setImmediate(resolve));result.ok=errors.length===0&&warnings.length===0;
}catch(e){result.error=e.stack||String(e);}finally{if(instance)await s.unmount(instance);dom.window.close();}
result.errors=errors;result.warnings=warnings;console.log(JSON.stringify(result));process.exitCode=result.ok?0:1;
`;
function exec(program,args,input,cwd){
 const p=spawnSync(program,args,{input:JSON.stringify(input),cwd,encoding:'utf8',timeout:30000,maxBuffer:16*1024*1024});
 let response;try{response=JSON.parse(p.stdout);}catch{}
 return {status:p.status,signal:p.signal,error:p.error?.message,stderr:p.stderr,response,raw:response?undefined:p.stdout};
}
for(const version of versions){
 const app=path.resolve(`target/svelte-versions/svelte-${version}`);
 const packageFile=path.join(app,'node_modules/svelte/package.json');
 const installed=JSON.parse(await readFile(packageFile,'utf8'));
 if(installed.version!==version)throw new Error(`Expected ${version}, installed ${installed.version}`);
 const identity={version,path:app,hashes:{}};
 for(const relative of ['package-lock.json','node_modules/svelte/package.json','node_modules/svelte/src/compiler/index.js','node_modules/svelte/src/internal/client/index.js','node_modules/svelte/src/internal/server/index.js','node_modules/jsdom/package.json'])identity.hashes[relative]=hash(await readFile(path.join(app,relative)));
 report.installations.push(identity);
 const compiler=await import(pathToFileURL(path.join(app,'node_modules/svelte/src/compiler/index.js')));
 const variants=[{name:'counter',file:'Counter.svelte',dev:false,kind:'counter'},{name:'select',file:'DefaultSelect.svelte',dev:false,kind:'select'},{name:'rest-props',file:'RestProps.svelte',dev:false,kind:'rest'},{name:'whole-props',file:'WholeProps.svelte',dev:false,kind:'whole'}];
 if(version==='5.57.2')variants.push({name:'counter-dev',file:'Counter.svelte',dev:true,kind:'counter'});
 for(const variant of variants){
  const dir=path.join(out,`${version}-${variant.name}`);await mkdir(dir);await symlink(path.join(app,'node_modules'),path.join(dir,'node_modules'),'dir');
  const text=await readFile(`scripts/svelte/fixtures/runtime/${variant.file}`,'utf8');
  const record={version,...variant,sourceSha256:hash(text),compilation:[],ssr:{},hydration:[],mount:[],ok:false};report.cases.push(record);
  try{
   for(const side of ['oracle','native'])for(const target of ['client','server']){
    let code;
    if(side==='oracle')code=compiler.compile(text,{filename:variant.file,generate:target,runes:true,dev:variant.dev,css:'external'}).js.code;
    else{
     const p=exec(binary,[],{op:'compile',document:{uri:variant.file,version:1,text},target,runes:true,dev:variant.dev,compatibility:{svelteVersion:version}},dir);
     await writeFile(path.join(dir,`native-${target}.json`),json(p));
     if(p.status!==0||p.response?.ok!==true)throw new Error(`Native ${target} failed`);
     code=p.response.result.javascript.code;
    }
    const file=path.join(dir,`${side}-${target}.mjs`);await writeFile(file,code);record.compilation.push({side,target,sha256:hash(code)});
   }
   const props=variant.kind==='counter'?{step:2,label:'Count'}:['rest','whole'].includes(variant.kind)?{named:'Named',title:'Title','data-probe':'spread'}:{};
   const conditions=variant.dev?['--conditions=development']:[];
   for(const side of ['oracle','native'])record.ssr[side]=exec(process.execPath,[...conditions,'--input-type=module','-e',server],{module:pathToFileURL(path.join(dir,`${side}-server.mjs`)).href,props},dir);
   record.ssrMatch=record.ssr.oracle.status===0&&record.ssr.native.status===0&&JSON.stringify(record.ssr.oracle.response)===JSON.stringify(record.ssr.native.response);
   for(const ssrSide of ['oracle','native'])for(const clientSide of ['oracle','native']){
    const rendered=record.ssr[ssrSide];
    const entry={server:ssrSide,client:clientSide};record.hydration.push(entry);
    if(rendered.status!==0){entry.unrun='SSR failed';continue;}
    entry.process=exec(process.execPath,['--conditions=browser',...conditions,'--input-type=module','-e',client],{...rendered.response,module:pathToFileURL(path.join(dir,`${clientSide}-client.mjs`)).href,props,kind:variant.kind},dir);
    entry.ok=entry.process.status===0&&entry.process.response?.ok===true;
   }
   for(const clientSide of ['oracle','native']){
    const entry={client:clientSide};record.mount.push(entry);
    entry.process=exec(process.execPath,['--conditions=browser',...conditions,'--input-type=module','-e',client],{body:'',head:'',mount:true,module:pathToFileURL(path.join(dir,`${clientSide}-client.mjs`)).href,props,kind:variant.kind},dir);
    entry.ok=entry.process.status===0&&entry.process.response?.ok===true;
   }
   const same=(a,b)=>JSON.stringify(a.process.response)===JSON.stringify(b.process.response);
   record.behaviorMatch=record.hydration.length===4&&same(record.hydration[0],record.hydration[1])&&same(record.hydration[0],record.hydration[2])&&same(record.hydration[0],record.hydration[3])&&same(record.mount[0],record.mount[1]);
   record.ok=record.ssrMatch&&record.behaviorMatch&&record.hydration.every(x=>x.ok)&&record.mount.every(x=>x.ok);
  }catch(error){record.failure=error.stack||String(error);}
  await save();console.log(version,variant.name,record.ok?'PASS':'FAIL');
 }
}
report.summary={cases:report.cases.length,passed:report.cases.filter(x=>x.ok).length,hydrationCases:report.cases.reduce((n,x)=>n+x.hydration.length,0),hydrationPassed:report.cases.reduce((n,x)=>n+x.hydration.filter(h=>h.ok).length,0),mountCases:report.cases.reduce((n,x)=>n+x.mount.length,0),mountPassed:report.cases.reduce((n,x)=>n+x.mount.filter(h=>h.ok).length,0)};
const after=spawnSync('python3',['scripts/svelte/source-identity.py'],{encoding:'utf8',maxBuffer:8*1024*1024});
await writeFile(path.join(out,'source-after.json'),after.stdout);
report.sourceStable=after.status===0&&source.stdout===after.stdout;
report.binaryStable=report.binarySha256===hash(await readFile(binary));
report.oraclesStable=true;
for(const installation of report.installations)for(const [relative,digest] of Object.entries(installation.hashes))if(digest!==hash(await readFile(path.join(installation.path,relative))))report.oraclesStable=false;
await save();console.log(json(report.summary));process.exitCode=report.cases.every(x=>x.ok)&&report.sourceStable&&report.binaryStable&&report.oraclesStable?0:1;
