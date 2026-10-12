#!/usr/bin/env python3
"""Usage: projection-inventory.py APP SAVED_COMPILER NEW_OUTPUT
Measure every .svelte projection using a frozen native compiler; app is read-only.
"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import time

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('app',type=Path)
parser.add_argument('compiler',type=Path)
parser.add_argument('output',type=Path)
args=parser.parse_args()
args.output.mkdir()
binary=args.compiler.resolve()
binary_hash=hashlib.sha256(binary.read_bytes()).hexdigest()
child=subprocess.Popen([str(binary),'--serve'],stdin=subprocess.PIPE,stdout=subprocess.PIPE)
cases=[]
started=time.monotonic()
try:
    for path in sorted((args.app/'src').rglob('*.svelte')):
        assert path.stat().st_size<=1024*1024
        text=path.read_text()
        request={'op':'project','typescript':True,'runes':True,'compatibility':{'svelteVersion':'5.53.11'},'document':{'uri':path.resolve().as_uri(),'version':0,'text':text}}
        child.stdin.write((json.dumps(request)+'\n').encode());child.stdin.flush()
        response=json.loads(child.stdout.readline())
        name=str(path.relative_to(args.app));case={'path':name,'sourceHash':hashlib.sha256(text.encode()).hexdigest(),'ok':response.get('ok')}
        if response.get('ok'):
            case['projectedBytes']=len(response['result']['code'].encode())
        else:case['failure']=response
        cases.append(case)
        (args.output/'report.json').write_text(json.dumps({'scope':'TypeScript-mode native projections of all src .svelte files; not semantic checking','compiler':str(binary),'binarySha256':binary_hash,'cases':cases},indent=2)+'\n')
finally:
    child.stdin.close();child.wait(timeout=10)
assert hashlib.sha256(binary.read_bytes()).hexdigest()==binary_hash
print(json.dumps({'cases':len(cases),'passed':sum(c['ok'] is True for c in cases),'failed':sum(c['ok'] is not True for c in cases),'elapsedSeconds':time.monotonic()-started}))
