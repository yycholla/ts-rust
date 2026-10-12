import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';
import { initializeRuntime, SvelteRuntime, SvelteRuntimeError } from '../index.js';

// Actual wasm-bindgen module only. Missing or stale builds fail; no mocked dispatch.
const bindingsUrl = process.env.TS_SVELTE_RUNTIME_BINDINGS
  ? pathToFileURL(resolve(process.env.TS_SVELTE_RUNTIME_BINDINGS))
  : new URL('../generated/ts_svelte_runtime.js', import.meta.url);
const wasmUrl = process.env.TS_SVELTE_RUNTIME_WASM
  ? pathToFileURL(resolve(process.env.TS_SVELTE_RUNTIME_WASM))
  : new URL('../generated/ts_svelte_runtime_bg.wasm', import.meta.url);
const bindings = await import(bindingsUrl.href);
const runtime = await initializeRuntime({ bindings, moduleOrPath: await readFile(wasmUrl) });
const bytes = text => Array.from(new TextEncoder().encode(text));

function input(family, scenario = 'normal') {
  const state = `
    export const state={building:false,prerendering:false,configured:0,started:0};
    export const manifest={assets:new Set()};
    export function set_building(){state.building=true}
    export function set_prerendering(){state.prerendering=true}
    export function set_env(env){state.env=env}
    export function set_private_env(env){state.private=env}
    export function set_public_env(env){state.public=env}
    export function set_manifest(m){if(m!==manifest)throw Error('Manifest identity')}
    export function set_read_implementation(read){state.read=read}
  `;
  const server = `
    import {state,manifest} from './state.js';
    const scenario=${JSON.stringify(scenario)};
    let remoteMap,routeIds,previousDependencies,visits=0;
    async function init(){
      if(++state.started!==1)throw Error('Init repeated');
      if(scenario==='init-fail')throw Error('Explicit init failure');
      await 0;
      if(state.env.VALUE!=='provided')throw Error('Environment mismatch');
      if(await new Response(state.read('init.txt')).text()!=='server init')throw Error('Server snapshot read');
    }
    async function respond(request,options){
      if(state.started!==1)throw Error('Respond before init');
      if(!(request instanceof Request)||request.method!=='GET')throw Error('Native GET required');
      if(!request.url.startsWith('https://example.test/base/'))throw Error('Explicit origin/base');
      if(scenario==='respond-fail')throw Error('Explicit respond failure');
      if(scenario==='client-address')options.getClientAddress();
      if(scenario==='missing-asset')options.read('missing.bin');
      const p=options.prerendering;
      if(!(p.dependencies instanceof Map)||!(p.remote_responses instanceof Map))throw Error('Native maps required');
      if(previousDependencies===p.dependencies)throw Error('Dependencies must be per visit');previousDependencies=p.dependencies;
      if(remoteMap&&remoteMap!==p.remote_responses)throw Error('Remote map replaced');remoteMap=p.remote_responses;
      ${family === 'kit3' ? `
        if(!(p.resolved_route_ids instanceof Set))throw Error('Kit3 Set required');
        if(routeIds&&routeIds!==p.resolved_route_ids)throw Error('Route set replaced');routeIds=p.resolved_route_ids;
        if(visits&&!routeIds.has('/shared'))throw Error('Route IDs lost');routeIds.add('/shared');
      ` : "if('resolved_route_ids' in p)throw Error('Kit3 option leaked into Kit2');"}
      if(!remoteMap.has('memo'))remoteMap.set('memo',Promise.resolve(23));
      const memo=await remoteMap.get('memo');
      const staticBytes=options.read('static.bin'),appBytes=options.read('_app/asset.bin');
      if(!(staticBytes instanceof Uint8Array)||!(appBytes instanceof Uint8Array))throw Error('Reads must synchronously return bytes');
      const visit=visits++;
      const body=new ReadableStream({async pull(controller){
        await 0;
        if(scenario==='body-fail')throw Error('Explicit body failure');
        p.dependencies.set('/provided-empty',{response:new Response('unused'),body:''});
        p.dependencies.set('https://unused.invalid/a%2Fb/%F0%9F%98%80?q=1#fragment',{
          response:new Response('unused',{headers:[['x-sveltekit-routeid','/dep%2Fid/%F0%9F%98%80']]}),
          body:new Uint8Array([99,0,255,99]).subarray(1,3)
        });
        p.dependencies.set('/streamed',{response:new Response(new ReadableStream({async pull(c){
          await 0;p.dependencies.set('/late',{response:new Response('unused'),body:'late\\ud800'});
          c.enqueue(new Uint8Array([3,128]));c.close();
        }})),body:null});
        controller.enqueue(new Uint8Array([...staticBytes,...appBytes,visit,memo]));controller.close();
      }});
      return new Response(body,{status:201,statusText:'caf\\u00e9',headers:[
        ['set-cookie','a=1'],['set-cookie','b=2'],['x-sveltekit-routeid','/root%2Fitem/%F0%9F%98%80']
      ]});
    }
    ${family === 'kit3' ? `
      export async function configure(config){
        if(++state.configured!==1||!config.building||!config.prerendering||config.manifest!==manifest)throw Error('Configure contract');
        state.env=config.env;state.read=config.read;await 0;return {init,respond};
      }
    ` : `
      if(!state.building||!state.prerendering||!state.env)throw Error('Kit2 module initialization order');
      export class Server{
        constructor(m){if(m!==manifest||++state.configured!==1)throw Error('Server construction')}
        async init(options){state.env=options.env;state.read=options.read;await init()}
        respond(request,options){return respond(request,options)}
      }
    `}
  `;
  const module = (source, name) => ({ source, importMetaUrl: `file:///prerender/${name}.js` });
  return {
    bootstrap: {
      serverBuildId: `prerender-${family}-${scenario}`,
      snapshot: {
        modules: { state: module(state, 'state'), server: module(server, 'server') },
        resolutions: { server: { './state.js': { kind: 'module', id: 'state' } } },
      },
      manifestEntry: 'state',
      initialization: family === 'kit3' ? { profile: family, server: 'server' }
        : { profile: family, internal: 'state', environment: 'state', publicPrefix: 'PUBLIC_', privatePrefix: '' },
      environment: { VALUE: 'provided', PUBLIC_VALUE: 'public' },
      assets: { 'init.txt': bytes('server init'), '_app/asset.bin': [0, 255] },
    },
    serverEntry: 'server', origin: 'https://example.test', base: '/base', appDir: '_app',
    staticAssets: { 'static.bin': [1, 2], '_app/asset.bin': [99] },
  };
}

for (const family of ['kit2', 'kit3']) {
  test(`real WASM ${family} prerender lifecycle collects bytes/dependencies and publishes explicit snapshots`, () => {
    const options = input(family);
    const { session, serverBuildId } = runtime.createPrerender({ input: options });
    try {
      assert.equal(serverBuildId, options.bootstrap.serverBuildId);
      assert.deepEqual(runtime.startPrerender(session), { started: true });
      assert.deepEqual(runtime.startPrerender(session), { started: true });
      const path = '/base/%F0%9F%98%80%2Fpage';
      const result = runtime.visitPrerender(session, path);
      assert.equal(result.serverBuildId, serverBuildId);
      assert.equal(result.encodedPath, path);
      assert.equal(result.decodedPath, '/base/😀%2Fpage');
      assert.equal(result.response.status, 201);
      assert.deepEqual(result.response.statusText, [99, 97, 102, 233]);
      assert.deepEqual(result.response.body, [1, 2, 0, 255, 0, 23]);
      assert.equal(result.response.routeId, '/root%2Fitem/😀');
      assert.deepEqual(result.response.headers.slice(0, 2), [[bytes('set-cookie'), bytes('a=1')], [bytes('set-cookie'), bytes('b=2')]]);
      assert.deepEqual(result.dependencies.map(dep => dep.path), [
        '/provided-empty', 'https://unused.invalid/a%2Fb/%F0%9F%98%80?q=1#fragment', '/streamed', '/late',
      ]);
      assert.deepEqual(result.dependencies[0].response.body, []);
      assert.equal(result.dependencies[1].encodedPath, '/a%2Fb/%F0%9F%98%80');
      assert.equal(result.dependencies[1].decodedPath, '/a%2Fb/😀');
      assert.equal(result.dependencies[1].response.routeId, '/dep%2Fid/😀');
      assert.deepEqual(result.dependencies[1].response.body, [0, 255]);
      assert.deepEqual(result.dependencies[2].response.body, [3, 128]);
      assert.deepEqual(result.dependencies[3].response.body, bytes('late�'));
      assert.deepEqual(runtime.publishPrerender(session, 'static.bin', [8, 9]), { published: true });
      assert.deepEqual(runtime.publishPrerender(session, '_app/asset.bin', [7]), { published: true });
      const anotherClient = new SvelteRuntime(bindings);
      assert.deepEqual(anotherClient.visitPrerender(session, '/base/next').response.body, [8, 9, 7, 1, 23]);
      // Invalid JSON field types fail before execution and do not mutate bytes.
      assert.equal(runtime.request({ op: 'publishPrerender', session, file: 'static.bin', bytes: [256] }).error.code, 'invalid_request');
      assert.deepEqual(runtime.visitPrerender(session, '/base/again').response.body, [8, 9, 7, 2, 23]);
    } finally {
      assert.deepEqual(runtime.dispose(session), { disposed: true });
    }
    assert.throws(() => runtime.visitPrerender(session, '/base/again'), { code: 'unknown_session' });
    assert.deepEqual(runtime.dispose(session), { disposed: false });
  });
}

test('real WASM prerender defers failing init and poisons only the failed rendering session', () => {
  for (const family of ['kit2', 'kit3']) {
    for (const scenario of ['before-start', 'init-fail', 'respond-fail', 'body-fail', 'missing-asset', 'client-address', 'bad-path']) {
      const { session } = runtime.createPrerender({ input: input(family, scenario) });
      try {
        const action = scenario === 'init-fail' ? () => runtime.startPrerender(session)
          : () => runtime.visitPrerender(session, scenario === 'bad-path' ? '/base/page?query=1' : '/base/page');
        if (scenario !== 'before-start' && scenario !== 'init-fail') runtime.startPrerender(session);
        assert.throws(action, error => {
          assert.ok(error instanceof SvelteRuntimeError);
          if (scenario === 'before-start') assert.match(error.message, /Start.*before visiting/);
          if (scenario === 'init-fail') assert.match(error.message, /Explicit init failure/);
          if (scenario === 'respond-fail') assert.match(error.message, /Explicit respond failure/);
          if (scenario === 'body-fail') assert.match(error.message, /Explicit body failure/);
          if (scenario === 'missing-asset') assert.match(error.message, /Missing snapshot asset/);
          if (scenario === 'bad-path') assert.match(error.message, /pathname without query or fragment/);
          return true;
        });
        assert.throws(() => runtime.startPrerender(session), { code: 'runtime_failed' });
      } finally {
        runtime.dispose(session);
      }
    }
  }
});

test('real WASM shares eight live sessions between metadata and prerender and rejects malformed create', () => {
  const caps = runtime.capabilities();
  assert.equal(caps.prerenderVisits, true);
  assert.equal(caps.prerender, false);
  assert.equal(caps.maxSessions, 8);
  const invalid = input('kit3');
  invalid.staticAssets['../escape'] = [1];
  assert.throws(() => runtime.createPrerender({ input: invalid }), { code: 'kit_prerender_input' });
  const sessions = [];
  const bootstrap = {
    serverBuildId: 'metadata-only', manifestEntry: 'server', initialization: { profile: 'kit3', server: 'server' },
    environment: {}, assets: {}, snapshot: {
      modules: { server: { source: 'export const manifest={nodes:[]};export function configure(){}', importMetaUrl: 'file:///metadata.js' } }, resolutions: {},
    },
  };
  try {
    for (let i = 0; i < 4; i++) {
      sessions.push(runtime.create({ bootstrap }).session);
      sessions.push(runtime.createPrerender({ input: input('kit3') }).session);
    }
    assert.equal(new Set(sessions).size, 8);
    assert.throws(() => runtime.loadNodes(sessions[1]), { code: 'unknown_session' });
    assert.throws(() => runtime.startPrerender(sessions[0]), { code: 'unknown_session' });
    assert.throws(() => runtime.create({ bootstrap }), { code: 'session_limit' });
    assert.throws(() => runtime.createPrerender({ input: input('kit2') }), { code: 'session_limit' });
    runtime.dispose(sessions.pop());
    sessions.push(runtime.createPrerender({ input: input('kit2') }).session);
  } finally {
    for (const session of sessions) runtime.dispose(session);
  }
});
