import { readFile } from 'node:fs/promises';
import assert from 'node:assert/strict';
import test from 'node:test';
import init, { js_runtime_json } from '../generated/ts_js_runtime.js';

await init({ module_or_path: await readFile(new URL('../generated/ts_js_runtime_bg.wasm', import.meta.url)) });
const call = (request) => JSON.parse(js_runtime_json(JSON.stringify(request)));

test('WASM pipes native streams through transforms and releases locks on abort', () => {
  const source = `export async function run(){
    const original=new ReadableStream({start(c){c.enqueue('one');c.enqueue('two');c.close();}});
    const transform=new TransformStream({transform(value,c){c.enqueue(value.toUpperCase());}});
    const output=[];
    const sink=new WritableStream({write(value){output.push(value);},close(){output.push('closed');}});
    await original.pipeThrough(transform).pipeTo(sink);
    const abort=new AbortController(), reason={stop:true}, seen=[];
    const pending=new ReadableStream({cancel(value){seen.push(value===reason);}});
    const destination=new WritableStream({abort(value){seen.push(value===reason);}});
    const piping=pending.pipeTo(destination,{signal:abort.signal});
    abort.abort(reason);let same=false;try{await piping}catch(error){same=error===reason}
    return [output,original.locked,sink.locked,seen,same,pending.locked,destination.locked];
  }`;
  const created=call({op:'create',capabilities:['streams'],snapshot:{modules:{entry:{source,importMetaUrl:'file:///pipe.js'}},resolutions:{}}});
  assert.equal(created.ok,true,JSON.stringify(created));
  const session=created.result.session;
  try {
    const result=call({op:'invokeExport',session,module:'entry',name:'run',arguments:[]});
    assert.equal(result.ok,true,JSON.stringify(result));
    assert.deepEqual(result.result.value,{kind:'json',value:[['ONE','TWO','closed'],false,false,[true,true],true,false,false]});
  } finally { call({op:'dispose',session}); }
});

test('WASM transform streams and native hashing use host entropy explicitly', () => {
  const source=`export async function run(){
    const uuid=crypto.randomUUID();
    const bytes=crypto.getRandomValues(new Uint8Array(4));
    const hash=Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',new TextEncoder().encode('abc'))),x=>x.toString(16).padStart(2,'0')).join('');
    const transform=new TransformStream({transform(chunk,controller){controller.enqueue(chunk.toUpperCase());}});
    const writer=transform.writable.getWriter();
    const writing=(async()=>{await writer.write('hello');await writer.close();})();
    const reader=transform.readable.getReader();
    const first=await reader.read(),last=await reader.read();await writing;
    return [uuid,Array.from(bytes),hash,first.value,last.done];
  }`;
  const created=call({op:'create',capabilities:['crypto','streams'],cryptoOptions:{entropy:Array.from({length:20},(_,i)=>i)},snapshot:{modules:{entry:{source,importMetaUrl:'file:///crypto.js'}},resolutions:{}}});
  assert.equal(created.ok,true,JSON.stringify(created));
  const session=created.result.session;
  const response=call({op:'invokeExport',session,module:'entry',name:'run',arguments:[]});
  assert.equal(response.ok,true,JSON.stringify(response));
  assert.deepEqual(response.result.value,{kind:'json',value:['00010203-0405-4607-8809-0a0b0c0d0e0f',[16,17,18,19],'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad', 'HELLO',true]});
  call({op:'dispose',session});
});

test('WASM builtin async context isolates concurrent await continuations', () => {
  const source=`import {AsyncLocalStorage} from 'node:async_hooks';
    const store=new AsyncLocalStorage();
    export async function run(){
      const alias=await import('async_hooks');
      const values=await Promise.all(['left','right'].map(value=>store.run(value,async()=>{await 0;await 0;return store.getStore();})));
      return [values,store.getStore()===undefined,alias.AsyncLocalStorage===AsyncLocalStorage,typeof globalThis.AsyncLocalStorage];
    }`;
  const created=call({op:'create',capabilities:['nodeAsyncHooks'],snapshot:{modules:{entry:{source,importMetaUrl:'file:///context.js'}},resolutions:{entry:{'node:async_hooks':{kind:'builtin',name:'node:async_hooks'},async_hooks:{kind:'builtin',name:'async_hooks'}}}}});
  assert.equal(created.ok,true,JSON.stringify(created));
  const session=created.result.session;
  for(let i=0;i<2;i++){
    const response=call({op:'invokeExport',session,module:'entry',name:'run',arguments:[]});
    assert.equal(response.ok,true,JSON.stringify(response));
    assert.deepEqual(response.result.value,{kind:'json',value:[['left','right'],true,true,'undefined']});
  }
  call({op:'dispose',session});
});
test('WASM evaluates real ESM, awaits promises and retains module state', () => {
  const snapshot = {
    modules: {
      entry: {source: "import { next } from './counter.js'; export async function run(){const m=await import('./counter.js'); return [next(),m.next(),import.meta.url];}", importMetaUrl: 'file:///entry.js'},
      counter: {source: 'let count=0; export function next(){return ++count;}', importMetaUrl: 'file:///counter.js'},
    },
    resolutions: {entry: {'./counter.js': {kind:'module', id:'counter'}}},
  };
  const created = call({op:'create', snapshot});
  assert.equal(created.ok, true, JSON.stringify(created));
  const session = created.result.session;
  for (const expected of [[1,2,'file:///entry.js'],[3,4,'file:///entry.js']]) {
    const response = call({op:'invokeExport',session,module:'entry',name:'run',arguments:[]});
    assert.equal(response.ok,true,JSON.stringify(response));
    assert.deepEqual(response.result.value,{kind:'json',value:expected});
  }
  assert.equal(call({op:'dispose',session}).result.disposed,true);
  assert.equal(call({op:'evaluate',session,module:'entry'}).error.code,'unknown_session');
});
test('WASM does not substitute absent exports or grant filesystem access', () => {
  const snapshot = {modules:{entry:{source:'export const empty=undefined; export const nil=null; export function host(){return typeof process;}',importMetaUrl:'file:///empty.js'}},resolutions:{}};
  const session = call({op:'create',snapshot}).result.session;
  assert.deepEqual(call({op:'readExport',session,module:'entry',name:'missing'}).result.value,{kind:'absent'});
  assert.deepEqual(call({op:'readExport',session,module:'entry',name:'empty'}).result.value,{kind:'undefined'});
  assert.deepEqual(call({op:'readExport',session,module:'entry',name:'nil'}).result.value,{kind:'json',value:null});
  assert.deepEqual(call({op:'invokeExport',session,module:'entry',name:'host',arguments:[]}).result.value,{kind:'json',value:'undefined'});
  call({op:'dispose',session});
});

test('WASM native web capabilities compose without JavaScript polyfills', () => {
  const source = `export async function run(){
    const url=new URL('../c?q=one','https://example.test/a/b');
    url.searchParams.append('q','two');
    const bytes=new TextEncoder().encode('A😀');
    const headers=new Headers([['X-Test',' first '],['x-test','second']]);
    const form=new FormData();
    form.append('upload',new Blob([bytes],{type:'text/plain'}),'a.txt');
    const controller=new AbortController();
    controller.signal.addEventListener('abort',()=>{throw new Error('listener failed');});
    controller.abort('finished');
    const stream=new ReadableStream({type:'bytes',start(c){c.enqueue(bytes);c.close();}});
    const reader=stream.getReader({mode:'byob'});
    const first=await reader.read(new Uint8Array(8));
    const last=await reader.read(new Uint8Array(8));
    return [url.href,headers.get('x-test'),form.get('upload').name,
      await form.get('upload').text(),controller.signal.reason,
      new TextDecoder().decode(first.value),last.done,bytes.byteLength];
  }`;
  const created=call({op:'create',capabilities:['url','text','headers','formData','abort','streams'],snapshot:{modules:{entry:{source,importMetaUrl:'file:///web.js'}},resolutions:{}}});
  assert.equal(created.ok,true,JSON.stringify(created));
  const session=created.result.session;
  const response=call({op:'invokeExport',session,module:'entry',name:'run',arguments:[]});
  assert.equal(response.ok,true,JSON.stringify(response));
  assert.deepEqual(response.result.value,{kind:'json',value:['https://example.test/c?q=one&q=two','first, second','a.txt','A😀','finished','A😀',true,0]});
  assert.equal(response.result.reportedErrors.length,1);
  assert.equal(response.result.reportedErrors[0].code,'event_listener_exception');
  call({op:'dispose',session});
});

test('WASM Request and Response preserve native body, cookie and clone semantics', () => {
  const source=`export async function run(){
    const form=new FormData(); form.append('name','Ada'); form.append('upload',new Blob(['hello']),'a.txt');
    const request=new Request('/upload',{method:'POST',body:form});
    const copy=request.clone();
    const parsed=await copy.formData();
    const response=Response.json({name:parsed.get('name')},{status:201,headers:[['set-cookie','a=1'],['set-cookie','b=2']]});
    const twin=response.clone();
    const json=await response.json();
    const specials=await new Response('[-0,1e999,"\\\\ud800"]').json();
    return [request.url,request.bodyUsed,copy.bodyUsed,parsed.get('upload').name,
      await parsed.get('upload').text(),response.status,response.headers.getSetCookie(),
      json.name,await twin.text(),Object.is(specials[0],-0),specials[1]===Infinity,
      specials[2].charCodeAt(0),typeof fetch];
  }`;
  const created=call({op:'create',capabilities:['requestResponse'],fetchOptions:{baseUrl:'https://example.test/base/',multipartEntropy:[Array(16).fill(1)]},snapshot:{modules:{entry:{source,importMetaUrl:'file:///fetch-classes.js'}},resolutions:{}}});
  assert.equal(created.ok,true,JSON.stringify(created));
  const session=created.result.session;
  const response=call({op:'invokeExport',session,module:'entry',name:'run',arguments:[]});
  assert.equal(response.ok,true,JSON.stringify(response));
  assert.deepEqual(response.result.value,{kind:'json',value:['https://example.test/upload',false,true,'a.txt','hello',201,['a=1','b=2'],'Ada','{"name":"Ada"}',true,true,0xd800,'undefined']});
  call({op:'dispose',session});
});
