/** Usage: node scripts/svelte/kit-lsp-probe.mjs ISOLATED_APP NEW_OUTPUT */
import {spawn} from 'node:child_process';
import {mkdir, readFile, writeFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import assert from 'node:assert/strict';
if(process.argv.length!==4||process.argv.includes('--help')){console.log('Usage: node scripts/svelte/kit-lsp-probe.mjs ISOLATED_APP NEW_OUTPUT');process.exit(process.argv.includes('--help')?0:2);}
const app=resolve(process.argv[2]),out=resolve(process.argv[3]);await mkdir(out);
const relative='src/routes/admin/settings/integrations/payroll-providers/+page.svelte';
const source=await readFile(app+'/'+relative,'utf8'),uri=pathToFileURL(app+'/'+relative).href;
const proc=spawn(resolve('target/release/svelte-kit-project'),['serve',app,'runes']);
let wire=Buffer.alloc(0),next=0;const history=[],pending=new Map(),tests=[];let stderr='';
const exited=new Promise(resolve=>proc.on('exit',(code,signal)=>resolve({code,signal})));
proc.stderr.on('data',data=>stderr+=data);
proc.stdout.on('data',data=>{wire=Buffer.concat([wire,data]);while(true){const split=wire.indexOf('\r\n\r\n');if(split<0)break;const size=Number(wire.subarray(0,split).toString().match(/Content-Length: (\d+)/)?.[1]);if(!Number.isFinite(size))throw Error('Invalid frame');if(wire.length<split+4+size)break;const m=JSON.parse(wire.subarray(split+4,split+4+size));wire=wire.subarray(split+4+size);history.push(m);pending.get(m.id)?.(m);pending.delete(m.id);}});
function send(m){const text=JSON.stringify({jsonrpc:'2.0',...m});proc.stdin.write(`Content-Length: ${Buffer.byteLength(text)}\r\n\r\n${text}`);}
function request(method,params){return new Promise((resolve,reject)=>{const id=++next;const timer=setTimeout(()=>reject(Error('Timed out: '+method)),90000);pending.set(id,m=>{clearTimeout(timer);resolve(m);});send({id,method,params});});}
const latest=()=>history.filter(m=>m.method==='textDocument/publishDiagnostics'&&m.params.uri===uri).at(-1)?.params.diagnostics;
async function test(name,fn){const start=performance.now();try{await fn();tests.push({name,status:'pass',milliseconds:performance.now()-start});}catch(error){tests.push({name,status:'fail',error:String(error.stack)});}}
try{
 await test('Kit plugin discovers and initializes persistent semantic LSP',async()=>{const r=await request('initialize',{rootUri:pathToFileURL(app).href});assert.equal(r.result?.capabilities.diagnosticProvider.interFileDependencies,true,JSON.stringify(r));});
 await test('full MoncuraHR graph checks on open without synthetic props error',async()=>{send({method:'textDocument/didOpen',params:{textDocument:{uri,version:1,languageId:'svelte',text:source}}});await request('svelte/sessionStats');assert(latest());assert(!latest().some(d=>d.severity===1),JSON.stringify(latest()));});
 await test('unsaved source error appears in same LSP process',async()=>{send({method:'textDocument/didChange',params:{textDocument:{uri,version:2},contentChanges:[{text:source+'\n<p>{__svelte_project_missing_name__}</p>\n'}]}});await request('svelte/sessionStats');assert(latest().some(d=>d.code===2304),JSON.stringify(latest()));});
 await test('repair removes semantic error and pull agrees',async()=>{send({method:'textDocument/didChange',params:{textDocument:{uri,version:3},contentChanges:[{text:source}]}});await request('svelte/sessionStats');assert(!latest().some(d=>d.severity===1),JSON.stringify(latest()));const r=await request('textDocument/diagnostic',{textDocument:{uri}});assert(!r.result.items.some(d=>d.severity===1));});
 await test('route load edit explicitly invalidates generated type snapshot',async()=>{const path='src/routes/admin/settings/integrations/payroll-providers/+page.server.ts',text=await readFile(app+'/'+path,'utf8'),loadUri=pathToFileURL(app+'/'+path).href;send({method:'textDocument/didOpen',params:{textDocument:{uri:loadUri,version:1,languageId:'typescript',text:text+'\n// unsaved route edit\n'}}});await request('svelte/sessionStats');assert(latest().some(d=>d.code==='incomplete_project_check'&&/snapshot|changed|sync/i.test(d.message)),JSON.stringify(latest()));send({method:'textDocument/didClose',params:{textDocument:{uri:loadUri}}});await request('svelte/sessionStats');assert(!latest().some(d=>d.code==='incomplete_project_check'));});
 await request('shutdown');send({method:'exit'});proc.stdin.end();
}finally{
 const shutdownTimer=setTimeout(()=>proc.kill('SIGKILL'),5000);await exited;clearTimeout(shutdownTimer);
 await writeFile(out+'/report.json',JSON.stringify({tests,history,exit:await exited},null,2)+'\n');await writeFile(out+'/stderr.log',stderr);
}
console.log(tests);process.exitCode=tests.some(t=>t.status!=='pass')?1:0;
