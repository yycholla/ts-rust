/** Usage: node scripts/svelte/application-probe.mjs OUTPUT. Refuses overwrite. */
import {spawn} from 'node:child_process';
import {mkdir,writeFile} from 'node:fs/promises';
import assert from 'node:assert/strict';
if(process.argv.includes('--help')||process.argv.includes('help')||process.argv.length!==3){console.log('Usage: node scripts/svelte/application-probe.mjs NEW_OUTPUT');process.exit(process.argv.length===3?0:2);}
import {resolve} from 'node:path';
const out=resolve(process.argv[2]);await mkdir(out,{recursive:false});
const tests=[];
async function test(name,fn){try{await fn();tests.push({name,status:'pass'});}catch(error){tests.push({name,status:'fail',error:String(error.stack)});}}
const child=spawn(resolve('target/release/svelte-service'),[]);let buffer='';const pending=[];
child.stdout.on('data',data=>{buffer+=data;let end;while((end=buffer.indexOf('\n'))>=0){const line=buffer.slice(0,end);buffer=buffer.slice(end+1);pending.shift()?.(JSON.parse(line));}});
const request=input=>new Promise(resolve=>{pending.push(resolve);child.stdin.write(JSON.stringify(input)+'\n');});
const uri='file:///test.svelte';
await test('session open lint and clear after edit',async()=>{
 let r=await request({op:'sessionOpen',document:{uri,version:1,text:'<script>debugger;</script><p>Hello</p>'}});assert.equal(r.ok,true);assert(r.result.diagnostics.some(d=>d.code==='no-debugger'));
 r=await request({op:'sessionChange',uri,version:2,edits:[{range:null,text:'<p>Hello</p>'}]});assert.equal(r.ok,true);assert.deepEqual(r.result.diagnostics,[]);
});
await test('stale version cannot replace newer diagnostics',async()=>{const r=await request({op:'sessionChange',uri,version:1,edits:[{range:null,text:'{#if}'}]});assert.equal(r.ok,false);assert.equal((await request({op:'sessionDiagnostics',uri})).result.version,2);});
await test('atomic invalid second edit and unicode CRLF',async()=>{
 let r=await request({op:'sessionChange',uri,version:3,edits:[{range:null,text:'<p>😀</p>\r\n<p>Hello</p>'}]});assert.equal(r.ok,true);
 r=await request({op:'sessionChange',uri,version:4,edits:[{range:null,text:'<p>Other</p>'},{range:{start:{line:99,character:0},end:{line:99,character:1}},text:'bad'}]});assert.equal(r.ok,false);assert.equal((await request({op:'sessionDiagnostics',uri})).result.version,3);
 r=await request({op:'sessionChange',uri,version:4,edits:[{range:{start:{line:0,character:4},end:{line:0,character:5}},text:'x'}]});assert.equal(r.ok,false);
 r=await request({op:'sessionChange',uri,version:4,edits:[{range:{start:{line:1,character:3},end:{line:1,character:8}},text:'world'}]});assert.equal(r.ok,true);
});
await test('syntax invalid then fixed',async()=>{let r=await request({op:'sessionChange',uri,version:5,edits:[{range:null,text:'{#if}'}]});assert(r.result.diagnostics.some(d=>d.severity==='error'));r=await request({op:'sessionChange',uri,version:6,edits:[{range:null,text:'<p>Fine</p>'}]});assert.deepEqual(r.result.diagnostics,[]);});
await test('profile invalidation changes grammar diagnostics',async()=>{await request({op:'sessionChange',uri,version:7,edits:[{range:null,text:'{@const x = 1}<p>{x}</p>'}]});const r=await request({op:'sessionConfigure',compatibility:{svelteVersion:'9.0.0'}});assert.equal(r.ok,false);assert.equal((await request({op:'sessionDiagnostics',uri})).result.compatibility.svelteVersion,'5.53.11');});
await test('native syntax rules ignore strings and comments',async()=>{let r=await request({op:'lint',document:{uri:'file:///x.ts',version:1,text:'// debugger; a == b\nconst text="debugger; =="; if(1 === 1) {}'}});assert.deepEqual(r.result.diagnostics,[]);r=await request({op:'lint',document:{uri:'file:///x.ts',version:1,text:'debugger; if(1 == 2) {}'}});assert.deepEqual(r.result.diagnostics.map(d=>d.code),['no-debugger','eqeqeq']);});
await test('repeated edits keep bounded session state and close clears',async()=>{for(let version=8;version<108;version++){const r=await request({op:'sessionChange',uri,version,edits:[{range:null,text:`<p>${version}</p>`}]});assert.equal(r.ok,true);}let r=await request({op:'sessionStats'});assert.equal(r.result.documents,1);assert.equal(r.result.cachedDiagnostics,1);assert(r.result.sourceBytes<32);r=await request({op:'sessionClose',uri});assert.deepEqual(r.result.diagnostics,[]);assert.equal((await request({op:'sessionStats'})).result.sourceBytes,0);});
child.stdin.end();await new Promise(resolve=>child.on('exit',resolve));
const lsp=spawn(resolve('target/release/svelte-lsp'));let wire=Buffer.alloc(0);const messages=[];const waiters=[];let nextId=0;
lsp.stdout.on('data',data=>{wire=Buffer.concat([wire,data]);while(true){const split=wire.indexOf('\r\n\r\n');if(split<0)break;const size=Number(wire.subarray(0,split).toString().match(/Content-Length: (\d+)/i)[1]);if(wire.length<split+4+size)break;const message=JSON.parse(wire.subarray(split+4,split+4+size));wire=wire.subarray(split+4+size);messages.push(message);for(let i=waiters.length-1;i>=0;i--)if(waiters[i].predicate(message)){waiters.splice(i,1)[0].resolve(message);}}});
function send(message){const body=JSON.stringify({jsonrpc:'2.0',...message});lsp.stdin.write(`Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`);}
function wait(predicate){return new Promise((resolve,reject)=>{const timeout=setTimeout(()=>reject(new Error('LSP response timed out')),10000);waiters.push({predicate,resolve:m=>{clearTimeout(timeout);resolve(m);}});});}
async function call(method,params){const id=++nextId;const result=wait(m=>m.id===id);send({id,method,params});return result;}
await test('LSP initialize rejects preinitialize and advertises real scope',async()=>{assert.equal((await call('textDocument/diagnostic',{})).error.code,-32002);const r=await call('initialize',{});assert.equal(r.result.capabilities.positionEncoding,'utf-16');assert.equal(r.result.capabilities.textDocumentSync.change,2);send({method:'initialized',params:{}});});
await test('LSP open edit pull close clear and shutdown',async()=>{
 let result=wait(m=>m.method==='textDocument/publishDiagnostics');send({method:'textDocument/didOpen',params:{textDocument:{uri,version:1,languageId:'svelte',text:'<script>debugger;</script>'}}});let r=await result;assert(r.params.diagnostics.some(d=>d.code==='no-debugger'));assert.equal(r.params.version,1);
 result=wait(m=>m.method==='textDocument/publishDiagnostics');send({method:'textDocument/didChange',params:{textDocument:{uri,version:2},contentChanges:[{text:'<p>Okay</p>'}]}});r=await result;assert.deepEqual(r.params.diagnostics,[]);assert.equal(r.params.version,2);
 r=await call('textDocument/diagnostic',{textDocument:{uri}});assert.deepEqual(r.result.items,[]);
 result=wait(m=>m.method==='textDocument/publishDiagnostics');send({method:'textDocument/didClose',params:{textDocument:{uri}}});assert.deepEqual((await result).params.diagnostics,[]);
 assert.equal((await call('shutdown')).result,null);
});
send({method:'exit'});lsp.stdin.end();await new Promise(resolve=>lsp.on('exit',resolve));
await writeFile(resolve(out,'report.json'),JSON.stringify({tests,passed:tests.filter(t=>t.status==='pass').length,failed:tests.filter(t=>t.status==='fail').length,messages},null,2)+'\n');console.log(tests.map(t=>`${t.status}: ${t.name}`).join('\n'));process.exitCode=tests.some(t=>t.status==='fail')?1:0;
