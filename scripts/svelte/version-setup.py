#!/usr/bin/env python3
"""Usage: version-setup.py NEW_EVIDENCE_DIR
Install exact Svelte/Kit oracle versions into target/svelte-versions. Never
modify application inputs. Each install and resolved lockfile is preserved.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
SVELTE = ['5.51.5', '5.52.0', '5.53.5', '5.53.11', '5.53.13', '5.54.1', '5.55.10', '5.56.10', '5.57.2']
KIT = ['2.67.0', '2.68.0', '2.69.2', '2.69.3', '2.70.3', '3.0.1', '3.0.0-next.32']


def main(out):
    out.mkdir(parents=True)
    root = ROOT / 'target/svelte-versions'
    root.mkdir(exist_ok=True)
    matrix = []
    for kind, versions in [('svelte', SVELTE), ('kit', KIT)]:
        for version in versions:
            label = kind + '-' + version
            directory = root / label
            directory.mkdir()
            deps = {'svelte': version if kind == 'svelte' else ('5.57.2' if version.startswith('3.') else '5.53.11'),
                    'svelte-check': '4.7.6', 'typescript': '6.0.3' if kind == 'kit' and version.startswith('3.') else '5.9.3',
                    'jsdom': '26.1.0'}
            if kind == 'kit':
                deps.update({'@sveltejs/kit': version, 'vite': '8.0.12' if version.startswith('3.') else '7.3.1',
                             '@sveltejs/vite-plugin-svelte': '7.0.0' if version.startswith('3.') else '6.2.4',
                             '@opentelemetry/api': '1.9.0'})
            (directory / 'package.json').write_text(json.dumps({'private':True,'type':'module','dependencies':deps}, indent=2)+'\n')
            with (out / (label + '.log')).open('w') as log:
                p = subprocess.run(['npm','install','--ignore-scripts','--no-audit','--no-fund'],cwd=directory,stdout=log,stderr=subprocess.STDOUT,timeout=180)
            item = {'kind':kind,'version':version,'directory':str(directory),'dependencies':deps,'exitCode':p.returncode}
            lock = directory / 'package-lock.json'
            if lock.exists():
                item['lockSha256'] = hashlib.sha256(lock.read_bytes()).hexdigest()
            matrix.append(item)
            (out / 'matrix.json').write_text(json.dumps(matrix,indent=2)+'\n')
            print(label,p.returncode,flush=True)
    print('DONE' if all(x['exitCode']==0 for x in matrix) else 'FAIL rc=1',flush=True)


if __name__ == '__main__':
    if len(sys.argv)!=2 or sys.argv[1] in ('help','--help'):
        print(__doc__)
    else:
        main(Path(sys.argv[1]).resolve())
