# Snapshot JavaScript runtime

`Runtime::new(HostSnapshot, RuntimeLimits)` creates one thread-confined Boa 0.22
context. `evaluate(entry)`, `read_export(entry, name)` and
`invoke_export(entry, name, arguments)` execute real ECMAScript modules and await
their promises. Results distinguish absent exports, JavaScript undefined and
JSON null. The context caches module identity and supports static cycles and
dynamic imports using **exact** snapshot resolution records.

Module IDs are opaque host keys. Each source includes its explicit
`import_meta_url`; internal path tags identify referrers without filesystem
access. Explicit `builtin` resolutions can name enabled native modules.
Import attributes, external modules and missing resolution records fail
explicitly. No implicit path search, environment variables, filesystem, network,
timers, DOM, fetch, console, Node or Svelte APIs are installed. Standard Boa
ECMAScript intrinsics remain available. This is not a browser polyfill package.

Limits cover source bytes/modules, argument/result JSON size/depth/node count,
Boa VM instructions, loop iterations, recursion, stack, queued jobs and job/future
poll count. The Boa `fuzz` feature is enabled specifically for its public real
instruction counter. Instruction and job budgets are cumulative for the lifetime
of this runtime; recreating it explicitly starts a new budget. A failed execution
poisons the runtime so leftover jobs cannot be reused as success. Unsupported
pending promises fail rather than spin or sleep awaiting nonexistent I/O.

These are execution/shape bounds, not a hard native heap or wall-time sandbox:
some built-in algorithms and parsing execute outside VM instruction dispatch.
Embedders requiring hostile-code memory/process isolation must supply it.
The library uses no OS I/O or threading and is designed for native/WASM hosts;
target compilation is separate evidence, not implied by this API.

JSON arguments are data only. Results support finite primitives, ordinary
objects and arrays, with bounded traversal and cycle detection. Unsupported
values (functions, BigInt, symbols, non-JSON nested undefined, exotic objects)
fail instead of being silently coerced. Object accessors are read using normal
ECMAScript property access. This strict data boundary is not JSON.stringify or
a structured-clone implementation. No executable application configuration is
loaded automatically.

Root owns builds and tests through the repository's capped Cargo runner.

## Persistent process interface

`js-runtime --serve` accepts newline-delimited JSON requests and emits one JSON
response per request. `create` takes an explicit `snapshot` and optional
`limits`, returning a session ID. `evaluate`, `readExport` and `invokeExport`
use that session and a module ID; export operations also name the export, and
invocation takes an `arguments` array. `dispose` releases the context.

Each process retains at most eight contexts with independent module caches and
cumulative execution budgets. Missing/disposed sessions and malformed requests
produce structured errors. `capabilities` lists the available host capabilities
(opt-in URL, text, Headers, Blob/File, FormData, Abort/EventTarget, readable
streams, Request/Response, async context and crypto). The same transport-neutral `protocol::Service`
is available to in-process native or worker hosts.


`create.capabilities` explicitly installs native APIs; defaults remain empty.
`formData` also installs `blob`, which installs `streams`; `abort` also installs `text` for native
DOMException construction. Event-listener exceptions are returned in
`reportedErrors` without changing dispatchEvent return behavior. Rust embedders
call `take_reported_errors` to consume the same queue.

These APIs are implemented in Rust against Boa native classes. Readable streams
include default and byte controllers, BYOB readers, iteration and teeing.
Writable and transform streams share the same native promise and queue engines.
Native `pipeTo`/`pipeThrough` preserve backpressure, lock ownership, cancellation,
abort signaling, and shutdown error precedence.
Blob/File streaming uses native byte streams. `requestResponse` installs its
URL, text, Headers, FormData and Abort dependencies and provides native body
consumption, cloning and static response constructors. It does not install a
network `fetch` function.

`create.fetchOptions` accepts an optional `baseUrl`, `origin` and
`multipartEntropy`: an explicit host-supplied queue of 16-byte entropy blocks
used for multipart boundaries. Native embedders can instead supply an entropy
callback through `Runtime::with_fetch_options`. Multipart encoding fails if
required entropy is absent or exhausted. These options require the
`requestResponse` capability.

`nodeAsyncHooks` enables explicit resolutions such as
`{"kind":"builtin","name":"node:async_hooks"}`. The `async_hooks` alias shares
that module. Native AsyncLocalStorage preserves stores through promises and
`await`; it does not add a global constructor. Other Node async_hooks exports
are unavailable. Host callbacks can capture their scheduling context and restore
it at dispatch through `node_async_hooks::capture`.

`crypto` installs native SHA-1/SHA-256/SHA-384/SHA-512 digest operations, integer
array randomness and UUID v4. Random operations require an explicit entropy
provider through `Runtime::with_host_options`, or host-generated bytes in
`create.cryptoOptions.entropy` (at most 1 MiB). Exhaustion fails; no seeded or
implicit fallback exists. Other SubtleCrypto operations are unavailable.

Trusted Rust drivers use `Runtime::native_operation` to keep real module
namespaces and callbacks inside Boa. `NativeSession` provides module evaluation,
call invocation, bounded promise settlement and result normalization. Operations
return JSON and retain cumulative budgets and failure poisoning. This native
embedding interface is not exposed by the JSON process/worker protocol.

The vendored engine's `NATIVE_REACTIONS.patch` also repairs `JsPromise::resolve`:
upstream 0.22.0 turned an abrupt `constructor` getter on a native promise
(PromiseResolve step 1.a) into an uncatchable `EnginePanic`. It now propagates
the getter's thrown value unchanged, as `Promise.resolve` and `await` do.

Abort timeouts, network transport and additional Node APIs
require further implementation. Multipart decoding supports bounded
RFC7578 forms and rejects unsupported transfer encodings/extended filenames.
