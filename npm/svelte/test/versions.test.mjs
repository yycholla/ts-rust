import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {spawnSync} from 'node:child_process';
import {createCompiler} from '../compiler.js';
const wasm = await WebAssembly.compile(await readFile(new URL('../generated/ts_svelte_compiler_bg.wasm', import.meta.url)));
const binary = new URL('../../../target/release/svelte-tool', import.meta.url);
function native(request) {
  const child = spawnSync(binary.pathname, [], {input:JSON.stringify(request),encoding:'utf8'});
  assert.ifError(child.error);
  return JSON.parse(child.stdout);
}
const document = text => ({uri:'Version.svelte',version:1,text});
test('version profiles isolate requests and match native output', async t => {
  const compiler=await createCompiler(wasm);t.after(()=>compiler.dispose());
  const capabilities=await compiler.request({op:'capabilities'});
  for(const svelteVersion of capabilities.result.svelteVersions) {
    const compatibility={svelteVersion};
    for(const request of [
      {op:'resolveCompatibility',versions:compatibility},
      {op:'compile',document:document('<script>let n=$state(1);</script><p>{n}</p>'),target:'client',compatibility},
      {op:'compile',document:document('<script><script>'),target:'server',compatibility},
      {op:'compile',document:document('😀\r\n{let value = 1}<p>{value}</p>'),target:'server',compatibility},
      {op:'project',document:document('{const value = 1}<p>{value}</p>'),typescript:true,compatibility},
      {op:'compile',document:document('<select defaultValue="b"><option>a</option><option>b</option></select>'),target:'client',compatibility},
      {op:'project',document:document('<script lang="ts">let n:number=1;</script>{n}'),typescript:true,compatibility},
    ]) {
      const response=await compiler.request(request);
      assert.deepEqual(response,native(request));
      assert.equal((response.ok?response.result:response).compatibility.svelteVersion,svelteVersion);
    }
  }
  assert.equal((await compiler.request({op:'compile',document:document('<p>default</p>'),target:'server'})).result.compatibility.svelteVersion,'5.53.11');
});
test('Kit stable and prerelease contracts are distinct in WASM', async t => {
  const compiler=await createCompiler(wasm);t.after(()=>compiler.dispose());
  const versions={svelteVersion:'5.57.2',kitVersion:'3.0.0-next.32',typescriptVersion:'6.0.3'};
  assert.equal((await compiler.request({op:'resolveCompatibility',versions})).error.code,'unsupported_version');
  const response=await compiler.request({op:'resolveCompatibility',versions:{...versions,allowPrerelease:true}});
  assert.equal(response.result.compatibility.channel,'experimental');
  assert.equal(response.result.compatibility.kitLibraryAlias,'#lib');
  assert.equal(response.result.compatibility.capabilities.kitBuild,false);
  for(const svelteVersion of ['latest','^5.53.11','5.50.0','5.57.999']) {
    assert.equal((await compiler.request({op:'resolveCompatibility',versions:{svelteVersion}})).error.code,'unsupported_version');
  }
});
