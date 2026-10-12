#!/usr/bin/env python3
"""Focused synthetic LSP correctness cases, without application trials or timing.

Usage: editor-native-features.py DEPENDENCIES NEW_OUTPUT [--skip-kit]

DEPENDENCIES is an installed node_modules directory containing Svelte, SvelteKit,
TypeScript and svelte-check. All fixtures and native Kit sync output stay under
NEW_OUTPUT. Uses target/release/svelte-lsp and target/release/svelte-kit-project.
No dependency/configuration JavaScript is executed. --skip-kit runs only the
standalone syntax and native lint action cases while the Kit binary is pending.
"""

import argparse
import json
import os
import pathlib
import select
import signal
import subprocess
import time
import traceback


REPO = pathlib.Path(__file__).resolve().parents[2]


def position(text, offset):
    before = text[:offset]
    return {"line": before.count("\n"),
            "character": len(before.rsplit("\n", 1)[-1].encode("utf-16-le")) // 2}


def offset(text, point):
    lines = text.splitlines(keepends=True)
    if text.endswith("\n") or not lines:
        lines.append("")
    assert 0 <= point["line"] < len(lines), point
    line = lines[point["line"]]
    content = line.removesuffix("\n").removesuffix("\r")
    units = 0
    base = sum(map(len, lines[:point["line"]]))
    for index, char in enumerate(content):
        if units == point["character"]:
            return base + index
        units += len(char.encode("utf-16-le")) // 2
    assert units == point["character"], (point, content)
    return base + len(content)


def span(text, start, end):
    return {"start": position(text, start), "end": position(text, end)}


def range_text(text, item):
    start, end = offset(text, item["start"]), offset(text, item["end"])
    assert start <= end, item
    return text[start:end]


def symbols(items):
    for item in items:
        yield item
        yield from symbols(item.get("children", []))


class Client:
    def __init__(self, command, output):
        self.output = output
        self.log = output.with_suffix(".stderr.log").open("w")
        temporary = output.with_suffix(".tmp")
        temporary.mkdir()
        environment = dict(os.environ, TMPDIR=str(temporary))
        self.proc = subprocess.Popen(command, cwd=REPO, stdin=subprocess.PIPE,
                                     stdout=subprocess.PIPE, stderr=self.log,
                                     bufsize=0, start_new_session=True, env=environment)
        self.buffer = bytearray()
        self.history = []
        self.next_id = 0

    def send(self, method, params=None, request=False):
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        if request:
            self.next_id += 1
            message["id"] = self.next_id
        self.write(message)
        return message.get("id")

    def write(self, message):
        payload = json.dumps(message, ensure_ascii=False).encode("utf-8")
        self.proc.stdin.write(f"Content-Length: {len(payload)}\r\n\r\n".encode() + payload)
        self.proc.stdin.flush()

    def receive(self, predicate):
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            marker = self.buffer.find(b"\r\n\r\n")
            if marker >= 0:
                headers = bytes(self.buffer[:marker]).decode("ascii").split("\r\n")
                sizes = [int(line.split(":", 1)[1]) for line in headers
                         if line.lower().startswith("content-length:")]
                assert len(sizes) == 1 and 0 <= sizes[0] <= 64 * 1024 * 1024, headers
                end = marker + 4 + sizes[0]
                if len(self.buffer) >= end:
                    message = json.loads(self.buffer[marker + 4:end])
                    del self.buffer[:end]
                    self.history.append(message)
                    if "method" in message and "id" in message:
                        # Acknowledge optional server refresh requests. Their
                        # payloads never become source edits in this driver.
                        self.write({"jsonrpc": "2.0", "id": message["id"], "result": None})
                    if predicate(message):
                        return message
                    continue
            ready, _, _ = select.select([self.proc.stdout], [], [],
                                        max(0, deadline - time.monotonic()))
            if not ready:
                break
            chunk = os.read(self.proc.stdout.fileno(), 65536)
            if not chunk:
                raise RuntimeError(f"LSP closed stdout (exit {self.proc.poll()})")
            self.buffer.extend(chunk)
        raise TimeoutError("LSP request exceeded 60 seconds")

    def request(self, method, params=None):
        request_id = self.send(method, params, request=True)
        message = self.receive(lambda message: message.get("id") == request_id)
        assert "error" not in message, (method, message)
        return message["result"]

    def barrier(self):
        return self.request("svelte/sessionStats")

    def open(self, uri, source, version):
        self.send("textDocument/didOpen", {"textDocument": {
            "uri": uri, "version": version, "languageId": "svelte", "text": source}})
        self.barrier()

    def change(self, uri, source, version):
        self.send("textDocument/didChange", {"textDocument": {"uri": uri, "version": version},
                                          "contentChanges": [{"text": source}]})
        self.barrier()

    def published(self, uri):
        matches = [message["params"] for message in self.history
                   if message.get("method") == "textDocument/publishDiagnostics"
                   and message["params"]["uri"] == uri]
        assert matches, "Expected published diagnostics"
        return matches[-1]

    def diagnostics(self, uri):
        return self.request("textDocument/diagnostic", {"textDocument": {"uri": uri}})["items"]

    def close(self):
        try:
            if self.proc.poll() is None:
                self.request("shutdown")
                self.send("exit")
                self.proc.stdin.close()
                self.proc.wait(timeout=5)
        finally:
            if self.proc.poll() is None:
                os.killpg(self.proc.pid, signal.SIGKILL)
                self.proc.wait(timeout=5)
            self.log.close()
            self.output.with_suffix(".messages.json").write_text(json.dumps(self.history, indent=2) + "\n")
        assert self.proc.returncode == 0, self.proc.returncode


def edits(action, uri, version):
    workspace = action["edit"]
    assert "changes" not in workspace, workspace
    changes = workspace["documentChanges"]
    assert len(changes) == 1, changes
    assert changes[0]["textDocument"] == {"uri": uri, "version": version}, changes
    assert changes[0]["edits"], action
    return changes[0]["edits"]


def apply(text, changes):
    replacements = sorted((offset(text, item["range"]["start"]),
                           offset(text, item["range"]["end"]), item["newText"])
                          for item in changes)
    for left, right in zip(replacements, replacements[1:]):
        assert left[1] <= right[0] and left[0] != right[0], replacements
    for start, end, new in reversed(replacements):
        text = text[:start] + new + text[end:]
    return text


def standalone(out, passed):
    source = ('<script lang="ts">\n'
              '  const title = "Native";\n'
              '  const disabled = false;\n'
              '  const visible = true;\n'
              '</script>\n'
              '<section>\n'
              '  <!-- native folding\n'
              '       second comment line -->\n'
              '  {#if visible}\n'
              '    <section>\n'
              '      🤖<button title={title} disabled={disabled}>Hello</button>\n'
              '    </section>\n'
              '  {/if}\n'
              '</section>\n')
    path = out / "Standalone.svelte"
    path.write_text(source)
    uri = path.as_uri()
    lint = {"rules": {"svelte/compiler": "off", "svelte/shorthand-attribute": "warn"}}
    client = Client([str(REPO / "target/release/svelte-lsp")], out / "standalone")
    try:
        initialized = client.request("initialize", {"capabilities": {"textDocument": {
            "documentSymbol": {"hierarchicalDocumentSymbolSupport": True},
            "codeAction": {"disabledSupport": True, "isPreferredSupport": True}}},
            "initializationOptions": {"runes": True, "lintConfig": lint}})
        caps = initialized["capabilities"]
        for capability in ("linkedEditingRangeProvider", "foldingRangeProvider", "selectionRangeProvider", "documentSymbolProvider"):
            assert caps[capability] is True, initialized
        assert "source.fixAll.svelte" in caps["codeActionProvider"]["codeActionKinds"], initialized
        client.send("initialized", {})
        client.open(uri, source, -7)
        diagnostics = client.diagnostics(uri)
        assert len(diagnostics) == 2, diagnostics
        assert all(item["code"] == "svelte/shorthand-attribute" and item["severity"] == 2 for item in diagnostics), diagnostics
        assert client.published(uri)["version"] == -7
        passed.append("standalone-capabilities-configured-template-lint-negative-version")

        inner = source.index("<section>", source.index("<section>") + 1) + 1
        linked = client.request("textDocument/linkedEditingRange", {
            "textDocument": {"uri": uri}, "position": position(source, inner + 2)})
        assert [range_text(source, item) for item in linked["ranges"]] == ["section", "section"], linked
        assert linked["ranges"][0]["start"] == position(source, inner), linked
        assert linked["ranges"][1]["start"] == position(source, source.index("</section>") + 2), linked
        button = source.index("<button") + 1
        linked_button = client.request("textDocument/linkedEditingRange", {
            "textDocument": {"uri": uri}, "position": position(source, button + 2)})
        assert range_text(source, linked_button["ranges"][0]) == "button", linked_button
        assert linked_button["ranges"][0]["start"]["character"] == 9, linked_button
        assert [range_text(source, item) for item in linked_button["ranges"]] == ["button", "button"]
        passed.append("standalone-linked-tags-nested-pairs-and-utf16")

        folds = client.request("textDocument/foldingRange", {"textDocument": {"uri": uri}})
        expected = {(5, 13, "region"), (6, 7, "comment"), (8, 12, "region"), (9, 11, "region")}
        actual = {(item["startLine"], item["endLine"], item.get("kind")) for item in folds}
        assert expected <= actual, folds
        passed.append("standalone-template-block-element-and-comment-folding")

        selection = client.request("textDocument/selectionRange", {
            "textDocument": {"uri": uri}, "positions": [position(source, button + 2)]})
        assert len(selection) == 1 and range_text(source, selection[0]["range"]) == "button", selection
        selected = selection[0]
        depth = 0
        while "parent" in selected:
            parent = selected["parent"]
            assert offset(source, parent["range"]["start"]) <= offset(source, selected["range"]["start"])
            assert offset(source, parent["range"]["end"]) >= offset(source, selected["range"]["end"])
            range_text(source, parent["range"])
            selected = parent
            depth += 1
            assert depth < 32
        assert depth >= 4 and range_text(source, selected["range"]) == source, selection
        document_symbols = client.request("textDocument/documentSymbol", {"textDocument": {"uri": uri}})
        flattened = list(symbols(document_symbols))
        assert sum(item["name"] == "section" for item in flattened) == 2, document_symbols
        assert any(item["name"] == "{#if}" for item in flattened), document_symbols
        button_symbol = next(item for item in flattened if item["name"] == "button")
        assert range_text(source, button_symbol["selectionRange"]) == "button", button_symbol
        assert range_text(source, button_symbol["range"]).startswith("<button "), button_symbol
        passed.append("standalone-selection-hierarchy-and-template-document-symbols")

        def actions(text, only, requested_range=None, diagnostics=None):
            return client.request("textDocument/codeAction", {"textDocument": {"uri": uri},
                "range": requested_range or span(text, 0, len(text)),
                "context": {"only": only, "diagnostics": diagnostics or []}})

        start = source.index("title={title}")
        malicious = [{"range": span(source, start, start + 5), "message": "Client payload", "data": {
            "fix": {"edits": [{"range": span(source, 0, len(source)), "newText": "CLIENT-EDIT"}]}}}]
        quickfixes = actions(source, ["quickfix"], span(source, start, start + 5), malicious)
        assert len(quickfixes) == 1, quickfixes
        assert quickfixes[0]["isPreferred"] is True, quickfixes
        assert "CLIENT-EDIT" not in json.dumps(quickfixes)
        quick_edits = edits(quickfixes[0], uri, -7)
        assert apply(source, quick_edits) == source.replace("title={title}", "{title}", 1)
        assert actions(source, ["refactor"]) == []
        all_actions = actions(source, ["source.fixAll.svelte"])
        assert len(all_actions) == 1 and all_actions[0]["kind"] == "source.fixAll.svelte", all_actions
        all_edits = edits(all_actions[0], uri, -7)
        assert len(all_edits) == 2, all_edits
        assert apply(source, all_edits) == source.replace("title={title}", "{title}").replace("disabled={disabled}", "{disabled}")
        passed.append("native-quickfix-filtering-untrusted-data-and-atomic-fix-all")

        source = apply(source, quick_edits)
        client.change(uri, source, -6)
        remaining = client.diagnostics(uri)
        assert len(remaining) == 1 and range_text(source, remaining[0]["range"]) == "disabled={disabled}", remaining
        repair = actions(source, ["source.fixAll.svelte"])
        source = apply(source, edits(repair[0], uri, -6))
        client.change(uri, source, -5)
        assert client.diagnostics(uri) == []
        assert client.published(uri)["version"] == -5
        assert actions(source, ["source.fixAll.svelte"]) == []
        passed.append("native-fixes-recheck-in-same-process-and-restore-signed-edit-versions")

        never = {"rules": {"svelte/compiler": "off", "svelte/shorthand-attribute": ["error", {"prefer": "never"}]}}
        client.send("workspace/didChangeConfiguration", {"settings": {"runes": True, "lintConfig": never}})
        client.barrier()
        diagnostics = client.diagnostics(uri)
        assert len(diagnostics) == 2 and all(item["severity"] == 1 for item in diagnostics), diagnostics
        repair = actions(source, ["source.fixAll.svelte"])
        source = apply(source, edits(repair[0], uri, -5))
        client.change(uri, source, -4)
        assert client.diagnostics(uri) == []
        assert "title={title}" in source and "disabled={disabled}" in source
        passed.append("persistent-lint-reconfiguration-options-severity-and-fixes")

        assert caps["documentFormattingProvider"] is True, initialized
        unformatted = '<script lang="ts">let value:number=1;</script>\r\n<p>🤖{value}</p>\r\n'
        client.change(uri, unformatted, -3)
        formatting = client.request("textDocument/formatting", {
            "textDocument": {"uri": uri}, "options": {"tabSize": 2, "insertSpaces": True}})
        assert len(formatting) == 1, formatting
        assert formatting[0]["range"] == {"start": position(unformatted, 0), "end": position(unformatted, len(unformatted))}, formatting
        formatted = apply(unformatted, formatting)
        assert "🤖" in formatted and "let value: number = 1;" in formatted, formatted
        client.change(uri, formatted, -2)
        assert client.request("textDocument/formatting", {
            "textDocument": {"uri": uri}, "options": {"tabSize": 2, "insertSpaces": True}}) == []
        passed.append("native-formatting-whole-document-utf16-crlf-and-idempotence")
    finally:
        client.close()


def native_kit(dependencies, out, passed):
    root = out / "kit-project"
    (root / "src/routes").mkdir(parents=True)
    (root / "src/lib").mkdir()
    modules = root / "node_modules"
    modules.mkdir()
    # Each package stays read-only in its original directory. Kit 3's generated
    # $app package must be local, never a link into shared installed dependencies.
    for entry in dependencies.iterdir():
        if entry.name != "$app":
            (modules / entry.name).symlink_to(entry, target_is_directory=entry.is_dir())
    kit_version = json.loads((dependencies / "@sveltejs/kit/package.json").read_text())["version"]
    major = int(kit_version.split(".")[0])
    assert major in (2, 3), kit_version
    (root / "package.json").write_text(json.dumps({"name": "native-editor-fixture", "private": True, "type": "module"}) + "\n")
    config = 'export default { compilerOptions: { runes: true }, kit: {} };\n' if major == 2 else 'export default { compilerOptions: { runes: true } };\n'
    (root / "svelte.config.js").write_text(config)
    extends = "./.svelte-kit/tsconfig.json" if major == 2 else "$app/tsconfig"
    (root / "tsconfig.json").write_text(json.dumps({"extends": extends, "compilerOptions": {
        "strict": True, "skipLibCheck": True, "allowJs": True, "checkJs": True, "moduleResolution": "bundler"}}) + "\n")
    (root / "src/app.html").write_text('<!doctype html><html lang="en"><head>%sveltekit.head%</head><body><div>%sveltekit.body%</div></body></html>\n')
    (root / "src/app.d.ts").write_text("declare global { namespace App {} }\nexport {};\n")
    (root / "src/lib/labels.ts").write_text('export const label: string = "Native";\n')
    original = '<script lang="ts">import { label } from "$lib/labels";</script>\n<p>{label}</p>\n'
    page = root / "src/routes/+page.svelte"
    page.write_text(original)
    names = out / "empty-env-names.json"
    names.write_text("[]\n")
    binary = REPO / "target/release/svelte-kit-project"
    temporary = out / "kit-sync.tmp"
    temporary.mkdir()
    sync = subprocess.run([str(binary), "sync", str(root), "--env-names", str(names)],
                          cwd=REPO, capture_output=True, text=True, timeout=60,
                          env=dict(os.environ, TMPDIR=str(temporary)))
    (out / "kit-sync.stdout.json").write_text(sync.stdout)
    (out / "kit-sync.stderr.log").write_text(sync.stderr)
    assert sync.returncode == 0, (sync.returncode, sync.stderr, sync.stdout)
    assert json.loads(sync.stdout).get("ok") is True, sync.stdout
    client = Client([str(binary), "serve", str(root), "runes"], out / "kit")
    uri = page.as_uri()
    try:
        initialized = client.request("initialize", {"rootUri": root.as_uri(), "capabilities": {}})
        assert initialized["capabilities"]["diagnosticProvider"]["interFileDependencies"] is True
        client.send("initialized", {})
        client.open(uri, original, 1)
        diagnostics = client.diagnostics(uri)
        assert not [item for item in diagnostics if item.get("severity") == 1], diagnostics
        passed.append("synthetic-kit-native-sync-and-provider-backed-lsp")

        added = root / "src/lib/added.ts"
        added.write_text("export const added: number = 42;\n")
        edited = '<script lang="ts">import { added } from "$lib/added";</script>\n<p>{added}</p>\n'
        client.change(uri, edited, 2)
        before = client.diagnostics(uri)
        assert any(item.get("code") == "incomplete_project_check" for item in before), before
        client.send("workspace/didChangeWatchedFiles", {"changes": [{"uri": added.as_uri(), "type": 1}]})
        client.barrier()
        after = client.diagnostics(uri)
        assert not [item for item in after if item.get("severity") == 1], after
        definitions = client.request("textDocument/definition", {"textDocument": {"uri": uri},
            "position": position(edited, edited.rindex("added}"))})
        assert isinstance(definitions, list) and any(item.get("uri", item.get("targetUri")) == added.as_uri() for item in definitions), definitions
        passed.append("kit-watched-addition-rediscovery-clears-stale-graph-and-resolves-definition")

        added.unlink()
        client.send("workspace/didChangeWatchedFiles", {"changes": [{"uri": added.as_uri(), "type": 3}]})
        client.barrier()
        deleted = client.diagnostics(uri)
        assert any(item.get("code") == 2307 for item in deleted), deleted
        assert not any(item.get("code") == "incomplete_project_check" for item in deleted), deleted
        client.change(uri, original, 3)
        repaired = client.diagnostics(uri)
        assert not [item for item in repaired if item.get("severity") == 1], repaired
        passed.append("kit-watched-deletion-reloads-roster-and-repair-clears-diagnostic")

        route = root / "src/routes/added"
        route.mkdir()
        load_file = route / "+page.ts"
        route_page = route / "+page.svelte"
        load_file.write_text("export const load = () => ({ title: 42 });\n")
        route_text = '<script lang="ts">let { data }: { data: import("./$types").PageData } = $props(); const title: string = data.title;</script>\n<p>{title}</p>\n'
        route_page.write_text(route_text)
        client.send("workspace/didChangeWatchedFiles", {"changes": [
            {"uri": load_file.as_uri(), "type": 1}, {"uri": route_page.as_uri(), "type": 1}]})
        client.barrier()
        assert (root / ".svelte-kit/types/src/routes/added/$types.d.ts").is_file()
        client.open(route_page.as_uri(), route_text, 1)
        route_diagnostics = client.diagnostics(route_page.as_uri())
        assert any(item.get("code") == 2322 for item in route_diagnostics), route_diagnostics
        assert not any(item.get("code") in (2307, "incomplete_project_check") for item in route_diagnostics), route_diagnostics
        repaired_route = route_text.replace("title: string", "title: number")
        client.change(route_page.as_uri(), repaired_route, 2)
        assert not [item for item in client.diagnostics(route_page.as_uri()) if item.get("severity") == 1]
        passed.append("kit-new-route-watch-regenerates-types-and-checks-load-data-without-restart")
    finally:
        client.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("dependencies", type=pathlib.Path)
    parser.add_argument("output", type=pathlib.Path)
    parser.add_argument("--skip-kit", action="store_true")
    args = parser.parse_args()
    dependencies = args.dependencies.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    passed = []
    failures = []
    try:
        standalone(output, passed)
    except Exception:
        failures.append({"stage": "standalone", "error": traceback.format_exc()})
    if not args.skip_kit:
        try:
            native_kit(dependencies, output, passed)
        except Exception:
            failures.append({"stage": "kit-provider", "error": traceback.format_exc()})
    report = {"tests": [{"name": name, "status": "pass"} for name in passed],
              "failures": failures, "unrun": ["kit-provider"] if args.skip_kit else [],
              "scope": "Synthetic native editor protocol correctness only; no application trial or benchmark"}
    (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main())
