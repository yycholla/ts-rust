/** Usage: node scripts/svelte/lint-probe.mjs APP_CHECKOUT NEW_OUTPUT_DIRECTORY */
import {spawnSync} from 'node:child_process';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import assert from 'node:assert/strict';
if(process.argv.includes('help')||process.argv.includes('--help')){console.log('Usage: node scripts/svelte/lint-probe.mjs APP_CHECKOUT NEW_OUTPUT_DIRECTORY');process.exit(0);}
if(process.argv.length!==4)throw new Error('Expected app and new output directory');
const app=resolve(process.argv[2]),out=resolve(process.argv[3]);await mkdir(out);
const source=spawnSync('python3',['scripts/svelte/source-identity.py'],{encoding:'utf8'}).stdout;await writeFile(out+'/source.json',source);
const text="const text = 'debugger; x == 2';\nlet x = 1;\ndebugger;\nif (x == 2) x++;\nif (x === 3) x++;\n";
await writeFile(out+'/control.ts',text);await writeFile(out+'/config.json','{}\n');
function run(binary,args){const start=performance.now();const p=spawnSync(binary,args,{encoding:'utf8',timeout:180000,maxBuffer:64*1024*1024});return {status:p.status,signal:p.signal,error:p.error?.message,elapsedMs:performance.now()-start,stderr:p.stderr,stdout:p.stdout};}
const native=run(resolve('target/release/svelte-lint'),[out+'/control.ts']);
const oracle=run(resolve('target/svelte-application-deps-1/node_modules/.bin/oxlint'),['-c',out+'/config.json','--disable-nested-config','-A','all','-W','no-debugger','-W','eqeqeq','--format','json',out+'/control.ts']);
await writeFile(out+'/control-native.json',JSON.stringify(native,null,2));await writeFile(out+'/control-oracle.json',JSON.stringify(oracle,null,2));
const n=JSON.parse(native.stdout),o=JSON.parse(oracle.stdout);
const names=n.results.flatMap(r=>r.diagnostics.map(d=>d.code)).sort(),expected=o.diagnostics.map(d=>d.code.match(/\((.*)\)/)[1]).sort();
assert.deepEqual(names,expected);assert.deepEqual(names,['eqeqeq','no-debugger']);assert.equal(n.ok,true);assert.equal(native.status,1);assert.equal(oracle.status,0);
const application=run(resolve('target/release/svelte-lint'),[app+'/src','5.53.11','--runes']);await writeFile(out+'/application.json',application.stdout);await writeFile(out+'/application-process.json',JSON.stringify({...application,stdout:undefined},null,2));
const result=JSON.parse(application.stdout),counts={};for(const file of result.results)for(const d of file.diagnostics)counts[d.code]=(counts[d.code]??0)+1;
const after=spawnSync('python3',['scripts/svelte/source-identity.py'],{encoding:'utf8'}).stdout;await writeFile(out+'/source-after.json',after);
const report={scope:'Native compiler diagnostics and two script rules; control compares rule hits only, not rule messages/ranges or project ESLint/oxlint/Prettier parity',controls:{matched:true,names},application:{complete:result.ok,files:result.files,counts,errors:result.results.filter(r=>r.error).map(r=>({path:r.path,error:r.error})),status:application.status,elapsedMs:application.elapsedMs},sourceStable:source===after};
await writeFile(out+'/report.json',JSON.stringify(report,null,2)+'\n');console.log(JSON.stringify(report,null,2));process.exitCode=result.ok&&report.sourceStable?0:1;
