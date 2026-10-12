#!/usr/bin/env python3
"""Usage: kit-project-probe.py APP NEW_OUTPUT
Discover a Kit sync snapshot and run the native checker; never modify APP.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time
import os
import signal

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('app',type=Path)
parser.add_argument('output',type=Path)
args=parser.parse_args()
out=args.output.resolve();out.mkdir()
app=args.app.resolve()
bins=Path('target/release').resolve()
identity=subprocess.check_output(['python3','scripts/svelte/source-identity.py'])
(out/'source.json').write_bytes(identity)
hashes={str(bins/n):hashlib.sha256((bins/n).read_bytes()).hexdigest() for n in ['svelte-kit-project','svelte-project','svelte-ts-check','svelte-tool','svelte-ts-transpile']}
(out/'binaries.json').write_text(json.dumps(hashes,indent=2)+'\n')
graph=subprocess.run([str(bins/'svelte-kit-project'),str(app)],capture_output=True)
(out/'graph.json').write_bytes(graph.stdout)
if graph.returncode:raise RuntimeError('Discovery failed: '+graph.stdout.decode()[:500])
with (out/'request.json').open('wb') as output:
    request=subprocess.run([str(bins/'svelte-kit-project'),'request',str(app),str(out/'workspace'),'runes'],stdout=output,stderr=subprocess.PIPE)
if request.returncode:raise RuntimeError('Request generation failed')
started=time.monotonic()
with (out/'request.json').open('rb') as source,(out/'response.json').open('wb') as output,(out/'stderr.log').open('wb') as stderr:
    try:
        child=subprocess.Popen([str(bins/'svelte-project')],stdin=source,stdout=output,stderr=stderr,start_new_session=True)
        status=child.wait(timeout=300)
    except subprocess.TimeoutExpired:
        os.killpg(child.pid,signal.SIGKILL);child.wait();status='timeout'
elapsed=time.monotonic()-started
graph_after=subprocess.run([str(bins/'svelte-kit-project'),str(app)],capture_output=True)
(out/'graph-after.json').write_bytes(graph_after.stdout)
inputs_stable=graph_after.returncode==0 and json.loads(graph.stdout)==json.loads(graph_after.stdout)
response=json.loads((out/'response.json').read_text() or '{}')
after=subprocess.check_output(['python3','scripts/svelte/source-identity.py']);(out/'source-after.json').write_bytes(after)
summary={'status':status,'complete':response.get('ok') is True,'stage':response.get('stage'),'error':response.get('error',response.get('failure')),'checkerStatus':response.get('checker',{}).get('status'),'diagnostics':len(response.get('checker',{}).get('diagnostics',[])),'requestBytes':(out/'request.json').stat().st_size,'elapsedSeconds':elapsed,'inputsStable':inputs_stable,'sourceStable':identity==after,'binariesStable':all(hashlib.sha256(Path(p).read_bytes()).hexdigest()==h for p,h in hashes.items())}
(out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n');print(json.dumps(summary,indent=2))
