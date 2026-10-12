import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import init, { tailwind_json } from '../generated/ts_tailwind.js';
await init({module_or_path: await readFile(new URL('../generated/ts_tailwind_bg.wasm', import.meta.url))});
const call = (request) => {
  const response = JSON.parse(tailwind_json(JSON.stringify(request)));
  assert.equal(response.ok, true, JSON.stringify(response));
  return response.result;
};
const prepare = (css, plugins = []) => call({op:'prepare', snapshot:{tailwindVersion:'4.2.1', entry:'app.css',stylesheets:[{id:'app.css',version:1,css}],plugins},options:{imports:{bases:{'app.css':'/app'}},licenseComment:false}}).session;
test('WASM selects Svelte sources and replaces generated CSS after edits', () => {
  const session=prepare('@theme { --color-brand: red; } @tailwind utilities;');
  const inventory={files:[{id:'component',path:'/app/A.svelte',binary:false}],automaticEligibleIds:['component']};
  const build=(text,version)=>call({op:'buildProject',session,sources:[{id:'component',version,extension:'svelte',text}],inventory});
  const first=build('<div class="bg-brand hover:flex" class:underline={active}/>',1);
  assert.match(first.stylesheet.css,/background-color: var\(--color-brand\)/);
  assert.match(first.stylesheet.css,/\.underline/);
  assert.equal(first.selection.selected[0].id,'component');
  const second=build('<div class="grid"/>',2);
  assert.match(second.stylesheet.css,/display: grid/);
  assert.doesNotMatch(second.stylesheet.css,/--color-brand|\.underline/);
  assert.equal(second.extraction.sources[0].version,2);
  assert.equal(call({op:'dispose',session}).disposed,true);
});
test('WASM executes the native Typography plugin', () => {
  const session=prepare('@plugin "@tailwindcss/typography"; @tailwind utilities;', [{specifier:'@tailwindcss/typography',implementation:'@tailwindcss/typography',version:'0.5.19'}]);
  const result=call({op:'build',session,candidates:['prose','prose-headings:underline']});
  assert.deepEqual(result.unsupported,[]);
  assert.deepEqual(result.invalid,[]);
  assert.match(result.css,/--tw-prose-body/);
  assert.match(result.css,/text-decoration-line: underline/);
  assert.doesNotMatch(result.css,/@plugin|@tailwind/);
  call({op:'dispose',session});
});
