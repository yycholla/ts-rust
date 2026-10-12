#!/usr/bin/env python3
"""Usage: application-build.py SOURCE OUTPUT {official,native}
Build an isolated snapshot, excluding environment/secrets, with uploads and
background jobs disabled. Native mode replaces Svelte compilation only.
"""
import hashlib,json,os,shutil,subprocess,sys,time,signal
from pathlib import Path
if len(sys.argv)!=4 or sys.argv[1] in ('help','--help'):
 print(__doc__);sys.exit(0 if len(sys.argv)>1 and sys.argv[1] in ('help','--help') else 2)
source=Path(sys.argv[1]).resolve();out=Path(sys.argv[2]).resolve();mode=sys.argv[3]
assert mode in ('official','native');out.mkdir(parents=True,exist_ok=False)
repo=Path(__file__).resolve().parents[2];app=out/'app';app.mkdir()
identity=subprocess.check_output([sys.executable,str(repo/'scripts/svelte/source-identity.py')])
(out/'source.json').write_bytes(identity)
binaries={str(p.relative_to(repo)):hashlib.sha256(p.read_bytes()).hexdigest() for p in [repo/'target/release/svelte-tool',repo/'target/release/ts_svelte_kit'] if p.is_file()}
(out/'binaries.json').write_text(json.dumps(binaries,indent=2)+'\n')
inputs={};controls=[]
for name in ('src','static','package.json','package-lock.json','svelte.config.js','vite.config.ts','tsconfig.json','postcss.config.js','tailwind.config.js','CHANGELOG.md'):
 path=source/name
 if not path.exists():continue
 if path.is_dir():shutil.copytree(path,app/name,symlinks=True)
 else:shutil.copy2(path,app/name)
for p in sorted(app.rglob('*')):
 if p.is_symlink():raise RuntimeError(f'Input symlink needs explicit handling: {p}')
 if p.is_file():inputs[str(p.relative_to(app))]=hashlib.sha256(p.read_bytes()).hexdigest()
config=app/'vite.config.ts';text=config.read_text()
if 'autoUploadSourceMaps: true' in text:
 text=text.replace('autoUploadSourceMaps: true','autoUploadSourceMaps: false');controls.append('Disable Sentry sourcemap upload in isolated config')
config.write_text(text)
subprocess.run(['cp','-a','--reflink=auto',str(Path(os.environ.get('TS_SVELTE_BUILD_DEPS',str(source/'node_modules')))),str(app/'node_modules')],check=True)
(out/'inputs.json').write_text(json.dumps(inputs,indent=2)+'\n')
env={k:os.environ[k] for k in ('PATH','NIX_LD','NIX_LD_LIBRARY_PATH','LD_LIBRARY_PATH') if k in os.environ}
env.update({'NODE_ENV':'production','APP_VERSION':'prototype-build','ENABLE_REMINDER_SCHEDULER':'false','SENTRY_DISABLE_AUTO_UPLOAD':'true','DO_NOT_TRACK':'1','CI':'1'})
if mode=='native':env.update({'NODE_OPTIONS':f'--import={repo}/scripts/svelte/register-build-loader.mjs','TS_SVELTE_BINARY':str(repo/'target/release/svelte-tool'),'TS_SVELTE_VERSION':'5.53.11','TS_SVELTE_BUILD_TRACE':str(out/'native-calls.jsonl'),'TS_SVELTE_REQUEST_DIR':str(out/'requests')})
command=[shutil.which('node'),str(app/'node_modules/vite/bin/vite.js'),'build','--configLoader','runner']
if mode=='native':command=[str(repo/'target/release/ts_svelte_kit'),str(app),shutil.which('node'),str(repo/'scripts/svelte/register-build-loader.mjs'),str(repo/'target/release/svelte-tool'),str(out/'native-calls.jsonl')]
start=time.monotonic()
def run_step(command, log_name):
 with (out/log_name).open('w') as log:
  child=subprocess.Popen(command,cwd=app,env=env,stdout=log,stderr=subprocess.STDOUT,start_new_session=True)
  try:return child.wait(timeout=600)
  except subprocess.TimeoutExpired:
   os.killpg(child.pid,signal.SIGKILL);child.wait();return 124
sync_status=run_step([shutil.which('node'),str(app/'node_modules/@sveltejs/kit/svelte-kit.js'),'sync'],'sync.log')
status=run_step(command,'build.log') if sync_status==0 else sync_status
identity_after=subprocess.check_output([sys.executable,str(repo/'scripts/svelte/source-identity.py')])
(out/'source-after.json').write_bytes(identity_after)
binaries_stable=all(hashlib.sha256((repo/path).read_bytes()).hexdigest()==digest for path,digest in binaries.items())
unchanged=all(hashlib.sha256((source/name).read_bytes()).hexdigest()==digest for name,digest in inputs.items())
result={'mode':mode,'command':command,'exitCode':status,'sourceStable':identity==identity_after,'binariesStable':binaries_stable,'elapsedSeconds':time.monotonic()-start,'sourceUnchanged':unchanged,'controls':controls+['No .env files or inherited credentials','Reminder scheduler disabled'],'buildExists':(app/'build/index.js').is_file(),'nativeScope':'Svelte component and rune-module compilation only; Kit/Vite/preprocessing/bundling/adapter remain JavaScript' if mode=='native' else None}
(out/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result));sys.exit(status or (0 if unchanged and identity==identity_after and binaries_stable else 3))
