#!/usr/bin/env node
/** Usage: node scripts/svelte/version-exports.mjs
 * Generate exact runtime export manifests from installed pinned oracle packages.
 * Parses source with TypeScript; refuses unresolved export-star and parse errors.
 */
import ts from '../../target/svelte-versions/svelte-5.53.11/node_modules/typescript/lib/typescript.js';
import {readFile,writeFile,mkdir} from 'node:fs/promises';
import {createHash} from 'node:crypto';
const versions=['5.51.5','5.52.0','5.53.5','5.53.11','5.53.13','5.54.1','5.55.10','5.56.10','5.57.2'];
if(process.argv.includes('help')||process.argv.includes('--help')){console.log('Usage: node scripts/svelte/version-exports.mjs');process.exit(0);}
const out='crates/ts_svelte_compiler/src/profiles';await mkdir(out,{recursive:true});
const provenance={};
for(const version of versions)for(const target of ['client','server']){
 const file=`target/svelte-versions/svelte-${version}/node_modules/svelte/src/internal/${target}/index.js`;
 const text=await readFile(file,'utf8');const ast=ts.createSourceFile(file,text,ts.ScriptTarget.Latest,true,ts.ScriptKind.JS);
 if(ast.parseDiagnostics.length)throw new Error(`Invalid runtime source ${file}`);
 const names=[];
 for(const s of ast.statements){
  if(ts.isExportDeclaration(s)){
   if(!s.exportClause||!ts.isNamedExports(s.exportClause))throw new Error(`Unresolved export ${file}`);
   for(const e of s.exportClause.elements)names.push(e.name.text);
  }else if(s.modifiers?.some(m=>m.kind===ts.SyntaxKind.ExportKeyword)){
   if(ts.isVariableStatement(s))for(const d of s.declarationList.declarations){if(!ts.isIdentifier(d.name))throw new Error('Destructuring export');names.push(d.name.text);}
   else if(s.name&&ts.isIdentifier(s.name))names.push(s.name.text);
   else throw new Error(`Unresolved declaration export ${file}`);
  }
 }
 const manifest=[...new Set(names)].sort().join('\n')+'\n';
 await writeFile(`${out}/${target}-${version}.txt`,manifest);
 provenance[`${target}-${version}`]={version,source:file,sourceSha256:createHash('sha256').update(text).digest('hex'),exportsSha256:createHash('sha256').update(manifest).digest('hex'),count:names.length};
}
await writeFile(`${out}/provenance.json`,JSON.stringify(provenance,null,2)+'\n');
console.log('Generated',Object.keys(provenance).length,'runtime manifests');
