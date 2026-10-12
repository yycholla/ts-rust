/** Usage: node scripts/svelte/lsp-failure-probe.mjs NEW_OUTPUT_DIRECTORY */
import {spawn} from 'node:child_process';
import {mkdir,writeFile,copyFile,readFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import assert from 'node:assert/strict';
if(process.argv.includes('help')||process.argv.includes('--help')){console.log('Usage: node scripts/svelte/lsp-failure-probe.mjs NEW_OUTPUT_DIRECTORY');process.exit(0);}
if(process.argv.length!==3)throw new Error('Expected output directory');
const out=resolve(process.argv[2]);await mkdir(out);await copyFile('target/release/svelte-lsp',out+'/svelte-lsp');
const root=out+'/project';await mkdir(root);await writeFile(root+'/A.svelte','<p>test</p>');
const uri=pathToFileURL(root+'/A.svelte').href;
const tests=[];
for(const [name,body,expected] of [
 ['missing diagnostics',`print('{"ok":true}')`,'missing diagnostics'],
 ['invalid exit',`print('{"ok":true,"checker":{"diagnostics":[],"status":0}}');sys.exit(42)`,'exit status'],
 ['timeout',`import time\nopen(${JSON.stringify(out+'/child-pid')},'w').write(str(os.getpid()))\ntime.sleep(90)`,'30 seconds'],
]){
 await writeFile(out+'/svelte-project',`#!/usr/bin/env python3\nimport sys,os\n${body}\n`,{mode:0o700});
 const proc=spawn(out+'/svelte-lsp');let wire=Buffer.alloc(0),id=0;const pending=new Map(),history=[];
 const exited=new Promise(r=>proc.on('exit',r));
 proc.stdout.on('data',data=>{wire=Buffer.concat([wire,data]);while(true){const split=wire.indexOf('\r\n\r\n');if(split<0)break;const size=Number(wire.subarray(0,split).toString().match(/Content-Length: (\d+)/)[1]);if(wire.length<split+4+size)break;const message=JSON.parse(wire.subarray(split+4,split+4+size));wire=wire.subarray(split+4+size);history.push(message);pending.get(message.id)?.(message);pending.delete(message.id);}});
 function send(message){const text=JSON.stringify({jsonrpc:'2.0',...message});proc.stdin.write(`Content-Length: ${Buffer.byteLength(text)}\r\n\r\n${text}`);}
 function request(method,params){return new Promise((resolve,reject)=>{const key=++id;const timer=setTimeout(()=>reject(new Error('LSP did not respond within 40 seconds')),40000);pending.set(key,m=>{clearTimeout(timer);resolve(m);});send({id:key,method,params});});}
 const start=Date.now();
 try{
  const init=await request('initialize',{initializationOptions:{project:{root,dependencies:resolve('target/svelte-versions/kit-2.69.2/node_modules'),components:[{path:'A.svelte',typescript:false}]}}});assert(init.result,JSON.stringify(init));
  send({method:'textDocument/didOpen',params:{textDocument:{uri,version:1,text:'<p>test</p>'}}});
  await request('svelte/sessionStats');
  const diagnostics=history.filter(m=>m.method==='textDocument/publishDiagnostics').at(-1).params.diagnostics;
  assert(diagnostics.some(d=>d.code==='incomplete_project_check'&&d.message.includes(expected)),JSON.stringify(diagnostics));
  if(name==='timeout'){const pid=Number(await readFile(out+'/child-pid','utf8'));assert.throws(()=>process.kill(pid,0),{code:'ESRCH'});}
  await request('shutdown');send({method:'exit'});proc.stdin.end();await exited;
  tests.push({name,status:'pass',elapsedMs:Date.now()-start,history});
 }catch(error){proc.kill('SIGKILL');await exited;tests.push({name,status:'fail',error:String(error.stack),history});}
}
await writeFile(out+'/report.json',JSON.stringify({tests},null,2)+'\n');console.log(tests.map(({name,status,elapsedMs,error})=>({name,status,elapsedMs,error})));process.exitCode=tests.some(t=>t.status!=='pass')?1:0;
