#!/usr/bin/env python3
"""Usage: bench-build.py SOURCE_APP OUT_DIR [--runs N] [--warmups N]
Time the official `vite build` (package.json "build") against the native
`svelte-build` project op on identical fresh copies of SOURCE_APP, on this
machine, alternating official and native runs after discarded warmups.

Both copies get the same sources, the same installed node_modules and the same
build environment (application-build.py controls: no .env files or inherited
credentials, Sentry upload disabled, reminder scheduler disabled). Every
official run starts without .svelte-kit/ and build/; every native run writes a
new output directory. Each run is measured with GNU time (-v): wall clock,
user+system CPU of the process tree, and the largest single-process peak RSS.

Writes OUT_DIR/runs.jsonl (one line per run), OUT_DIR/summary.json and the
logs, then prints the summary. The last log line is DONE or FAIL rc=<N>.
"""
import json,os,platform,shutil,statistics,subprocess,sys,time
from pathlib import Path
args=sys.argv[1:]
if len(args)<2 or args[0] in ('help','--help'):
 print(__doc__);sys.exit(0 if args and args[0] in ('help','--help') else 2)
source=Path(args[0]).resolve();out=Path(args[1]).resolve()
def option(name,default):
 return int(args[args.index(name)+1]) if name in args else default
runs=option('--runs',5);warmups=option('--warmups',1)
repo=Path(__file__).resolve().parents[2]
native_bin=repo/'target/release/svelte-build';time_bin=shutil.which('time',path='/run/current-system/sw/bin:/usr/bin')
assert time_bin,'GNU time is required'
out.mkdir(parents=True,exist_ok=False)
log=(out/'bench.log').open('a')
def note(text):
 print(text,flush=True);log.write(text+'\n');log.flush()
def copy_app(target):
 target.mkdir()
 for name in ('src','static','package.json','package-lock.json','svelte.config.js','vite.config.ts','tsconfig.json','postcss.config.js','tailwind.config.js','CHANGELOG.md'):
  path=source/name
  if not path.exists():continue
  if path.is_dir():shutil.copytree(path,target/name,symlinks=True)
  else:shutil.copy2(path,target/name)
 config=target/'vite.config.ts';text=config.read_text()
 config.write_text(text.replace('autoUploadSourceMaps: true','autoUploadSourceMaps: false'))
 subprocess.run(['cp','-a',str(source/'node_modules'),str(target/'node_modules')],check=True)
env={k:os.environ[k] for k in ('PATH','NIX_LD','NIX_LD_LIBRARY_PATH','LD_LIBRARY_PATH','HOME') if k in os.environ}
build_env={'NODE_ENV':'production','APP_VERSION':'prototype-build','ENABLE_REMINDER_SCHEDULER':'false','SENTRY_DISABLE_AUTO_UPLOAD':'true','DO_NOT_TRACK':'1','CI':'1'}
env.update(build_env)
official_app=out/'official-app';native_app=out/'native-app'
note(f'Copying {source} twice');copy_app(official_app);copy_app(native_app)
node=shutil.which('node');package=json.loads((official_app/'package.json').read_text())
assert package['scripts']['build']=='vite build',package['scripts']['build']
def gnu_time(path):
 fields={}
 for line in path.read_text().splitlines():
  key,_,value=line.strip().rpartition(': ')
  fields[key]=value
 clock=[float(x) for x in fields['Elapsed (wall clock) time (h:mm:ss or m:ss)'].split(':')]
 wall=sum(v*60**i for i,v in enumerate(reversed(clock)))
 return {'wallSeconds':wall,'userSeconds':float(fields['User time (seconds)']),'systemSeconds':float(fields['System time (seconds)']),'maxRssKiB':int(fields['Maximum resident set size (kbytes)'])}
def tree_bytes(path):
 return sum(p.stat().st_size for p in path.rglob('*') if p.is_file())
def run(kind,index):
 name=f'{kind}-{index}';timing=out/f'{name}.time'
 load=os.getloadavg()[0]
 if kind=='official':
  for stale in ('.svelte-kit','build'):shutil.rmtree(official_app/stale,ignore_errors=True)
  command=[node,str(official_app/'node_modules/vite/bin/vite.js'),'build'];cwd=official_app;stdin=None
 else:
  target=out/f'native-out-{index}'
  request={'op':'project','project':{'root':str(native_app),'environment':build_env,'versionName':'bench','define':{'__APP_VERSION__':json.dumps(build_env['APP_VERSION'])}},'outputDirectory':str(target),'acceptRequirements':['vite/sentry','vite/esbuild']}
  (out/f'{name}.request.json').write_text(json.dumps(request))
  command=[str(native_bin)];cwd=native_app;stdin=(out/f'{name}.request.json').open('rb')
 with (out/f'{name}.log').open('wb') as output:
  status=subprocess.run([time_bin,'-v','-o',str(timing)]+command,cwd=cwd,env=env,stdin=stdin,stdout=output,stderr=subprocess.STDOUT).returncode
 if status!=0:
  note(f'{name} failed with {status}; see {name}.log');return status,None
 result={'kind':kind,'index':index,'loadBefore':load,**gnu_time(timing)}
 built=official_app/'build' if kind=='official' else out/f'native-out-{index}/build'
 result['buildBytes']=tree_bytes(built)
 if kind=='native' and index!=runs:shutil.rmtree(out/f'native-out-{index}')
 if kind=='native':
  response=json.loads((out/f'{name}.log').read_text().splitlines()[-1])
  if response.get('ok')!=True:note(f'{name} response not ok');return 1,None
 note(json.dumps(result))
 with (out/'runs.jsonl').open('a') as lines:lines.write(json.dumps(result)+'\n')
 return 0,result
results={'official':[],'native':[]}
order=[('official',f'w{i}') for i in range(1,warmups+1)]+[('native',f'w{i}') for i in range(1,warmups+1)]
for i in range(1,runs+1):order+=[('official',i),('native',i)]
for kind,index in order:
 status,result=run(kind,index)
 if status:
  note(f'FAIL rc={status}');sys.exit(status)
 if not str(index).startswith('w'):results[kind].append(result)
def stats(rows,key):
 values=[row[key] for row in rows]
 return {'median':statistics.median(values),'min':min(values),'max':max(values),'values':values}
summary={'machine':{'host':platform.node(),'cpu':next((l.split(':',1)[1].strip() for l in open('/proc/cpuinfo') if l.startswith('model name')),None),'logicalCpus':os.cpu_count(),'memoryKiB':int(next(l.split()[1] for l in open('/proc/meminfo') if l.startswith('MemTotal'))),'governor':Path('/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor').read_text().strip() if Path('/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor').exists() else None,'node':subprocess.check_output([node,'--version'],text=True).strip()},
 'source':str(source),'runs':runs,'warmupsDiscarded':warmups,'order':'warmups, then official/native alternating',
 'nativeBinarySha256':subprocess.check_output(['sha256sum',str(native_bin)],text=True).split()[0],
 'gitHead':subprocess.check_output(['git','-C',str(repo),'rev-parse','HEAD'],text=True).strip(),
 'gitDirty':bool(subprocess.check_output(['git','-C',str(repo),'status','--porcelain','--untracked-files=no'],text=True).strip())}
for kind in results:
 rows=results[kind]
 summary[kind]={key:stats(rows,key) for key in ('wallSeconds','userSeconds','systemSeconds','maxRssKiB','buildBytes')}
 summary[kind]['cpuSeconds']=stats([{'v':r['userSeconds']+r['systemSeconds']} for r in rows],'v')
(out/'summary.json').write_text(json.dumps(summary,indent=1)+'\n')
for kind in results:
 s=summary[kind];note(f"{kind}: wall median {s['wallSeconds']['median']:.1f}s (min {s['wallSeconds']['min']:.1f}, max {s['wallSeconds']['max']:.1f}), CPU median {s['cpuSeconds']['median']:.1f}s, peak RSS median {s['maxRssKiB']['median']/1024:.0f} MiB")
note('DONE')
