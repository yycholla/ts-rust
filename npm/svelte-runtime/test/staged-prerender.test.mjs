import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import test from 'node:test';
import { initializeRuntime, SvelteRuntime, SvelteRuntimeError } from '../index.js';

// Actual WASM only: absent/stale generated bindings are failures, not skips.
const bindingsUrl = process.env.TS_SVELTE_RUNTIME_BINDINGS
  ? pathToFileURL(resolve(process.env.TS_SVELTE_RUNTIME_BINDINGS))
  : new URL('../generated/ts_svelte_runtime.js', import.meta.url);
const wasmUrl = process.env.TS_SVELTE_RUNTIME_WASM
  ? pathToFileURL(resolve(process.env.TS_SVELTE_RUNTIME_WASM))
  : new URL('../generated/ts_svelte_runtime_bg.wasm', import.meta.url);
const bindings = await import(bindingsUrl.href);
const runtime = await initializeRuntime({ bindings, moduleOrPath: await readFile(wasmUrl) });
const bytes = text => Array.from(new TextEncoder().encode(text));

function input(family, failBody = false) {
  const state = `
    export const manifest={assets:new Set()};
    export const state={};
    export function set_building(){}
    export function set_prerendering(){}
    export function set_env(){}
    export function set_private_env(){}
    export function set_public_env(){}
    export function set_manifest(){}
    export function set_read_implementation(){}
  `;
  const server = `
    import {manifest,state} from './state.js';
    let visits=0;
    async function init(){state.started=true}
    async function respond(request,options){
      if(!state.started)throw Error('Not initialized');
      const visit=visits++;
      const dependencies=options.prerendering.dependencies;
      const response=new Response('must not consume fallback',{
        headers:[['x-sveltekit-routeid','%malformed-until-body'],['x-stage','before']]
      });
      let read=false;
      dependencies.set('/first%2Fdep',{
        response,
        get body(){
          if(read)throw Error('Body getter repeated');read=true;
          if(${failBody})throw Error('Deferred dependency body failure');
          const saved=options.read('main.html');
          if(saved.length!==2)throw Error('Main was not saved before dependency body');
          response.headers.set('x-sveltekit-routeid','/dep/%F0%9F%98%80');
          response.headers.set('x-stage','after');
          dependencies.set('/late',{
            response:new Response('unused'),
            get body(){return options.read('streamed.bin')}
          });
          return new Uint8Array([...saved,255]);
        }
      });
      dependencies.set('/streamed',{
        response:new Response(new ReadableStream({
          async pull(controller){
            await 0;
            controller.enqueue(options.read('first.bin'));
            controller.close();
          }
        },{highWaterMark:0})),
        body:null
      });
      return new Response(new ReadableStream({
        async pull(controller){
          await 0;
          controller.enqueue(new Uint8Array([...options.read('seed.bin'),visit]));
          controller.close();
        }
      },{highWaterMark:0}),{
        status:201,statusText:'caf\\u00e9',headers:[
          ['x-sveltekit-routeid','/page/%F0%9F%98%80'],['set-cookie','a=1'],['set-cookie','b=2']
        ]
      });
    }
    ${family === 'kit3' ? `
      export function configure(options){
        if(options.manifest!==manifest)throw Error('Wrong manifest');
        return {init,respond};
      }
    ` : `
      export class Server{
        constructor(value){if(value!==manifest)throw Error('Wrong manifest')}
        init(){return init()}
        respond(request,options){return respond(request,options)}
      }
    `}
  `;
  const module = (source, name) => ({ source, importMetaUrl: `file:///staged/${name}.js` });
  return {
    bootstrap: {
      serverBuildId: `staged-${family}-${failBody}`, manifestEntry: 'state',
      snapshot: {
        modules: { state: module(state, 'state'), server: module(server, 'server') },
        resolutions: { server: { './state.js': { kind: 'module', id: 'state' } } },
      },
      initialization: family === 'kit3' ? { profile: family, server: 'server' }
        : { profile: family, internal: 'state', environment: 'state', publicPrefix: 'PUBLIC_', privatePrefix: '' },
      environment: {}, assets: {},
    },
    serverEntry: 'server', origin: 'https://staged.test', base: '', appDir: '_app',
    staticAssets: { 'seed.bin': [0] },
  };
}

function started(family, failBody = false) {
  const created = runtime.createPrerender({ input: input(family, failBody) });
  runtime.startPrerender(created.session);
  return created;
}

for (const family of ['kit2', 'kit3']) {
  test(`real WASM ${family} staged body reads observe published artifacts and live dependencies`, () => {
    assert.equal(runtime.capabilities().stagedPrerenderVisits, true);
    assert.equal(runtime.capabilities().prerender, false);
    const { session, serverBuildId } = started(family);
    let previousToken = 0;
    try {
      for (let visit = 0; visit < 2; visit++) {
        const head = runtime.beginPrerenderVisit(session, '/%F0%9F%98%80%2Fpage');
        assert.ok(Number.isSafeInteger(head.token) && head.token > previousToken);
        previousToken = head.token;
        assert.equal(head.serverBuildId, serverBuildId);
        assert.equal(head.encodedPath, '/%F0%9F%98%80%2Fpage');
        assert.equal(head.decodedPath, '/😀%2Fpage');
        assert.equal(head.response.status, 201);
        assert.deepEqual(head.response.statusText, [99, 97, 102, 233]);
        assert.equal(head.response.routeId, '/page/😀');
        assert.equal(Object.hasOwn(head.response, 'body'), false);
        assert.deepEqual(head.response.headers.filter(([name]) => String.fromCharCode(...name) === 'set-cookie'), [
          [bytes('set-cookie'), bytes('a=1')], [bytes('set-cookie'), bytes('b=2')],
        ]);
        runtime.publishPrerender(session, 'seed.bin', [5 + visit]);
        const main = runtime.readPrerenderMain(session, head.token);
        assert.deepEqual(main.body, [5 + visit, visit]);
        runtime.publishPrerender(session, 'main.html', main.body);

        const depHead = runtime.nextPrerenderDependency(session, head.token);
        assert.equal(depHead.path, '/first%2Fdep');
        assert.equal(depHead.decodedPath, '/first%2Fdep');
        assert.equal(depHead.response.routeId, null);
        assert.equal(Object.hasOwn(depHead.response, 'body'), false);
        assert.ok(depHead.response.headers.some(([name, value]) =>
          String.fromCharCode(...name) === 'x-stage' && String.fromCharCode(...value) === 'before'));
        // Replace the saved main between inspecting the dependency and invoking
        // its body getter. Getter execution must happen at readDependency.
        runtime.publishPrerender(session, 'main.html', [9, visit]);
        const first = new SvelteRuntime(bindings).readPrerenderDependency(session, head.token);
        assert.deepEqual(first.response.body, [9, visit, 255]);
        assert.equal(first.response.routeId, '/dep/😀');
        assert.ok(first.response.headers.some(([name, value]) =>
          String.fromCharCode(...name) === 'x-stage' && String.fromCharCode(...value) === 'after'));
        runtime.publishPrerender(session, 'first.bin', first.response.body);

        assert.equal(runtime.nextPrerenderDependency(session, head.token).path, '/streamed');
        const streamed = runtime.readPrerenderDependency(session, head.token);
        assert.deepEqual(streamed.response.body, first.response.body);
        runtime.publishPrerender(session, 'streamed.bin', [0, 128, 255]);
        assert.equal(runtime.nextPrerenderDependency(session, head.token).path, '/late');
        assert.deepEqual(runtime.readPrerenderDependency(session, head.token).response.body, [0, 128, 255]);
        assert.equal(runtime.nextPrerenderDependency(session, head.token), null);
      }
      // The convenience API still completes a visit after staged completion.
      const collected = runtime.visitPrerender(session, '/convenience');
      assert.deepEqual(collected.response.body, [6, 2]);
      assert.deepEqual(collected.dependencies.map(dependency => dependency.path), ['/first%2Fdep', '/streamed', '/late']);
    } finally {
      runtime.dispose(session);
    }
    assert.throws(() => runtime.beginPrerenderVisit(session, '/disposed'), { code: 'unknown_session' });
  });

  test(`real WASM ${family} dependency failure occurs during read rather than head inspection`, () => {
    const { session } = started(family, true);
    try {
      const head = runtime.beginPrerenderVisit(session, '/failure');
      runtime.readPrerenderMain(session, head.token);
      assert.equal(runtime.nextPrerenderDependency(session, head.token).path, '/first%2Fdep');
      assert.throws(() => runtime.readPrerenderDependency(session, head.token), error => {
        assert.ok(error instanceof SvelteRuntimeError);
        assert.match(error.message, /Deferred dependency body failure/);
        return true;
      });
      assert.throws(() => runtime.beginPrerenderVisit(session, '/after'), { code: 'runtime_failed' });
    } finally {
      runtime.dispose(session);
    }
  });
}

test('real WASM staged ordering and token misuse poison only the affected session', () => {
  const cases = {
    overlapping: (session, token) => runtime.beginPrerenderVisit(session, '/overlap'),
    convenienceOverlap: session => runtime.visitPrerender(session, '/overlap'),
    dependencyBeforeMain: (session, token) => runtime.nextPrerenderDependency(session, token),
    readWithoutInspection: (session, token) => runtime.readPrerenderDependency(session, token),
    wrongToken: (session, token) => runtime.readPrerenderMain(session, token + 1),
    repeatedMain(session, token) {
      runtime.readPrerenderMain(session, token);
      runtime.readPrerenderMain(session, token);
    },
    advanceUnreadDependency(session, token) {
      runtime.readPrerenderMain(session, token);
      runtime.nextPrerenderDependency(session, token);
      runtime.nextPrerenderDependency(session, token);
    },
  };
  for (const family of ['kit2', 'kit3']) {
    for (const [name, misuse] of Object.entries(cases)) {
      const { session } = started(family);
      try {
        const { token } = runtime.beginPrerenderVisit(session, '/misuse');
        assert.throws(() => misuse(session, token), SvelteRuntimeError, name);
        assert.throws(() => runtime.startPrerender(session), { code: 'runtime_failed' }, name);
      } finally {
        runtime.dispose(session);
      }
    }
    const { session } = started(family);
    try {
      const { token } = runtime.beginPrerenderVisit(session, '/finished');
      const main = runtime.readPrerenderMain(session, token);
      runtime.publishPrerender(session, 'main.html', main.body);
      while (runtime.nextPrerenderDependency(session, token)) {
        const dependency = runtime.readPrerenderDependency(session, token);
        const file = dependency.path === '/first%2Fdep' ? 'first.bin' : 'streamed.bin';
        runtime.publishPrerender(session, file, dependency.response.body);
      }
      const next = runtime.beginPrerenderVisit(session, '/new-token');
      assert.notEqual(next.token, token);
      assert.throws(() => runtime.readPrerenderMain(session, token), SvelteRuntimeError);
      assert.throws(() => runtime.readPrerenderMain(session, next.token), { code: 'runtime_failed' });
    } finally {
      runtime.dispose(session);
    }
  }
});
