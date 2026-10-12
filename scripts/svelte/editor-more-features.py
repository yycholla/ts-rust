#!/usr/bin/env python3
"""Usage: editor-more-features.py DEPENDENCIES OUTPUT
Focused additional editor and rune-module correctness cases. No timing comparison.
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
source = ('<script lang="ts">\n'
          'function greet(name: string, suffix: string) {\n'
          '  return name + suffix;\n'
          '}\n'
          'const message = greet("Ada", "!");\n'
          'const person = { name: "Ada" };\n'
          '</script>\n'
          '<p>{message} {person.name}</p>\n')
file = root / 'App.svelte'
file.write_text(source)
uri = file.as_uri()
ordinary = 'import { zebra, alpha } from "./values";\nconsole.log(zebra, alpha);\n'
jsx = 'declare global { namespace JSX { interface IntrinsicElements { section: {}; } } }\nexport const view = <section>hello</section>;\n'
module = 'export class Counter { constructor(public value: number) {} }\nexport function answer() { return 42; }\n'
(root / 'values.ts').write_text('export const alpha = 1; export const zebra = 2;\n')
(root / 'ordinary.ts').write_text(ordinary)
(root / 'View.tsx').write_text(jsx)
(root / 'state.svelte.ts').write_text(module)
config = {'root': str(root), 'dependencies': str(deps), 'components': [{'path': 'App.svelte', 'typescript': True}],
          'files': ['values.ts', 'ordinary.ts', 'View.tsx', 'state.svelte.ts'], 'runes': False, 'runeModules': True}
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

def source_position(text, token, occurrence=0, delta=0):
    offset = -1
    for _ in range(occurrence + 1):
        offset = text.index(token, offset + 1)
    before = text[:offset + delta]
    return {'line': before.count('\n'), 'character': len(before.rsplit('\n', 1)[-1].encode('utf-16-le')) // 2}


def point_key(point):
    return point['line'], point['character']


def source_offset(text, point):
    lines = text.splitlines(keepends=True)
    line = point['line']
    assert 0 <= line <= len(lines), point
    if line == len(lines):
        assert point['character'] == 0, point
        return len(text)
    units = 0
    prefix = sum(len(item) for item in lines[:line])
    for offset, char in enumerate(lines[line]):
        if units == point['character']:
            return prefix + offset
        units += len(char.encode('utf-16-le')) // 2
    assert units == point['character'], point
    return prefix + len(lines[line])


def range_text(text, span):
    start, end = source_offset(text, span['start']), source_offset(text, span['end'])
    assert start <= end, span
    return text[start:end]


def symbols_flat(items):
    for item in items:
        yield item
        yield from symbols_flat(item.get('children', []))


def diagnostics_for(document_uri):
    return receive(lambda m: m.get('method') == 'textDocument/publishDiagnostics'
                   and m['params']['uri'] == document_uri)['params']['diagnostics']


def open_other(path, text, language):
    other_uri = (root / path).as_uri()
    send('textDocument/didOpen', {'textDocument': {'uri': other_uri, 'version': 1,
         'languageId': language, 'text': text}}, False)
    diagnostics = diagnostics_for(other_uri)
    assert not [d for d in diagnostics if d.get('severity') == 1], diagnostics
    return other_uri


def workspace_edits(action, expected_uri):
    edit = action.get('edit')
    assert edit, action
    edits = list(edit.get('changes', {}).get(expected_uri, []))
    assert all(key == expected_uri for key in edit.get('changes', {})), edit
    for change in edit.get('documentChanges', []):
        assert change.get('textDocument', {}).get('uri') == expected_uri, change
        edits.extend(change['edits'])
    assert edits, action
    return edits


passed = []
try:
    caps = {
        'textDocument': {
            'signatureHelp': {'signatureInformation': {'parameterInformation': {'labelOffsetSupport': True}}},
            'documentSymbol': {'hierarchicalDocumentSymbolSupport': True},
            'semanticTokens': {
                'requests': {'full': True, 'range': True}, 'formats': ['relative'],
                'tokenTypes': ['namespace', 'class', 'enum', 'interface', 'struct', 'typeParameter',
                               'type', 'parameter', 'variable', 'property', 'enumMember', 'decorator',
                               'event', 'function', 'method', 'macro', 'label', 'comment', 'string',
                               'keyword', 'number', 'regexp', 'operator'],
                'tokenModifiers': ['declaration', 'definition', 'readonly', 'static', 'deprecated',
                                   'abstract', 'async', 'modification', 'documentation', 'defaultLibrary', 'local']
            }
        }
    }
    init = request('initialize', {'capabilities': caps})
    send('initialized', {}, False)
    send('textDocument/didOpen', {'textDocument': {'uri': uri, 'version': 1,
         'languageId': 'svelte', 'text': source}}, False)
    assert not [d for d in diagnostics_for(uri) if d.get('severity') == 1]

    signature = query('textDocument/signatureHelp', '"!"', context={'triggerKind': 1, 'isRetrigger': False})
    assert signature and signature['signatures'], signature
    selected = signature['signatures'][signature.get('activeSignature', 0)]
    assert 'name: string' in selected['label'] and 'suffix: string' in selected['label'], signature
    assert selected.get('activeParameter', signature.get('activeParameter')) == 1, signature
    passed.append('svelte-signature-help-second-parameter')

    highlights = query('textDocument/documentHighlight', 'message', 1)
    assert highlights and len(highlights) >= 2, highlights
    assert all(range_text(source, item['range']) == 'message' for item in highlights), highlights
    assert {point_key(item['range']['start']) for item in highlights} >= {
        point_key(position('message', 0)), point_key(position('message', 1))}, highlights
    passed.append('svelte-highlights-source-ranges')

    symbols = request('textDocument/documentSymbol', {'textDocument': {'uri': uri}})
    found = list(symbols_flat(symbols or []))
    greet = next(item for item in found if item['name'] == 'greet')
    assert range_text(source, greet['selectionRange']) == 'greet', greet
    assert 'return name + suffix;' in range_text(source, greet['range']), greet
    assert any(item['name'] == 'message' for item in found), found
    passed.append('svelte-hierarchical-document-symbols')

    selected_ranges = request('textDocument/selectionRange', {
        'textDocument': {'uri': uri}, 'positions': [position('suffix;', 0)]})
    assert len(selected_ranges or []) == 1, selected_ranges
    node = selected_ranges[0]
    assert range_text(source, node['range']) == 'suffix', node
    depth = 0
    previous = node['range']
    while node.get('parent'):
        node = node['parent']
        assert point_key(node['range']['start']) <= point_key(previous['start']), node
        assert point_key(node['range']['end']) >= point_key(previous['end']), node
        range_text(source, node['range'])
        previous = node['range']
        depth += 1
        assert depth < 1000
    assert depth >= 1, selected_ranges
    passed.append('svelte-selection-parent-ranges')

    folds = request('textDocument/foldingRange', {'textDocument': {'uri': uri}})
    assert any(item['startLine'] == 1 and item['endLine'] == 3 for item in folds or []), folds
    passed.append('svelte-function-folding-range')

    token_provider = init['capabilities']['semanticTokensProvider']
    legend = token_provider['legend']
    tokens = request('textDocument/semanticTokens/full', {'textDocument': {'uri': uri}})
    assert tokens and tokens['data'] and len(tokens['data']) % 5 == 0, tokens
    decoded = []
    line, column = 0, 0
    for index in range(0, len(tokens['data']), 5):
        delta_line, delta_column, length, kind, modifiers = tokens['data'][index:index + 5]
        line += delta_line
        column = delta_column if delta_line else column + delta_column
        assert 0 <= kind < len(legend['tokenTypes']) and length > 0, tokens
        span = {'start': {'line': line, 'character': column},
                'end': {'line': line, 'character': column + length}}
        decoded.append((range_text(source, span), legend['tokenTypes'][kind], span))
    assert any(text == 'greet' and kind == 'function' for text, kind, _ in decoded), decoded
    assert any(text == 'message' for text, _, _ in decoded), decoded
    passed.append('svelte-semantic-token-decoding-and-mapping')

    send('workspace/didChangeConfiguration', {'settings': {
        'includeInlayParameterNameHints': 'all', 'includeInlayVariableTypeHints': True,
        'includeInlayFunctionLikeReturnTypeHints': True}}, False)
    hints = request('textDocument/inlayHint', {'textDocument': {'uri': uri}, 'range': {
        'start': {'line': 1, 'character': 0}, 'end': {'line': 6, 'character': 0}}})
    assert hints, hints
    label_text = lambda item: item['label'] if isinstance(item['label'], str) else ''.join(part['value'] for part in item['label'])
    assert any('name:' in label_text(item) and item['position'] == position('"Ada"', 0)
               for item in hints), hints
    assert any('suffix:' in label_text(item) and item['position'] == position('"!"')
               for item in hints), hints
    passed.append('svelte-inlay-parameter-hints')

    ordinary_uri = open_other('ordinary.ts', ordinary, 'typescript')
    actions = request('textDocument/codeAction', {'textDocument': {'uri': ordinary_uri},
        'range': {'start': {'line': 0, 'character': 0}, 'end': {'line': 0, 'character': 0}},
        'context': {'diagnostics': [], 'only': ['source.organizeImports']}})
    action = next(item for item in actions or [] if item.get('kind') == 'source.organizeImports.ts')
    edits = workspace_edits(action, ordinary_uri)
    edited = ordinary
    spans = sorted([(source_offset(ordinary, edit['range']['start']),
                     source_offset(ordinary, edit['range']['end']), edit['newText'])
                    for edit in edits], reverse=True)
    previous = len(ordinary) + 1
    for start, end, replacement in spans:
        assert 0 <= start <= end <= previous, spans
        edited = edited[:start] + replacement + edited[end:]
        previous = start
    assert 'import { alpha, zebra }' in edited and 'console.log(zebra, alpha)' in edited, edited
    passed.append('organize-imports-applicable-workspace-edit')

    jsx_uri = open_other('View.tsx', jsx, 'typescriptreact')
    linked = request('textDocument/linkedEditingRange', {'textDocument': {'uri': jsx_uri},
        'position': source_position(jsx, '<section>', delta=2)})
    assert len(linked.get('ranges', [])) == 2, linked
    assert all(range_text(jsx, span) == 'section' for span in linked['ranges']), linked
    assert {point_key(span['start']) for span in linked['ranges']} == {
        point_key(source_position(jsx, '<section>', delta=1)),
        point_key(source_position(jsx, '</section>', delta=2))}, linked
    passed.append('tsx-linked-tag-editing')

    module_uri = open_other('state.svelte.ts', module, 'typescript')
    bad_module = module.replace('return 42', 'return $state(42)')
    send('textDocument/didChange', {'textDocument': {'uri': module_uri, 'version': 2},
         'contentChanges': [{'text': bad_module}]}, False)
    broken = diagnostics_for(module_uri)
    diagnostic = next(d for d in broken if d.get('code') == 'state_invalid_placement')
    assert diagnostic['severity'] == 1 and diagnostic['range']['start']['line'] == 1, diagnostic
    passed.append('runtime-typescript-rune-module-compiler-diagnostic')
    send('textDocument/didChange', {'textDocument': {'uri': module_uri, 'version': 3},
         'contentChanges': [{'text': module}]}, False)
    repaired = diagnostics_for(module_uri)
    assert not [d for d in repaired if d.get('severity') == 1], repaired
    passed.append('runtime-typescript-rune-module-repair')

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
