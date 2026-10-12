# Native SvelteKit runtime interface

This package calls `ts_svelte_runtime` through its real WebAssembly JSON export.
The Rust framework layer initializes Kit server snapshots and loads their node
modules and analyzes route metadata on the separate Boa runtime. JavaScript supplies the interface; it does
not reimplement framework initialization, asset streams or module execution.

Generate bindings from `target/wasm32-unknown-unknown/release/ts_svelte_runtime.wasm`
using wasm-bindgen **0.2.129**, `--target web --out-dir npm/svelte-runtime/generated`.
The generated files are ignored. Root's serialized Rust build queue produces the
WASM artifact. The package uses the same binding conventions as `npm/js-runtime`.

```js
import { initializeRuntime } from '@ts-rust/svelte-runtime';

// In browsers, omit moduleOrPath to load the adjacent generated .wasm URL.
// In Node, pass the emitted WASM bytes from the embedding's readFile call.
const runtime = await initializeRuntime({ moduleOrPath: wasmBytes });
const { session } = runtime.create({ bootstrap });
try {
  const { nodes } = runtime.loadNodes(session);
  console.log(nodes);
} finally {
  runtime.dispose(session);
}
```

`bootstrap` contains an explicit `serverBuildId`, `snapshot` of module sources and
exact resolutions, `manifestEntry`, version-specific `initialization`, environment
values and server-relative asset byte arrays. See [index.d.ts](./index.d.ts) for
the complete contract. `limits` may be omitted for native defaults; a supplied
limits object must contain every limit. Use ordinary byte arrays in JSON, not
`Uint8Array` objects.

The Kit 2 initialization profile names its internal and environment modules and
public/private environment prefixes. Kit 3 names the module exporting
`configure`. The runtime receives emitted module sources; it does not read
application configuration or `.env` files. Initialization/node inspection does
not execute route load functions or application init hooks. This interface is
not a network server or a complete prerendering implementation. Query
`capabilities()` for the native build's available operations.

`create({bootstrap, fetchOptions?, cryptoOptions?})` forwards explicit base URL,
origin and entropy pools. Production entropy must come from the embedding's
secure entropy source; the wrapper neither manufactures random bytes nor silently
replenishes a pool. Exhaustion remains a native error. No filesystem or network
access is granted by this interface.

Operations are synchronous. Run the client inside a dedicated worker for browser
or editor use. One initialized wasm-bindgen module owns its native service and
up to eight live sessions. Multiple `SvelteRuntime` wrappers around that module
share sessions; a fresh worker/module instance provides isolation. Disposal frees
a session, and a failed execution can poison that session until disposal.

Convenience methods throw `SvelteRuntimeError` with the native `code`, `message`
and complete `detail`. `request({...})` returns the original Rust success/error
envelope without unwrapping, allowing newer operations through the same boundary.
Transport errors and WASM traps propagate. `./wasm` also exposes the generated
module directly for hosts using the raw `svelte_runtime_json` protocol.

After building, run `node --test npm/svelte-runtime/test/wasm.test.mjs`. Set
`TS_SVELTE_RUNTIME_BINDINGS` and `TS_SVELTE_RUNTIME_WASM` to explicit filesystem
paths to test another emitted build. These tests instantiate actual WASM and use
synthetic Kit 2/3 contract modules to check initialization order, native asset
reads, cached node loading, host options, metadata inheritance/entries, poisoning and disposal. They fail if
bindings are absent; they are not application builds, benchmarks or a parity claim.

`analyzeMetadata(session, {profile, hash?})` evaluates route metadata using an
explicit pinned Kit version. Its result includes node facts and routes with
config, entries, methods, page options and action names/callability. Metadata
analysis invokes actual entries functions and endpoint module loaders; entries
and route analysis are cached. Subsequent calls must retain the same version
profile and hash option. Profiles from another initialization family are rejected.

Config, entries, prerender and page option values retain Rust's tagged
`RuntimeValue` representation (`absent`, `undefined`, or `json`); the wrapper does
not collapse undefined to null. Route load functions, handlers and actions remain
uninvoked. Metadata is not adapter feature validation or prerender execution.

For rendering, use `createPrerender({ input, fetchOptions?, cryptoOptions? })`,
then `startPrerender(session)` and `visitPrerender(session, encodedPath)`.
`publishPrerender(session, file, byteArray)` exposes explicitly saved output to
later visits. A rendering session uses an independent module realm, invokes
application init hooks only at start, and returns binary response/dependency
records. Header names, values and status text are byte arrays; decoding and
artifact policy belong to the caller. The native driver does not crawl links or
automatically publish responses. `dispose` releases either session kind.

For save-policy ordering, use the staged operations after `startPrerender`:

```js
const head = runtime.beginPrerenderVisit(session, '/page');
// Inspect head.response before consuming its body.
const main = runtime.readPrerenderMain(session, head.token);
// The embedding chooses the output identity and whether to save this response.
runtime.publishPrerender(session, 'page.html', main.body);
let dependency;
while ((dependency = runtime.nextPrerenderDependency(session, head.token)) !== null) {
  const record = runtime.readPrerenderDependency(session, head.token);
  // Apply the embedding's dependency save policy, then publish accepted bytes
  // before advancing to the next dependency.
}
```

`nextPrerenderDependency` inspects headers without accessing the dependency's
body getter. `readPrerenderDependency` accesses that getter and consumes any
required stream; published files are visible immediately to application reads.
Dependency head `routeId` is null; the completed record decodes it after body
consumption. Response headers can change during a body read, so completed records
carry the resulting headers. Headers and status text remain raw byte arrays.

Only one visit may be active per session. Read the main response once, then
alternate dependency inspection and reading until inspection returns null.
That null finishes the visit and releases retained objects. Tokens are scoped to
the session and must not be reused after completion. Invalid ordering or active
token misuse poisons the rendering session, requiring disposal. Publishing is
allowed between stages. `visitPrerender` remains the convenience collector; it
cannot interleave caller save policy with body reads. Capability
`stagedPrerenderVisits` identifies this interface; `prerender` remains false
because this package does not supply automatic crawling or artifact policy.

Run the runtime suites after building:
`node --test --test-isolation=none npm/svelte-runtime/test/*.test.mjs`.
