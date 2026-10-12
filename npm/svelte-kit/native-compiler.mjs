/** Explicit hybrid build bridge: only Svelte compilation runs in Rust. */
import { spawn } from 'node:child_process';
import { appendFileSync, mkdirSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
const tool = process.env.TS_SVELTE_BINARY;
if (!tool) throw new Error('TS_SVELTE_BINARY must name the native compiler');
let child, buffer = '', failed;
const pending = [];
function send(request) {
  if (failed) return Promise.reject(failed);
  if (!child) {
    child = spawn(tool, ['--serve'], { stdio: ['pipe', 'pipe', 'inherit'] });
    child.unref(); child.stdin.unref(); child.stdout.unref(); child.stdout.setEncoding('utf8');
    const fail = error => { failed = error; for (const job of pending.splice(0)) { clearTimeout(job.timer); job.reject(error); } child.kill(); };
    child.on('error', fail); child.stdin.on('error', fail);
    child.on('exit', code => fail(new Error(`Native compiler exited (${code})`)));
    child.stdout.on('data', data => {
      buffer += data;
      if (buffer.length > 64 * 1024 * 1024) { fail(new Error('Native response exceeds 64 MiB')); return; }
      let end;
      while ((end = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, end); buffer = buffer.slice(end + 1);
        const job = pending.shift();
        if (!job) { fail(new Error('Unexpected native response')); return; }
        clearTimeout(job.timer);
        try { job.resolve(JSON.parse(line)); } catch(error) { job.reject(error); fail(error); }
      }
    });
    process.on('exit', () => child.kill());
  }
  return new Promise((resolve,reject) => {
    const timer = setTimeout(() => { failed = new Error('Native compiler request timed out'); child.kill(); }, 30000);
    pending.push({ resolve, reject, timer });
    child.stdin.write(JSON.stringify(request) + '\n');
  });
}
async function invoke(op, code, options) {
  const {filename,generate='client',runes,dev=false,warningFilter,hmr,customElement,cssHash, ...rest}=options;
  if (hmr || customElement || cssHash) throw new Error('Native build bridge does not support HMR, custom elements or custom CSS hash callbacks');
  if (!['client','server'].includes(generate)) throw new Error(`Unsupported native generation target: ${generate}`);
  const request={op,document:{uri:filename,version:1,text:code},target:generate,dev,options:rest,compatibility:{svelteVersion:process.env.TS_SVELTE_VERSION||'5.53.11'}};
  if(op==='compile')request.runes=runes;
  if(process.env.TS_SVELTE_BUILD_TRACE)appendFileSync(process.env.TS_SVELTE_BUILD_TRACE,JSON.stringify({op,filename,target:generate,event:"start"})+"\n");
  if(process.env.TS_SVELTE_REQUEST_DIR){mkdirSync(process.env.TS_SVELTE_REQUEST_DIR,{recursive:true});writeFileSync(`${process.env.TS_SVELTE_REQUEST_DIR}/${createHash("sha256").update(JSON.stringify(request)).digest("hex")}.json`,JSON.stringify(request));}
  const response=await send(request);
  if(process.env.TS_SVELTE_BUILD_TRACE)appendFileSync(process.env.TS_SVELTE_BUILD_TRACE,JSON.stringify({op,filename,target:generate,ok:response.ok,error:response.error})+'\n');
  const convert=d=>({code:d.code,message:d.message,filename,start:d.range?{line:d.range.start.line+1,column:d.range.start.character,character:d.bytes?Buffer.from(code).subarray(0,d.bytes.start).toString("utf8").length:undefined}:undefined,end:d.range?{line:d.range.end.line+1,column:d.range.end.character,character:d.bytes?Buffer.from(code).subarray(0,d.bytes.end).toString("utf8").length:undefined}:undefined});
  if(!response.ok){const d=response.diagnostics?.find(d=>d.severity==='error');throw Object.assign(new Error(d?.message||response.error?.message),convert(d||{code:response.error?.code,message:response.error?.message}));}
  const r=response.result;
  const mapped=artifact=>artifact?{code:artifact.code,map:typeof artifact.map==='string'?JSON.parse(artifact.map):artifact.map}:null;
  const warnings=r.diagnostics.map(convert).filter(w=>!warningFilter||warningFilter(w));
  return {js:mapped(r.javascript),css:mapped(r.css),warnings,metadata:{runes:r.runes},ast:null};
}
export function compile(code,options){return invoke('compile',code,options);}
export function compileModule(code,options){return invoke('compileModule',code,options);}
