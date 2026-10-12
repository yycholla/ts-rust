import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';
import { initializeRuntime, SvelteRuntime, SvelteRuntimeError } from '../index.js';

// No mocked transport or fallback: a missing/stale build fails this suite.
const bindingsUrl = process.env.TS_SVELTE_RUNTIME_BINDINGS
  ? pathToFileURL(resolve(process.env.TS_SVELTE_RUNTIME_BINDINGS))
  : new URL('../generated/ts_svelte_runtime.js', import.meta.url);
const wasmUrl = process.env.TS_SVELTE_RUNTIME_WASM
  ? pathToFileURL(resolve(process.env.TS_SVELTE_RUNTIME_WASM))
  : new URL('../generated/ts_svelte_runtime_bg.wasm', import.meta.url);
const bindings = await import(bindingsUrl.href);
const runtime = await initializeRuntime({ bindings, moduleOrPath: await readFile(wasmUrl) });

function fixture(profile, { failNode = false, hostOptions = false } = {}) {
  const internal = `
    export const state={}; export const events=[];
    export function set_building(){events.push('building');state.building=true}
    export function set_private_env(env){events.push('private');state.private=env}
    export function set_public_env(env){events.push('public');state.public=env}
    export function set_manifest(manifest){events.push('manifest');state.manifest=manifest}
    export function set_read_implementation(read){events.push('read');state.read=read}
    export function init(){throw Error('Application init must not run during analysis')}
    export async function configure(config){
      events.push('configure:start'); await 0;
      if(config.building!==true)throw Error('Expected build initialization');
      Object.assign(state,config);events.push('configure:end');
    }
  `;
  const expectedEvents = profile === 'kit2'
    ? ['building', 'private', 'public', 'manifest', 'read', 'env']
    : ['configure:start', 'configure:end'];
  const manifest = `
    import {state,events} from './internal.js';
    let first=0,second=0;
    const nodes=[async()=>{
      if(++first!==1)throw Error('Node loader repeated');
      if(JSON.stringify(events)!==${JSON.stringify(JSON.stringify(expectedEvents))})throw Error('Initialization order');
      if(state.manifest!==manifest||!state.building||state.env.SECRET!=='private')throw Error('Initialization identity');
      ${profile === 'kit2' ? "if(JSON.stringify(state.private)!=='{\"SECRET\":\"private\"}'||JSON.stringify(state.public)!=='{\"PUBLIC_NAME\":\"public\"}')throw Error('Environment filtering');" : ''}
      events.push('first:start');await 0;
      if(!events.includes('second:start'))throw Error('Node loaders must start concurrently');
      ${failNode ? "throw Error('explicit node failure');" : ''}
      const response=new Response(state.read('assets/greeting.txt'));
      const twin=response.clone();
      if(await response.text()!=='native 😀 asset'||await twin.text()!=='native 😀 asset')throw Error('Asset bytes/stream clone');
      if(typeof process!=='undefined'||typeof fetch!=='undefined')throw Error('Implicit host access');
      ${hostOptions ? "if(new Request('/from-node').url!=='https://example.test/from-node')throw Error('Fetch host options');if(crypto.randomUUID()!=='00010203-0405-4607-8809-0a0b0c0d0e0f')throw Error('Explicit entropy');" : ''}
      return import('./node.js');
    },async()=>{
      if(++second!==1)throw Error('Second node loader repeated');
      events.push('second:start');await 0;
      return {index:1,server:{trailingSlash:'always'},universal:{}};
    }];
    export const manifest=${profile === 'kit2' ? '{_:{nodes}}' : '{nodes}'};
  `;
  const module = (source, name) => ({ source, importMetaUrl: `file:///fixture/${name}.mjs` });
  return {
    serverBuildId: `wasm-${profile}-fixture`,
    snapshot: {
      modules: {
        internal: module(internal, 'internal'),
        manifest: module(manifest, 'manifest'),
        environment: module("import {state,events} from './internal.js';export function set_env(env){state.env=env;events.push('env')}", 'environment'),
        node: module("export const index=0;export const server={load:null};export const universal={load(){throw Error('Load function must not execute')}};", 'node'),
      },
      resolutions: {
        manifest: { './internal.js': { kind: 'module', id: 'internal' }, './node.js': { kind: 'module', id: 'node' } },
        environment: { './internal.js': { kind: 'module', id: 'internal' } },
      },
    },
    manifestEntry: 'manifest',
    initialization: profile === 'kit2'
      ? { profile, internal: 'internal', environment: 'environment', publicPrefix: 'PUBLIC_', privatePrefix: '' }
      : { profile, server: 'internal' },
    environment: { SECRET: 'private', PUBLIC_NAME: 'public' },
    assets: { 'assets/greeting.txt': Array.from(new TextEncoder().encode('native 😀 asset')) },
  };
}
const expectedNodes = [
  { index: 0, hasServerLoad: true, hasUniversalLoad: true, serverLoadCallable: false, universalLoadCallable: true },
  { index: 1, hasServerLoad: true, hasUniversalLoad: false, serverLoadCallable: false, universalLoadCallable: false },
];

test('real WASM advertises explicit framework boundaries and preserves protocol errors', () => {
  const caps = runtime.capabilities();
  assert.equal(caps.engine, 'boa');
  assert.equal(caps.framework, 'sveltekit');
  assert.equal(caps.persistent, true);
  assert.deepEqual(caps.initializationProfiles, ['kit2', 'kit3']);
  assert.equal(caps.nodeLoads, true);
  assert.equal(caps.routeMetadata, true);
  assert.equal(caps.maxSessions, 8);
  assert.equal(caps.networkTransport, false);
  assert.equal(caps.implicitFilesystem, false);
  const failure = runtime.request({ op: 'loadNodes', session: 'missing' });
  assert.equal(failure.ok, false);
  assert.equal(failure.error.code, 'unknown_session');
  assert.throws(() => runtime.loadNodes('missing'), error => {
    assert.ok(error instanceof SvelteRuntimeError);
    assert.equal(error.code, failure.error.code);
    assert.equal(error.message, failure.error.message);
    assert.deepEqual(error.detail, failure.error);
    return true;
  });
  const malformed = runtime.request({ op: 'capabilities', unexpected: true });
  assert.equal(malformed.ok, false, 'The strict protocol must reject unknown capabilities fields');
  assert.equal(malformed.error.code, 'invalid_request');
});

for (const profile of ['kit2', 'kit3']) {
  test(`real WASM ${profile} initializes before concurrent cached node imports and native asset reads`, () => {
    const created = runtime.create({
      bootstrap: fixture(profile, { hostOptions: true }),
      fetchOptions: { baseUrl: 'https://example.test/base/' },
      cryptoOptions: { entropy: Array.from({ length: 16 }, (_, i) => i) },
    });
    try {
      assert.equal(created.serverBuildId, `wasm-${profile}-fixture`);
      const result = runtime.loadNodes(created.session);
      assert.deepEqual(result, { serverBuildId: created.serverBuildId, nodes: expectedNodes });
      // A second wrapper over the same WASM module shares its native service.
      // Repeating the operation cannot rerun loaders or consume entropy again.
      const anotherClient = new SvelteRuntime(bindings);
      assert.deepEqual(anotherClient.loadNodes(created.session), result);
    } finally {
      assert.deepEqual(runtime.dispose(created.session), { disposed: true });
    }
    assert.deepEqual(runtime.dispose(created.session), { disposed: false });
    assert.throws(() => runtime.loadNodes(created.session), { code: 'unknown_session' });
  });
}

test('real WASM loader failure poisons the session without replacing diagnostics or assets', () => {
  for (const failure of ['loader', 'asset']) {
    const bootstrap = fixture('kit3', { failNode: failure === 'loader' });
    if (failure === 'asset') bootstrap.assets = {};
    const { session } = runtime.create({ bootstrap });
    try {
      assert.throws(() => runtime.loadNodes(session), error => {
        assert.ok(error instanceof SvelteRuntimeError);
        assert.match(error.message, failure === 'loader' ? /explicit node failure/ : /Missing snapshot server asset/);
        return true;
      });
      // Runtime::check exposes the stable runtime_failed code; "poisoned"
      // describes its state/message, not a separate protocol error code.
      assert.throws(() => runtime.loadNodes(session), { code: 'runtime_failed' });
    } finally {
      runtime.dispose(session);
    }
  }
});

test('real WASM enforces eight live sessions and failed creation does not consume a slot', () => {
  const invalid = fixture('kit3');
  invalid.assets['../outside'] = [1];
  assert.throws(() => runtime.create({ bootstrap: invalid }), { code: 'kit_runtime_input' });
  const sessions = [];
  try {
    for (let i = 0; i < 8; i++) sessions.push(runtime.create({ bootstrap: fixture('kit3') }).session);
    assert.equal(new Set(sessions).size, 8);
    assert.throws(() => runtime.create({ bootstrap: fixture('kit3') }), { code: 'session_limit' });
    runtime.dispose(sessions.pop());
    sessions.push(runtime.create({ bootstrap: fixture('kit3') }).session);
  } finally {
    for (const session of sessions) runtime.dispose(session);
  }
});

test('real WASM metadata preserves inherited options, version precedence and cached async entries', () => {
  const json = value => ({ kind: 'json', value });
  for (const [family, profile, region] of [['kit2', '2.69.2', 'server'], ['kit3', '3.0.1', 'universal']]) {
    const bootstrap = fixture(family);
    bootstrap.snapshot.modules.manifest.source = `
      let entryCalls=0,endpointCalls=0;
      const loaded=[
        {index:0,universal:{config:{region:'universal',ancestor:true},csr:false,ssr:false,prerender:true,trailingSlash:'always'},server:{config:{region:'server'}}},
        {index:1,universal:{config:{leaf:true},ssr:true,prerender:false,async entries(){
          if(++entryCalls!==1)throw Error('Entries evaluated twice');await 0;return [{slug:'first'},{slug:'雪'}];
        }},server:{actions:{save(){throw Error('Action invoked')},bad:17}}},
        {index:2,universal:{config:{reset:true}}}
      ];
      const nodes=loaded.map(node=>async()=>node);
      const routes=[
        {id:'/(group)/[slug]',page:{layouts:[0,undefined],leaf:1}},
        {id:'/reset',page:{layouts:[],leaf:2}},
        {id:'/api',async endpoint(){if(++endpointCalls!==1)throw Error('Endpoint imported twice');await 0;return {GET(){throw Error('Handler invoked')}}}}
      ];
      export const manifest=${family === 'kit2' ? '{_:{nodes,routes}}' : '{nodes,routes}'};
    `;
    const { session } = runtime.create({ bootstrap });
    try {
      const options = { profile };
      const result = runtime.analyzeMetadata(session, options);
      assert.equal(result.serverBuildId, bootstrap.serverBuildId);
      assert.equal(result.nodes.length, 3);
      assert.equal(result.routes.length, 3);
      assert.deepEqual(result.routes[0], {
        id: '/(group)/[slug]',
        config: json({ region, ancestor: true, leaf: true }),
        prerender: json(false),
        entries: json(['/first', '/雪']),
        methods: ['GET', 'POST'],
        pageMethods: ['GET', 'POST'],
        endpointMethods: [],
        endpoint: null,
        page: {
          csr: json(false), ssr: json(true), prerender: json(false), trailingSlash: json('always'),
          actions: [{ name: 'save', callable: true }, { name: 'bad', callable: false }],
        },
      });
      assert.deepEqual(result.routes[1].config, json({ reset: true }));
      assert.deepEqual(result.routes[1].page.csr, json(true));
      assert.deepEqual(result.routes[1].entries, { kind: 'undefined' });
      assert.equal(result.routes[2].page, null);
      // Both pinned analyse.js endpoint paths use mod.prerender ?? false.
      // An absent entries export, unlike this normalized option, stays undefined.
      assert.deepEqual(result.routes[2].prerender, json(false));
      assert.deepEqual(result.routes[2].entries, { kind: 'undefined' });
      assert.deepEqual(result.routes[2].endpointMethods, ['GET']);
      assert.deepEqual(result.routes[2].endpoint, { trailingSlash: { kind: 'undefined' } });
      assert.deepEqual(runtime.analyzeMetadata(session, { profile, hash: false }), result);
      assert.throws(() => runtime.analyzeMetadata(session, { profile, hash: true }), error => {
        assert.equal(error.code, 'kit_runtime_input');
        assert.match(error.message, /options cannot change/);
        return true;
      });
      assert.throws(() => runtime.analyzeMetadata(session, { profile: family === 'kit2' ? '3.0.1' : '2.69.2' }), error => {
        assert.equal(error.code, 'kit_runtime_input');
        assert.match(error.message, /initialized Kit family/);
        return true;
      });
      assert.throws(() => runtime.analyzeMetadata(session, { profile: family === 'kit2' ? '2.69.3' : '3.0.0-next.32' }), error => {
        assert.equal(error.code, 'kit_runtime_input');
        assert.match(error.message, /options cannot change/);
        return true;
      });
      assert.equal(runtime.request({ op: 'analyzeMetadata', session, options: { profile: 'latest' } }).error.code, 'invalid_request');
      // Rejected option changes do not poison or invalidate the existing cache.
      assert.deepEqual(runtime.analyzeMetadata(session, options), result);
    } finally {
      runtime.dispose(session);
    }
  }
});

test('real WASM remote metadata uses route-first module imports, preserves undefined and caches', () => {
  const value = value => ({ kind: 'json', value });
  for (const [family, profile] of [['kit2', '2.69.2'], ['kit3', '3.0.1']]) {
    const bootstrap = fixture(family);
    bootstrap.snapshot.modules.manifest.source = `
      let entries=0,remoteCalls=0;
      export let routeComplete=false;
      const nodes=[async()=>({index:0,universal:{async entries(){
        if(++entries!==1)throw Error('Entries repeated');await 0;
        routeComplete=true;return [{slug:'native'}];
      }}})];
      const routes=[{id:'/[slug]',page:{layouts:[],leaf:0}}];
      const remotes={hash:async function(){
        if(this!==undefined)throw Error('Remote loader receiver');
        if(!routeComplete||++remoteCalls!==1)throw Error('Remote order/cache');
        await 0;return import('./remote-wrapper.js');
      }};
      export const manifest=${family === 'kit2' ? '{_:{nodes,routes,remotes}}' : '{nodes,routes,remotes}'};
    `;
    bootstrap.snapshot.modules.remoteWrapper = {
      source: "import * as functions from './remote-functions.js'; export default functions;",
      importMetaUrl: 'file:///fixture/remote-wrapper.mjs',
    };
    bootstrap.snapshot.modules.remoteFunctions = {
      source: `
        import {routeComplete} from './manifest.js';
        if(!routeComplete)throw Error('Remote source evaluated before routes');
        function query(){throw Error('Analysis invoked query')}
        function prerender(){throw Error('Analysis invoked prerender')}
        Object.defineProperty(query,'__',{value:{type:'query',id:'hash/query',name:'query'}});
        Object.defineProperty(prerender,'__',{value:{type:'prerender',id:'hash/prerender',name:'prerender',inputs(){throw Error('Analysis invoked inputs')}}});
        export {query,prerender}`,
      importMetaUrl: 'file:///fixture/remote-functions.mjs',
    };
    bootstrap.snapshot.resolutions.manifest['./remote-wrapper.js'] = { kind: 'module', id: 'remoteWrapper' };
    bootstrap.snapshot.resolutions.remoteWrapper = { './remote-functions.js': { kind: 'module', id: 'remoteFunctions' } };
    bootstrap.snapshot.resolutions.remoteFunctions = { './manifest.js': { kind: 'module', id: 'manifest' } };
    const { session } = runtime.create({ bootstrap });
    const chunks = [{ hash: 'hash', file: 'src/functions.remote.ts' }];
    try {
      assert.equal(runtime.capabilities().remoteMetadata, true);
      assert.throws(() => runtime.analyzeRemotes(session, chunks), error => {
        assert.equal(error.code, 'kit_runtime_input');
        assert.match(error.message, /route metadata before remote/);
        return true;
      });
      const routes = runtime.analyzeMetadata(session, { profile });
      assert.deepEqual(routes.routes[0].entries, value(['/native']));
      const result = runtime.analyzeRemotes(session, chunks);
      assert.deepEqual(result, {
        serverBuildId: bootstrap.serverBuildId,
        remotes: [{
          hash: 'hash', file: 'src/functions.remote.ts', exports: [
            { name: 'prerender', kind: value('prerender'), dynamic: { kind: 'undefined' } },
            { name: 'query', kind: value('query'), dynamic: value(true) },
          ],
        }],
      });
      assert.deepEqual(new SvelteRuntime(bindings).analyzeRemotes(session, chunks), result);
      assert.deepEqual(runtime.analyzeMetadata(session, { profile }), routes);
      assert.throws(() => runtime.analyzeRemotes(session, []), error => {
        assert.equal(error.code, 'kit_runtime_input');
        assert.match(error.message, /inventory cannot change/);
        return true;
      });
      assert.deepEqual(runtime.analyzeRemotes(session, chunks), result);
    } finally {
      runtime.dispose(session);
    }
    assert.throws(() => runtime.analyzeRemotes(session, chunks), { code: 'unknown_session' });
  }
});
