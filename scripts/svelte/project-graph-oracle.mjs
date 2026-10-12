/** Usage: node project-graph-oracle.mjs APP NATIVE_GRAPH NEW_OUTPUT */
import {readFile,mkdir,writeFile} from 'node:fs/promises';
import {resolve,relative} from 'node:path';
import {pathToFileURL} from 'node:url';
if(process.argv.length!==5||process.argv.includes('--help')){console.log('Usage: node scripts/svelte/project-graph-oracle.mjs APP NATIVE_GRAPH NEW_OUTPUT');process.exit(0);}
const [app,graphPath,out]=process.argv.slice(2).map(p=>resolve(p));await mkdir(out);
const {default:ts}=await import(pathToFileURL(app+'/node_modules/typescript/lib/typescript.js'));
const {config,error}=ts.readConfigFile(app+'/tsconfig.json',ts.sys.readFile);if(error)throw Error(ts.flattenDiagnosticMessageText(error.messageText,'\n'));
const parsed=ts.parseJsonConfigFileContent(config,ts.sys,app,undefined,app+'/tsconfig.json',undefined,[{extension:'svelte',isMixedContent:true,scriptKind:ts.ScriptKind.Deferred}]);
const native=JSON.parse(await readFile(graphPath,'utf8'));
const officialRoots=parsed.fileNames.map(p=>relative(app,p)).sort(),nativeRoots=native.inputs.filter(i=>i.root).map(i=>i.path).sort();
const officialSet=new Set(officialRoots),nativeSet=new Set(nativeRoots);
const result={officialRoots,nativeRoots,missing:officialRoots.filter(p=>!nativeSet.has(p)),extra:nativeRoots.filter(p=>!officialSet.has(p)),options:parsed.options,errors:parsed.errors.map(e=>({code:e.code,message:ts.flattenDiagnosticMessageText(e.messageText,'\n')}))};
await writeFile(out+'/report.json',JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify({official:officialRoots.length,native:nativeRoots.length,missing:result.missing,extra:result.extra,errors:result.errors},null,2));

process.exitCode=result.missing.length||result.extra.length||result.errors.length?1:0;
