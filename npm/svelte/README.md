# ts-rust-svelte prototype

Experimental Rust tooling with separate source/mapping, compiler/projection, and
persistent document/lint APIs. These APIs have native and WASM targets.
The compiler uses pinned rsvelte; the existing TypeScript WASM package remains
separate. This JavaScript/WASM interface does not yet provide type checking, SvelteKit
builds or LSP. The [native project checker](../../crates/ts_svelte_check/README.md)
combines checking projections with the existing Rust TypeScript engine.

## Build and test

```sh
rustup target add wasm32-unknown-unknown
scripts/svelte/build.sh
scripts/run-cargo-capped.sh test --release --locked -p ts_svelte
node --test npm/svelte/test/source.test.mjs
```

To retain source hashes and logs for the whole runtime check, create the parent
directory then run `scripts/svelte/verify.sh path/to/new-evidence-directory`.
It refuses to overwrite a prior run. Also run
`scripts/run-cargo-capped.sh fmt --check -p ts_svelte -p ts_svelte_wasm` and
`scripts/run-cargo-capped.sh clippy --release --locked -p ts_svelte -p ts_svelte_wasm --all-targets -- -D warnings`.

The capped runner requires a user systemd session on Linux. On a NixOS host,
activate a Rust/C compiler environment with the WASM standard library before
running it. The output is `target/release/svelte-source` and
`npm/svelte/ts_svelte.wasm`. The generated WASM binary is not committed.

## JavaScript / TypeScript

Node:

```js
import { readFile } from 'node:fs/promises';
import { createSourceTools } from './index.js';

const tools = await createSourceTools(await readFile(new URL('./ts_svelte.wasm', import.meta.url)));
const result = tools.request({
    op: 'locate',
    document: { uri: 'file:///Counter.svelte', version: 1, text: '<p>😀</p>' },
    positions: [{ line: 0, character: 5 }],
    offsets: [7],
});
// result.result.byteOffsets = [7]
// result.result.positions = [{ line: 0, character: 5 }]
tools.dispose();
```

In a browser module Worker, import the same `index.js` and load bytes with
`fetch(new URL('./ts_svelte.wasm', import.meta.url)).then(r => r.arrayBuffer())`.
There are no Node/WASI imports. JS is only transport; coordinate conversion,
editing, mapping validation and protocol errors are implemented in Rust.

The source layer accepts either JavaScript or TypeScript text, including text
inside `.svelte`, `.svelte.js` and `.svelte.ts`; no syntax analysis is implied.

## Persistent semantic worker

The experimental `ts-rust-svelte/semantic` entry selects Node worker threads or
browser module workers. Build its separate WASI reactor with
`scripts/svelte/build-semantic.sh` and a Rust toolchain containing `wasm32-wasip1`.
This module keeps a Rust TypeScript project alive between requests. Supply a
complete in-memory filesystem, including dependencies and generated declarations:

```js
import { createSemanticSession } from 'ts-rust-svelte/semantic';

const session = await createSemanticSession({
  root: '/project',
  project: '/project/tsconfig.json',
  files: {
    '/project/tsconfig.json': '{"compilerOptions":{"strict":true}}',
    '/project/main.ts': 'export const count: number = "wrong";',
  },
});
const diagnostics = await session.request('diagnostics', {
  textDocument: { uri: 'file:///project/main.ts' },
});
await session.shutdown();
```

`files()` applies a batch of disk-snapshot changes; `request()` also supports
open/change/close and semantic editor operations. `snapshot()` captures replayable
inputs, not serialized checker memory. Requests are serialized with generation
and worker identity checks. A trap requires a new worker. The browser WASI shim
supplies clocks, randomness and output, without host filesystem access.

Svelte projection, original-source mapping and Kit type generation remain
separate layers. Supplying a raw `.svelte` file directly to this TypeScript host
does not provide component checking. This worker interface alone does not imply
complete browser editor or build parity.

## Source protocol

The native CLI accepts the identical request as JSON on stdin and writes one
response on stdout. Successful requests exit 0, protocol errors exit 1. Run
`svelte-source --help` or send `{"op":"capabilities"}` for capabilities.

- `locate`: `{ document, positions, offsets }` returns UTF-8 byte offsets for
  UTF-16 editor positions, and editor positions for byte offsets.
- `edit`: `{ document, version, edits }` applies edits sequentially to a new
  snapshot. `version` must increase; each edit is `{ range?, text }`. An absent
  or null range replaces all text. Failure never modifies the input snapshot.
- `project`: `{ document, chunks, ranges }` builds a virtual document from
  `{ copy: { start, end } }` and `{ generated: "helper text" }` chunks. `ranges`
  are generated UTF-8 byte ranges to map back to source bytes and editor ranges.
  Generated helper text, gaps and ambiguous boundaries return `null`.

Requests return `{ ok: true, result }` or `{ ok: false, error: { code, message } }`.
Malformed requests and unknown fields are errors. All ranges are half-open.
Line endings are LF, CRLF or CR. Invalid lines/columns, split Unicode codepoints,
split surrogate pairs and the interior of CRLF are rejected rather than clamped.
Unpaired UTF-16 surrogates are not valid source text for this UTF-8 engine.

Maps belong to one immutable source snapshot; they do not survive edits to a new
version. Only exact copies map. Contiguous copies coalesce; a diagnostic crossing
generated text or reordered source does not acquire a fabricated range. Point
diagnostics bordering helper text are conservatively unmapped.

Requests and projected text are capped at 16 MiB each. Wire coordinates and
versions are unsigned 32-bit integers on both targets. Requests allow at most
4,096 total operations (positions + offsets, edits, or chunks + ranges). A
conservative 128 MiB scan-work budget bounds repeated full-document/line scans;
large batches can return `size_limit` even when their input fits. Capabilities
exposes these limits. Split batches in the host as needed.

Calls are synchronous and serialized per instance.
Drop a disposed instance to reclaim its WASM memory; `dispose` frees Rust buffers
but WebAssembly memory cannot shrink. After a WASM trap, create a new instance.

This first implementation rebuilds line indexes after edits and uses a stateless
JSON protocol. It is not incremental parsing, a persistent language server, or a
measured performance improvement. See [the plan](../../docs/svelte-prototype/plan.md).


## Svelte compiler (experimental)

The isolated `crates/ts_svelte_compiler` workspace pins rsvelte commit
`5ed8ea3a3401b9fbbe780d3b18d6f90073291d6b` (facade 0.12.6, targeting Svelte
5.57.1). MoncuraHR uses Svelte 5.53.11; this is an explicit compatibility target,
not an assertion that these versions behave identically. The adapter preserves
the app version's accessibility click-warning wording; failed compilation uses
the core backend's structured error API to retain real messages and spans. The backend uses
registry OXC 0.149 rather than upstream's workspace Git patches. Its separate
lockfile avoids changing ts-rust's exact dependency versions. A documented
[vendored core patch](../../crates/ts_svelte_compiler/vendor/README.md) disables
a newer runtime optimization, preserving Svelte 5.53.11 child/reset emission.
Client development mode is available for the exact 5.57.2 profile. Older profiles
reject it explicitly. Emitted helpers absent from the selected runtime return
`runtime_compatibility` errors. Helper validation checks resolved import
bindings, static/string-computed member access, and rejects namespace escapes
or dynamic access. Export availability alone does not prove every helper ABI.

Install wasm-bindgen-cli **0.2.129**, matching the lockfile, then run:

```sh
scripts/svelte/build-compiler.sh
node --test npm/svelte/test/compiler.test.mjs
```

`WASM_BINDGEN` can name a binding-generator executable. Generated bindings and
WASM live in `generated/` and are not committed. This compiler WASM uses
wasm-bindgen imports; only the source engine above is import-free.

```js
import { readFile } from 'node:fs/promises';
import { createCompiler } from './compiler.js';

const compiler = await createCompiler(await readFile(
    new URL('./generated/ts_svelte_compiler_bg.wasm', import.meta.url)
));
try {
    const response = await compiler.request({
        op: 'compile', target: 'server', runes: true,
        document: {
            uri: 'Counter.svelte', version: 1,
            text: '<script>let count = $state(1);</script><p>{count}</p>',
        },
    });
    if (!response.ok) throw new Error(response.error.message);
    console.log(response.result.javascript.code);
} finally {
    compiler.dispose();
}
```

In browsers, provide bytes from `fetch` instead of `readFile`. Each compiler
owns a dedicated Node or browser module worker; requests are asynchronous and
processed serially. `dispose()` terminates that worker, releases its memory,
and rejects pending/future requests. A trapped backend is retired permanently.
Reuse a compiled `WebAssembly.Module` when creating multiple workers.

Native `target/release/svelte-tool` takes the same JSON request on stdin.
`compile` accepts `client`/`server`, optional `runes` and `dev`, returning JS,
external CSS, source maps and compiler diagnostics. `project` accepts explicit
`typescript: true|false` and returns the full virtual checking source, exact
copied-span mappings and prop facts. Projection output is not executable output
and is not itself proof of type checking. `typescript: false` rejects parsed
TypeScript script blocks; `true` can also project JavaScript input. JavaScript
projections use JSDoc helpers and a JSX parser; TypeScript uses TSX. Invalid JavaScript
components may fail during language validation; editor error recovery is not
yet supported.

Compiler requests are capped at 16 MiB and components at 1 MiB. BOM-prefixed
components are explicitly rejected because the pinned backend's BOM handling
can misalign offsets. Diagnostic `bytes` are raw backend UTF-8 offsets;
`range` is a validated UTF-16 range, or null when absent/invalid. Unlocated
errors remain visible. Native panic catching does not protect against stack
overflow or allocation failure; hosts running untrusted sources need process
resource limits and worker timeouts.

`compileModule` compiles JavaScript rune modules to client/server JavaScript.
As with the upstream compiler API, TypeScript rune modules must first have their
TypeScript syntax removed by a host preprocessor. The Kit adapter uses Vite for
that step. Preprocessing, SvelteKit generation, bundling and general hydration
parity remain outside the compiler crate's scope.
Run `scripts/svelte/verify-compiler.sh NEW_DIRECTORY APP_CHECKOUT` for retained
release/native/WASM evidence, pinned diagnostic comparisons, native checking,
and the JSDOM hydration/interaction matrix. Run
`scripts/svelte/quality.sh NEW_DIRECTORY APP_CHECKOUT` with rustfmt and clippy
installed for source-bound formatting, lint and declaration evidence.
`compiler-oracle.mjs --project-compile` also measures every app component without
executing its config or changing its inputs. All differences remain in the report.

## Exact version profiles

Native and WASM use the same Rust profile resolver. Omit `compatibility` to keep
MoncuraHR's Svelte 5.53.11 default, or pass it to `compile` and `project`:

```js
const compatibility = {
    svelteVersion: '5.57.2', kitVersion: '3.0.1', typescriptVersion: '6.0.3',
};
const profile = await compiler.request({ op: 'resolveCompatibility', versions: compatibility });
const compiled = await compiler.request({ op: 'compile', compatibility,
    target: 'server', document: { uri: 'Hello.svelte', version: 1, text: '<p>Hello</p>' } });
```

Recognized Svelte pins: **5.51.5, 5.52.0, 5.53.5, 5.53.11, 5.53.13,
5.54.1, 5.55.10, 5.56.10, 5.57.2**. Recognized Kit pins: **2.67.0, 2.68.0,
2.69.2, 2.69.3, 2.70.3, 3.0.1**, and experimental **3.0.0-next.32** with
`allowPrerelease: true`. At the recorded registry snapshot this `next` release
predates stable3.0.1; there is no separate beta tag. These are exact tested
targets, not promises for every
patch in a minor line. Unknown versions, ranges, and mutable tags fail with
`unsupported_version`; the compiler never downloads dependencies.

Kit 3 profiles require Svelte 5.57.2 and, when supplied, TypeScript 6.0.3.
TypeScript 5.9.3 and 6.0.3 identify comparison toolchains; the native checker
still uses its fixed Rust TypeScript implementation and embedded standard
libraries. This field does not emulate a TypeScript release or switch its libs.

Every resolved response carries the immutable profile. Capabilities distinguish
prototype component compilation/projection from Kit graph generation, Kit builds,
and LSP, which remain `false` in the compiler API. Separate host crates provide
explicit-project LSP and hybrid Kit builds. Kit version metadata identifies future config,
alias, and environment-module contracts; it does not implement those contracts.
The profile id alone is not a cache key: hosts must also include compiler build,
source snapshot, options and dependency identity. No shared compiler cache is
implemented here.

Version setup and verification tools:

```sh
python3 scripts/svelte/version-setup.py --help
node scripts/svelte/version-matrix.mjs --out path/to/new-version-evidence
node scripts/svelte/version-runtime.mjs path/to/new-runtime-evidence
```

The diagnostic matrix compares exact messages and ranges. Project comparisons
run official TypeScript on the same Rust checking projections; they do not prove
parity with the language-tools transformation. Runtime tests compare SSR output,
cross-compiler hydration, fresh mounting, state updates and select reset behavior
using JSDOM, not a full browser or application build. Each oracle installation is
pinned independently; experimental profiles never advance through a mutable tag.


## Separate tooling and framework integration

The dependency boundaries are documented in
[application architecture](../../docs/svelte-prototype/application-architecture.md).
`ts_svelte_compiler` does not depend on the tooling or Kit host.
`ts_svelte_tools` supplies native lint, a persistent JSON service, and stdio LSP.
The separate `ts_svelte_kit` and `npm/svelte-kit` adapter runs the installed Kit/Vite
build pipeline with native component and rune-module compilation. Kit/Vite,
preprocessing, bundling, Tailwind and adapter-node still execute JavaScript.

Build the tooling with `scripts/svelte/build-tools.sh`. Browser and Node consumers
can import `createTooling` from `ts-rust-svelte/tooling`, load `./tooling.wasm`, and
send `sessionOpen`, `sessionChange`, `sessionDiagnostics`, `sessionClose` and
`sessionStats` requests. Each worker owns an isolated session and must be disposed.
Browser tooling provides compiler/lint diagnostics; the native LSP can additionally
check explicitly listed project inputs through the Rust TypeScript checker.
See [tooling contracts and limits](../../crates/ts_svelte_tools/README.md).

Run `scripts/svelte/verify-application.sh NEW_DIRECTORY APP_CHECKOUT` for the
retained compiler/version suites plus tooling WASM, native lint, persistent LSP,
timeout recovery and session memory checks. Full hybrid builds and browser probes
use `application-build.py` and `application-browser.mjs`; invoke their `help`.
The lint rules are compiler/a11y/CSS diagnostics plus `no-debugger` and `eqeqeq`
in scripts; they do not reproduce the full oxlint/ESLint/Prettier configuration.
# Persistent project and formatting interfaces

`ts-rust-svelte/project` composes two Rust workers: the Svelte source/projection
frontend and a persistent TypeScript semantic session. Create one with
`createSvelteProjectSession({ root, files, components, documents, rootFiles,
compilerOptions, compatibility, toolingWasm, semanticWasm })`. Supply a complete
in-memory filesystem and resolved options; no application configuration or
filesystem discovery runs inside the workers. Components need the installed
Svelte declarations and svelte-check shim in that filesystem.

Call `request('textDocument/diagnostic', { textDocument: { uri } })` or another
supported LSP method with original source coordinates. `replace(snapshot)`
stages a full new snapshot, updates the existing checker, and commits source
mapping only after synchronization succeeds. Preparation errors retain the old
session; errors after partial synchronization retire the workers. Signed editor
versions and UTF-16 positions are preserved. `prepared()` exposes completion
metadata as well as generated code; `semanticComplete` is separate from rune
module transpilation completion. `dispose()` terminates both workers.

The current project interface requires canonical file URIs and caller-supplied
dependency files. Unsupported preprocessing and unresolved projections are
explicit errors or incomplete metadata. This API does not establish full
SvelteKit or language-server parity.

`ts-rust-svelte/formatter` provides `createFormatter(wasmModuleOrBytes)`, using
the separate `ts_svelte_format` crate. Its worker accepts the same JSON protocol
as `svelte-format --serve`: `{ op: 'format', document: { uri, version, text },
options: { tabWidth: 2 } }`. Successful results contain formatted code and an
atomic replacement edit in original UTF-16 coordinates. No files are written.
Build the native executable and WASM artifact with
`scripts/svelte/build-formatter.sh`. Tailwind class sorting, range formatting,
and executable formatter configuration remain unsupported.

