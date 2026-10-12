#!/usr/bin/env python3
"""Usage: kit-check-oracle.py ISOLATED_APP NEW_OUTPUT
Run pinned official svelte-check with machine JSON lines in a clean environment.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import time

p=argparse.ArgumentParser(description=__doc__)
p.add_argument('app',type=Path);p.add_argument('output',type=Path);a=p.parse_args()
app=a.app.resolve();out=a.output.resolve();out.mkdir()
def identity():
    files={}
    for directory in ['src','test','tests','scripts','.svelte-kit/types']:
        for f in sorted((app/directory).rglob('*')):
            if f.is_file() and not f.is_symlink():files[str(f.relative_to(app))]=hashlib.sha256(f.read_bytes()).hexdigest()
    for name in ['tsconfig.json','svelte.config.js','package.json','package-lock.json','.svelte-kit/tsconfig.json','.svelte-kit/ambient.d.ts','.svelte-kit/non-ambient.d.ts']:
        f=app/name
        if f.is_file():files[name]=hashlib.sha256(f.read_bytes()).hexdigest()
    for f in sorted(app.iterdir()):
        if f.is_file() and f.suffix in ['.ts','.js','.mts','.cts','.mjs','.cjs','.json']:files[f.name]=hashlib.sha256(f.read_bytes()).hexdigest()
    for f in sorted((app/'.svelte-kit').glob('*.ts')):files[str(f.relative_to(app))]=hashlib.sha256(f.read_bytes()).hexdigest()
    return files
oracle_code={name:hashlib.sha256((app/'node_modules'/name).read_bytes()).hexdigest() for name in ['typescript/lib/typescript.js','svelte-check/dist/src/index.js','svelte/compiler/index.js']}
(out/'oracle-code.json').write_text(json.dumps(oracle_code,indent=2)+'\n')
packages={name:hashlib.sha256((app/'node_modules'/name/'package.json').read_bytes()).hexdigest() for name in ['svelte','@sveltejs/kit','typescript','svelte-check']}
(out/'packages.json').write_text(json.dumps(packages,indent=2)+'\n')
before=identity();(out/'inputs.json').write_text(json.dumps(before,indent=2)+'\n')
env={'PATH':os.environ['PATH'],'NODE_ENV':'development','DO_NOT_TRACK':'1','ENABLE_REMINDER_SCHEDULER':'false','SENTRY_DISABLE_AUTO_UPLOAD':'true'}
command=['node','node_modules/svelte-check/bin/svelte-check','--tsconfig','./tsconfig.json','--output','machine-verbose']
started=time.monotonic()
with (out/'stdout.log').open('wb') as stdout,(out/'stderr.log').open('wb') as stderr:
    child=subprocess.Popen(command,cwd=app,env=env,stdout=stdout,stderr=stderr,start_new_session=True)
    try:status=child.wait(timeout=300)
    except subprocess.TimeoutExpired:
        os.killpg(child.pid,signal.SIGKILL);child.wait();status='timeout'
report={'command':command,'status':status,'elapsedSeconds':time.monotonic()-started,'inputsUnchanged':before==identity()}
(out/'report.json').write_text(json.dumps(report,indent=2)+'\n');print(json.dumps(report))
