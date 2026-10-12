# Persistent Rust semantic host

This crate exposes the existing Rust TypeScript language service over a supplied
in-memory project. It neither invokes a JavaScript checker nor launches processes.
Svelte projection and Kit graph generation remain separate; callers send generated
TypeScript and its declarations, and can supply the existing `projectDiagnostics`
projection metadata to map diagnostics back to original documents.

`Host::request` and `Host::request_json` accept:

```json
{"id":1,"op":"initialize","params":{"root":"/project","project":"/project/tsconfig.json","files":{"/project/tsconfig.json":"{}","/project/main.ts":"export const count: number = 'error';"}}}
```

The reply contains `session` and `generation`. Echo both in every subsequent
request. Every dispatched request consumes a generation, including failures;
rejected stale envelopes do not. Requests use the native `svelte-ts-service`
operation names and LSP params. `open`/`change` preserve signed LSP versions;
versions must increase. `files` accepts `{files:{absolutePath:string|null}}`,
validates the entire batch, updates the VFS, and delivers watch invalidations.
Null removes a file. Open overlays continue to override filesystem contents.
`shutdown` is terminal. Drop/close releases the session's snapshot reference.

Filesystem paths are canonical case-sensitive absolute POSIX paths, with no
symlinks. Load all dependencies and generated declarations before querying them.
Empty directories are not modeled. Core filesystem writes are denied; explicit
`files` batches own mutation. Limits: 64 MiB encoded request/reply, 8 MiB per file
or open document, 256 MiB stored filesystem, and 100000 stored files. An error
after dispatch can leave accepted state changes applied; use the returned
generation. Oversized replies close the host rather than inviting unsafe retry.

## WASI worker ABI

Build the cdylib for `wasm32-wasip1`. Supply WASI preview1 clock, random and output
imports, as the existing `npm/wasm` host does. No host filesystem imports are used.
The existing `ts_wasm` ABI is untouched.

`scripts/svelte/build-semantic.sh` builds and copies the reactor into the JS
package. With an immutable source-built compiler, a downloaded standard library
can be incompatible even when its displayed version matches. In that case use
a project-local sysroot containing the matching Rust sources and Cargo's
`-Zbuild-std=std,panic_abort` support. `scripts/svelte/rustc-sysroot.sh` forwards
`TS_SVELTE_RUSTC` and `TS_SVELTE_SYSROOT` as a `RUSTC` wrapper so Cargo's sysroot
discovery sees that overlay. This does not modify the installed compiler.

1. `semantic_input(byteLength)` allocates a zeroed request buffer and returns its
   address; zero rejects the call. Reacquire the memory view after allocation.
2. Write UTF-8 JSON, then call `semantic_request()` once. Zero means a JSON reply
   is available; one means invalid ABI use. Inspect the reply's `ok` separately.
3. Copy `semantic_output_len()` bytes from `semantic_output()` before the next
   allocating call. Never call exports recursively from host imports.

One module instance owns one active session. Serialize worker messages. A WASM
trap or `proc_exit` poisons the entire instance: discard it and reconstruct files
and open overlays in a fresh worker. Native caught engine panics likewise require
discarding the owning thread, because core state may be inconsistent. Cancellation
is supported before dispatch by the caller, not during synchronous computation.

The wrapper bypasses the core's timer-arming document-event convenience methods:
it submits the same `FileChange` records and obtains snapshots synchronously.
All checker contexts use the persistent API lifetime, whose release does not arm
idle cleanup. Pending local tasks drain after requests; speculative auto-import
warming is cancelled and released. Completion requests still obtain auto-imports
on demand. No timer, thread, sleep, async filesystem or native watch is required.

This is single-configured-project semantics, as in the native service; cross-
project references/rename are not supplied. `wasm32-unknown-unknown` is explicitly
unsupported because the unchanged core uses `std::time::Instant`. WASI modules
can run in browser workers using a JS WASI shim. These source contracts do not
constitute a claim of tested WASM behavior or complete Svelte parity.
