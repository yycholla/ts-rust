/** Usage: node scripts/svelte/session-soak.mjs NEW_OUTPUT_DIRECTORY */
import {spawn} from 'node:child_process';
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import assert from 'node:assert/strict';
if(process.argv.includes('help')||process.argv.includes('--help')){console.log('Usage: node scripts/svelte/session-soak.mjs NEW_OUTPUT_DIRECTORY');process.exit(0);}
if(process.argv.length!==3)throw new Error('Expected output directory');
const out=resolve(process.argv[2]);await mkdir(out);
const child=spawn(resolve('target/release/svelte-service'));let buffer='',receive;
child.stdout.setEncoding('utf8');child.stdout.on('data',text=>{buffer+=text;let index;while((index=buffer.indexOf('\n'))>=0){const line=buffer.slice(0,index);buffer=buffer.slice(index+1);receive?.(JSON.parse(line));receive=null;}});
const exited=new Promise(r=>child.on('exit',r));
function request(value){return new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(new Error('Service timeout')),10000);receive=reply=>{clearTimeout(timer);resolve(reply);};child.stdin.write(JSON.stringify(value)+'\n');});}
const rss=async()=>Number((await readFile(`/proc/${child.pid}/status`,'utf8')).match(/^VmRSS:\s+(\d+)/m)[1])*1024;
const samples=[];const started=performance.now();let failure;
try{
 assert.equal((await request({op:'sessionOpen',document:{uri:'Soak.svelte',version:1,text:'<p>start</p>'}})).ok,true);
 for(let version=2;version<=1201;version++){
  const response=await request({op:'sessionChange',uri:'Soak.svelte',version,edits:[{range:null,text:`<script>let count=$state(${version});</script><button onclick={()=>count++}>{count}</button>`}]});
  assert.equal(response.ok,true);assert.deepEqual(response.result.diagnostics,[]);
  if(version%100===1)samples.push({edits:version-1,rssBytes:await rss(),stats:(await request({op:'sessionStats'})).result});
 }
 assert(samples.at(-1).rssBytes-samples[0].rssBytes<32*1024*1024,'Sustained editor RSS grew more than 32 MiB after warmup');
 assert(samples.every(s=>s.stats.documents===1&&s.stats.cachedDiagnostics===1&&s.stats.sourceBytes<1024));
 await request({op:'sessionClose',uri:'Soak.svelte'});const stats=(await request({op:'sessionStats'})).result;assert.equal(stats.documents,0);assert.equal(stats.cachedDiagnostics,0);assert.equal(stats.sourceBytes,0);
}catch(error){failure=String(error.stack);}finally{child.stdin.end();if(failure)child.kill('SIGKILL');await exited;}
const report={passed:!failure,failure,edits:1200,elapsedMs:performance.now()-started,samples,scope:'One persistent native session, 1200 full-buffer edits, post-warmup RSS cap; not a comparative performance benchmark'};await writeFile(out+'/report.json',JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify({...report,samples:undefined},null,2));process.exitCode=failure?1:0;
