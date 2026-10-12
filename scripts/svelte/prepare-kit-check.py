#!/usr/bin/env python3
"""Usage: prepare-kit-check.py ORIGINAL_APP SYNCED_BUILD_APP NEW_OUTPUT
Prepare a checker copy with original source/test/config inputs and existing synced types.
No env files are copied and no original application configuration is executed.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess

parser=argparse.ArgumentParser(description=__doc__)
for name in ['original','synced','output']:parser.add_argument(name,type=Path)
a=parser.parse_args();out=a.output.resolve();out.mkdir();app=out/'app'
subprocess.run(['cp','--reflink=auto','-a',str(a.synced),str(app)],check=True)
manifest={}
def copy(path):
    name=path.relative_to(a.original);data=path.read_bytes();manifest[str(name)]=hashlib.sha256(data).hexdigest()
    dest=app/name;dest.parent.mkdir(parents=True,exist_ok=True);dest.write_bytes(data)
for directory in ['src','test','tests','scripts']:
    root=a.original/directory
    if root.exists():
        for p in sorted(root.rglob('*')):
            if p.is_file() and not p.is_symlink() and not p.name.startswith('.env'):copy(p)
for p in a.original.iterdir():
    if p.is_file() and (p.suffix in ['.ts','.js','.mjs','.cjs'] or p.name in ['tsconfig.json','package.json','package-lock.json']):copy(p)
assert all(hashlib.sha256((a.original/name).read_bytes()).hexdigest()==digest for name,digest in manifest.items())
(out/'original-inputs.json').write_text(json.dumps(manifest,indent=2)+'\n')
(out/'provenance.json').write_text(json.dumps({'original':str(a.original.resolve()),'syncedTypesFrom':str(a.synced.resolve()),'scope':'Existing isolated Kit sync snapshot; original source/tests/config copied without executing config; environment types reflect isolated build environment, not production env'},indent=2)+'\n')
print(json.dumps({'app':str(app),'originalFiles':len(manifest)}))
