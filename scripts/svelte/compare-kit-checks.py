#!/usr/bin/env python3
"""Usage: compare-kit-checks.py APP NATIVE_RUN OFFICIAL_RUN NEW_OUTPUT
Compare complete diagnostics including message, severity and UTF-16 range.
"""
import argparse
import json
import hashlib
import re
from pathlib import Path
from urllib.parse import unquote, urlparse

p = argparse.ArgumentParser(description=__doc__)
for name in ['app', 'native', 'official', 'output']:
    p.add_argument(name, type=Path)
a = p.parse_args()
a.output.mkdir()
response = json.loads((a.native/'response.json').read_text())
summary = json.loads((a.native/'summary.json').read_text())
assert summary['complete'] and all(summary[key] for key in ['sourceStable', 'binariesStable', 'inputsStable'])
official_report = json.loads((a.official/'report.json').read_text())
assert response['ok'] and response['checker']['projectionsVerified']
assert summary['status'] in [0, 1] and isinstance(response['hasErrors'], bool)
assert summary['status'] == int(response['hasErrors'])
assert official_report['status'] in [0, 1] and official_report['inputsUnchanged']
native = []
for d in response['compilerDiagnostics']:
    path = Path(unquote(urlparse(d['uri']).path)) if d['uri'].startswith('file:') else a.app/d['uri']
    native.append({'file': str(path.resolve().relative_to(a.app.resolve())), 'code': d['code'], 'severity': d['severity'], 'message': d['message'], 'range': d['range']})
# Do not silently compare a warning-only subset when checker errors exist.
assert not response['checker']['diagnostics'], 'TS diagnostics require explicit comparison support'
official = []
lines = (a.official/'stdout.log').read_text().splitlines()
completion = re.fullmatch(r'\d+ COMPLETED (\d+) FILES (\d+) ERRORS (\d+) WARNINGS (\d+) FILES_WITH_PROBLEMS', lines[-1])
assert completion and int(completion[1]) > 0, 'Official checker did not complete'
for line in lines:
    if ' {' not in line:
        continue
    d = json.loads(line.split(' ', 1)[1])
    if d['type'] not in ['ERROR', 'WARNING']:
        continue
    official.append({'file': d['filename'], 'code': d['code'], 'severity': d['type'].lower(), 'message': d['message'], 'range': {key: d[key] for key in ['start', 'end']}})
key = lambda item: json.dumps(item, sort_keys=True)
assert sum(d['severity'] == 'error' for d in official) == int(completion[2])
assert sum(d['severity'] == 'warning' for d in official) == int(completion[3])
native.sort(key=key)
official.sort(key=key)
report = {'match': native == official, 'nativeCount': len(native), 'officialCount': len(official), 'native': native, 'official': official, 'suppressedDiagnostics': response['checker'].get('suppressedDiagnostics', [])}
report['evidenceHashes'] = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in [Path(__file__), a.native/'source.json', a.native/'summary.json', a.native/'graph.json', a.native/'graph-after.json', a.native/'binaries.json', a.native/'response.json', a.official/'inputs.json', a.official/'packages.json', a.official/'oracle-code.json', a.official/'stdout.log', a.official/'report.json']}
(a.output/'report.json').write_text(json.dumps(report, indent=2)+'\n')
print(json.dumps({k: report[k] for k in ['match', 'nativeCount', 'officialCount']}))
raise SystemExit(0 if report['match'] else 1)
