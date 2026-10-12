#!/usr/bin/env python3
"""Usage: editor-features.py DEPENDENCIES OUTPUT
Focused native Svelte editor correctness tests; not a benchmark/application audit.
"""
import json
import pathlib
import select
import subprocess
import sys
import time

if len(sys.argv) != 3 or sys.argv[1] in ('help', '--help'):
    print(__doc__)
    raise SystemExit(0 if len(sys.argv) > 1 and sys.argv[1] in ('help', '--help') else 2)
deps = pathlib.Path(sys.argv[1]).resolve()
out = pathlib.Path(sys.argv[2]).resolve()
out.mkdir(parents=True, exist_ok=False)
root = out / 'project'
root.mkdir()
source = '<script lang="ts">\nlet count: number = 1;\nconst user = { name: "Ada" };\n</script>\n<p>{count} {user.name}</p>\n'
file = root / 'App.svelte'
file.write_text(source)
uri = file.as_uri()
config = {'root': str(root), 'dependencies': str(deps), 'components': [{'path': 'App.svelte', 'typescript': True}], 'files': [], 'runes': False}
config_file = out / 'config.json'
config_file.write_text(json.dumps(config))
log = (out / 'stderr.log').open('w')
proc = subprocess.Popen(['target/release/svelte-lsp', '--project-config', str(config_file)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=log, bufsize=0)
messages = []
buffer = bytearray()
next_id = 0

def send(method, params, request=True):
    global next_id
    next_id += 1
    message = {'jsonrpc': '2.0', 'method': method, 'params': params}
    if request:
        message['id'] = next_id
    payload = json.dumps(message).encode()
    proc.stdin.write(f'Content-Length: {len(payload)}\r\n\r\n'.encode() + payload)
    return next_id

def receive(predicate):
    deadline = time.monotonic() + 60
    while time.monotonic() < deadline:
        marker = buffer.find(b'\r\n\r\n')
        if marker >= 0:
            headers = bytes(buffer[:marker]).decode()
            size = int(next(h.split(':', 1)[1] for h in headers.split('\r\n') if h.lower().startswith('content-length:')))
            if len(buffer) >= marker + 4 + size:
                data = json.loads(buffer[marker + 4:marker + 4 + size])
                del buffer[:marker + 4 + size]
                messages.append(data)
                if predicate(data):
                    return data
                continue
        ready, _, _ = select.select([proc.stdout], [], [], max(0, deadline - time.monotonic()))
        if not ready:
            break
        data = proc.stdout.read(65536)
        if not data:
            raise RuntimeError(f'LSP exited: {proc.poll()}')
        buffer.extend(data)
    raise TimeoutError('LSP response timeout')

def request(method, params):
    id_ = send(method, params)
    response = receive(lambda m: m.get('id') == id_)
    assert 'error' not in response, response
    return response['result']

def position(token, occurrence=0):
    offset = -1
    for _ in range(occurrence + 1):
        offset = source.index(token, offset + 1)
    before = source[:offset]
    return {'line': before.count('\n'), 'character': len(before.rsplit('\n', 1)[-1].encode('utf-16-le')) // 2}

def query(method, token, occurrence=0, **extra):
    return request(method, {'textDocument': {'uri': uri}, 'position': position(token, occurrence), **extra})

passed = []
try:
    init = request('initialize', {})
    assert init['capabilities']['hoverProvider'] is True
    send('initialized', {}, False)
    send('textDocument/didOpen', {'textDocument': {'uri': uri, 'version': 1, 'languageId': 'svelte', 'text': source}}, False)
    opened = receive(lambda m: m.get('method') == 'textDocument/publishDiagnostics' and m['params']['uri'] == uri)
    assert not [d for d in opened['params']['diagnostics'] if d.get('severity') == 1], opened
    passed.append('open-project-diagnostics')
    hover = query('textDocument/hover', 'count', 1)
    assert 'number' in str(hover) and hover['range']['start'] == position('count', 1), hover
    passed.append('template-hover-mapped')
    definitions = query('textDocument/definition', 'count', 1)
    assert definitions and uri in str(definitions) and 'svelte.tsx' not in str(definitions), definitions
    passed.append('template-definition-original')
    references = query('textDocument/references', 'count', 1, context={'includeDeclaration': True})
    assert len(references) >= 2 and all(r['uri'] == uri for r in references), references
    passed.append('template-references-original')
    prepared = query('textDocument/prepareRename', 'count', 1)
    assert prepared['range']['start'] == position('count', 1), prepared
    rename = query('textDocument/rename', 'count', 1, newName='total')
    assert 'total' in str(rename) and 'svelte.tsx' not in str(rename), rename
    passed.append('template-rename-atomic')
    completions = query('textDocument/completion', 'name}', context={'triggerKind': 1})
    items = completions if isinstance(completions, list) else completions['items']
    item = next(i for i in items if i['label'] == 'name')
    resolved = request('completionItem/resolve', item)
    assert resolved['label'] == 'name'
    passed.append('member-completion-and-resolve')
    broken = source.replace('number = 1', 'number = "bad"')
    send('textDocument/didChange', {'textDocument': {'uri': uri, 'version': 2}, 'contentChanges': [{'text': broken}]}, False)
    changed = receive(lambda m: m.get('method') == 'textDocument/publishDiagnostics' and m['params']['uri'] == uri)
    assert any(str(d.get('code')) == '2322' for d in changed['params']['diagnostics']), changed
    passed.append('persistent-type-error')
    send('textDocument/didChange', {'textDocument': {'uri': uri, 'version': 3}, 'contentChanges': [{'text': source}]}, False)
    repaired = receive(lambda m: m.get('method') == 'textDocument/publishDiagnostics' and m['params']['uri'] == uri)
    assert not [d for d in repaired['params']['diagnostics'] if d.get('severity') == 1], repaired
    passed.append('persistent-repair')
    request('shutdown', {})
    send('exit', {}, False)
    assert proc.wait(timeout=5) == 0
    (out / 'report.json').write_text(json.dumps({'passed': passed, 'complete': True}, indent=2))
    print(json.dumps({'passed': passed}))
finally:
    (out / 'messages.json').write_text(json.dumps(messages, indent=2))
    if proc.poll() is None:
        proc.kill()
        proc.wait()
    log.close()
