/** Usage: node application-browser.mjs OFFICIAL_APP NATIVE_APP NEW_OUTPUT. */
import {spawn,spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {mkdir,writeFile,readFile,readdir} from 'node:fs/promises';
import {createWriteStream}from'node:fs';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
if(process.argv.includes('--help')||process.argv.includes('help')||process.argv.length!==5){console.log('Usage: node scripts/svelte/application-browser.mjs OFFICIAL_APP NATIVE_APP NEW_OUTPUT');process.exit(process.argv.length===5?0:2);}
const [official,native,out]=process.argv.slice(2).map(p=>resolve(p));await mkdir(out);
const source=spawnSync('python3',['scripts/svelte/source-identity.py'],{encoding:'utf8'});if(source.status!==0)throw new Error('Source identity failed');await writeFile(out+'/source.json',source.stdout);
async function identify(app){const files={};async function walk(dir){for(const e of (await readdir(app+'/'+dir,{withFileTypes:true})).sort((a,b)=>a.name.localeCompare(b.name))){const name=dir+'/'+e.name;if(e.isDirectory())await walk(name);else if(e.isFile())files[name]=createHash('sha256').update(await readFile(app+'/'+name)).digest('hex');else throw new Error('Unexpected build symlink '+name);}}await walk('build');for(const name of ['package-lock.json','node_modules/svelte/package.json'])files[name]=createHash('sha256').update(await readFile(app+'/'+name)).digest('hex');return files;}
const identities={official:await identify(official),native:await identify(native)};await writeFile(out+'/artifacts.json',JSON.stringify(identities,null,2)+'\n');
const {chromium}=await import(pathToFileURL(official+'/node_modules/playwright/index.mjs'));
const executablePath=process.env.PLAYWRIGHT_CHROMIUM??'/nix/store/dypizpwkjf8v2vscas3ksz3945lhirkx-playwright-chromium-headless-shell/chrome-headless-shell-linux64/chrome-headless-shell';
const browser=await chromium.launch({executablePath,headless:true,args:['--no-sandbox']});
const reports=[];
try{for(const [mode,app]of[['official',official],['native',native]]){
 const port=mode==='official'?49171:49172,origin=`http://127.0.0.1:${port}`;
 const log=createWriteStream(out+'/'+mode+'-server.log');
 const server=spawn(process.execPath,['build/index.js'],{cwd:app,env:{PATH:process.env.PATH,NODE_ENV:'production',HOST:'127.0.0.1',PORT:String(port),ORIGIN:origin,ENABLE_REMINDER_SCHEDULER:'false'}});server.stdout.pipe(log);server.stderr.pipe(log);
 const report={mode,routes:[]};
 try{
  let ready=false;for(let n=0;n<100;n++){try{await fetch(origin+'/health');ready=true;break;}catch{await new Promise(r=>setTimeout(r,100));}}if(!ready)throw new Error('Server did not start');
  const context=await browser.newContext();await context.route('**/*',route=>route.request().url().startsWith(origin)?route.continue():route.abort());
  for(const route of ['/health','/login','/oauth/login','/docs']){
   const page=await context.newPage();const errors=[],failed=[];page.on('pageerror',e=>errors.push(e.message));page.on('requestfailed',r=>{if(r.url().startsWith(origin))failed.push(r.url().slice(origin.length));});
   const response=await page.goto(origin+route,{waitUntil:'networkidle',timeout:30000});
   const record={route,status:response.status(),url:page.url().replace(origin,''),title:await page.title(),text:(await page.locator('body').innerText()).slice(0,12000),inputs:await page.locator('input').count(),errors,failed};
   if(await page.locator('input[type=email]').count()){await page.locator('input[type=email]').first().fill('prototype@example.invalid');record.emailInteraction=await page.locator('input[type=email]').first().inputValue()==='prototype@example.invalid';}
   if(route==='/health'){const health=JSON.parse(record.text);delete health.timestamp;record.comparableText=JSON.stringify(health);record.normalization='Health timestamp only';}
   if(route==='/login'||route==='/docs'){
    const password=page.locator('[data-testid="login-password-input"]');await password.waitFor();
    const toggle=password.locator('..').locator('button[type="button"]');
    await toggle.click();await page.waitForFunction(()=>document.querySelector('[data-testid="login-password-input"]').type==='text');
    await toggle.click();await page.waitForFunction(()=>document.querySelector('[data-testid="login-password-input"]').type==='password');
    record.passwordToggle=true;
   }
   record.buttons=await page.locator('button').allTextContents();await page.screenshot({path:out+'/'+mode+'-'+route.replaceAll('/','_')+'.png',fullPage:true});report.routes.push(record);await page.close();
  }
  await context.close();
 }catch(error){report.error=String(error.stack);}finally{const exited=new Promise(r=>server.on('exit',r));server.kill();const force=setTimeout(()=>server.kill('SIGKILL'),3000);await exited;clearTimeout(force);log.end();}
 reports.push(report);
}}finally{await browser.close();}
const comparisons=reports[0]?.routes.map((a,i)=>{const b=reports[1]?.routes[i];return {route:a.route,matched:!!b&&a.status===b.status&&a.url===b.url&&(a.comparableText??a.text)===(b.comparableText??b.text)&&JSON.stringify(a.errors)===JSON.stringify(b.errors)&&a.inputs===b.inputs&&a.failed.length===0&&b.failed.length===0&&a.passwordToggle===b.passwordToggle,officialStatus:a.status,nativeStatus:b?.status,nativeErrors:b?.errors,hydratedInputInteraction:b?.emailInteraction,svelteStateToggle:b?.passwordToggle};})||[];
const after=spawnSync('python3',['scripts/svelte/source-identity.py'],{encoding:'utf8'});await writeFile(out+'/source-after.json',after.stdout);
const sourceStable=after.status===0&&source.stdout===after.stdout,artifactsStable=JSON.stringify(identities)===JSON.stringify({official:await identify(official),native:await identify(native)});
const result={reports,comparisons,sourceStable,artifactsStable,passed:sourceStable&&artifactsStable&&reports.length===2&&reports.every(r=>!r.error)&&comparisons.every(c=>c.matched&&c.nativeStatus<500&&c.nativeErrors.length===0&&(!['/login','/docs'].includes(c.route)||c.svelteStateToggle===true))};
await writeFile(out+'/report.json',JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify({passed:result.passed,comparisons,errors:reports.map(r=>r.error)},null,2));process.exitCode=result.passed?0:1;
