import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { createSourceTools } from "../index.js";

const wasm = await readFile(new URL("../ts_svelte.wasm", import.meta.url));
const module = await WebAssembly.compile(wasm);
const native = fileURLToPath(new URL("../../../target/release/svelte-source", import.meta.url));
const document = { uri: "file:///Counter.svelte", version: 1, text: "a😀\r\nbé\n" };
const p = (line, character) => ({ line, character });

function runNative(request) {
    const child = spawnSync(native, [], { input: JSON.stringify(request), encoding: "utf8" });
    assert.ifError(child.error);
    assert.equal(child.stderr, "");
    const response = JSON.parse(child.stdout);
    assert.equal(child.status, response.ok ? 0 : 1);
    return response;
}

const cases = [
    ["capabilities", { op: "capabilities" }],
    ["UTF-16/UTF-8 and CRLF", { op: "locate", document, positions: [p(0,3),p(1,2),p(2,0)], offsets: [5,10,11] }],
    ["invalid surrogate boundary", { op: "locate", document, positions: [p(0,2)], offsets: [] }],
    ["invalid UTF-8 boundary", { op: "locate", document, positions: [], offsets: [2] }],
    ["invalid CRLF boundary", { op: "locate", document, positions: [], offsets: [6] }],
    ["invalid line", { op: "locate", document, positions: [p(100,0)], offsets: [] }],
    ["sequential edits", { op: "edit", document, version: 2, edits: [
        { range: { start:p(0,1), end:p(0,3) }, text:"x\ny" },
        { range: { start:p(1,0), end:p(1,1) }, text:"z" },
    ] }],
    ["stale version", { op: "edit", document, version: 1, edits: [] }],
    ["full replacement", { op: "edit", document, version: 9, edits: [{ text: "const x = 1;" }] }],
    ["exact mapping", { op: "project", document, chunks: [{ generated:"_" }, { copy:{ start:0,end:5 } }], ranges:[{start:2,end:6},{start:0,end:2},{start:1,end:1}] }],
    ["unpaired surrogate", { op: "edit", document, version: 2, edits: [{ text: "\ud800" }] }],
    ["unknown field", { op: "capabilities", extra: true }],
    ["unknown operation", { op: "check" }],
    ["negative offset", { op: "locate", document, positions: [], offsets: [-1] }],
    ...[2**32-1, 2**32, Number.MAX_SAFE_INTEGER, 0.5, 1e100].flatMap((value) => [
        [`offset ${value}`, { op:"locate", document, positions:[], offsets:[value] }],
        [`copy range ${value}`, { op:"project", document, chunks:[{copy:{start:0,end:value}}], ranges:[] }],
        [`generated range ${value}`, { op:"project", document, chunks:[], ranges:[{start:0,end:value}] }],
    ]),
    ["operation limit", { op:"locate", document, positions:[], offsets:Array(4097).fill(0) }],
    ["scan-work limit", { op:"locate", document:{...document,text:"x".repeat(1024*1024)}, positions:[], offsets:Array(129).fill(1024*1024) }],
];

for (const [name, request] of cases) {
    test(`native/WASM parity: ${name}`, async () => {
        const tools = await createSourceTools(module);
        try { assert.deepEqual(tools.request(request), runNative(request)); }
        finally { tools.dispose(); }
    });
}

test("coordinate results have independently specified expected values", async () => {
    const tools = await createSourceTools(module);
    try {
        const response = tools.request(cases[1][1]);
        assert.equal(response.ok, true);
        assert.deepEqual(response.result.byteOffsets, [5,10,11]);
        assert.deepEqual(response.result.positions, [p(0,3),p(1,2),p(2,0)]);
    } finally { tools.dispose(); }
});

test("actual WASM has no imports and handles memory growth and repeated calls", async () => {
    assert.deepEqual(WebAssembly.Module.imports(module), []);
    const tools = await createSourceTools(wasm);
    try {
        const longDocument = { ...document, text: "😀".repeat(100_000) };
        const result = tools.request({ op:"locate", document:longDocument, positions:[p(0,200_000)], offsets:[400_000] });
        assert.equal(result.ok, true);
        assert.deepEqual(result.result.byteOffsets, [400_000]);
        for (let i = 0; i < 100; ++i) {
            assert.equal(tools.request({ op:"check" }).ok, false);
            assert.equal(tools.request({ op:"capabilities" }).result.stage, "source-foundation");
        }
    } finally { tools.dispose(); }
});

test("disposal is idempotent and prevents use", async () => {
    const tools = await createSourceTools(module);
    tools.dispose();
    tools.dispose();
    assert.throws(() => tools.request({ op:"capabilities" }), /disposed/);
});

test("host serialization errors do not poison the instance", async () => {
    const tools = await createSourceTools(module);
    try {
        assert.throws(() => tools.request(undefined), /serializable/);
        assert.throws(() => tools.request({ value: 1n }), TypeError);
        assert.equal(tools.request({ op:"capabilities" }).ok, true);
    } finally { tools.dispose(); }
});

test("reentrant requests during serialization are rejected", async () => {
    const tools = await createSourceTools(module);
    try {
        const request = { toJSON() { tools.request({ op:"capabilities" }); } };
        assert.throws(() => tools.request(request), /reentrant/);
        assert.equal(tools.request({ op:"capabilities" }).ok, true);
    } finally { tools.dispose(); }
});

test("WASM ABI cannot replay a stale request after run or rejected allocation", async () => {
    const { exports: api } = await WebAssembly.instantiate(module, {});
    const read = () => JSON.parse(new TextDecoder().decode(new Uint8Array(api.memory.buffer, api.svelte_output(), api.svelte_output_len())));
    const bytes = new TextEncoder().encode('{"op":"capabilities"}');
    const pointer = api.svelte_input(bytes.length);
    new Uint8Array(api.memory.buffer, pointer, bytes.length).set(bytes);
    api.svelte_run();
    assert.equal(read().ok, true);
    api.svelte_run();
    assert.equal(read().ok, false);
    assert.equal(api.svelte_input(16*1024*1024 + 1), 0);
    api.svelte_run();
    assert.equal(read().ok, false);
    api.svelte_reset();
});

test("requests over the byte cap are rejected without poisoning the wrapper", async () => {
    const tools = await createSourceTools(module);
    try {
        assert.throws(() => tools.request({ value: "x".repeat(16*1024*1024) }), /16 MiB/);
        assert.equal(tools.request({ op:"capabilities" }).ok, true);
    } finally { tools.dispose(); }
});
