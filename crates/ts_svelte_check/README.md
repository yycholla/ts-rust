# Native Svelte checking prototype

`svelte-project` builds an isolated project from explicit source snapshots,
compiles/checks Svelte components with the Rust backend, and checks their full
virtual TypeScript/JavaScript with the existing Rust TypeScript engine. Node is
not used by these production commands. Tests use the installed JavaScript tools
as an independent oracle.

Build the compiler first with `scripts/svelte/build-compiler.sh`, then:

```sh
scripts/run-cargo-capped.sh build --release --locked -p ts_svelte_check --bins
```

The three sibling executables are `svelte-project`, `svelte-tool` and
`svelte-ts-check` in `target/release/`.

Pass one JSON request on stdin:

```json
{
  "workspace": "/tmp/new-svelte-project",
  "dependencies": "/home/yycholla/Documents/MoncuraHR/node_modules",
  "components": [
    {
      "path": "Counter.svelte",
      "document": {
        "uri": "file:///Counter.svelte",
        "version": 1,
        "text": "<script lang=\"ts\">let count = $state(1);</script><p>{count.notARealMember()}</p>"
      },
      "typescript": true
    }
  ],
  "files": []
}
```

The workspace parent must exist and the workspace directory must be new.
Existing files are never overwritten; failed workspaces remain inspectable.
`files` may contain ordinary project source snapshots `{ "path", "text" }`.
Explicit `.svelte.ts` and `.svelte.js` inputs fail with
`unsupported_rune_module` before workspace creation; declaration files remain
allowed. Transitive rune-module compiler diagnostics are not covered.
Paths are relative to the isolated workspace. Component imports retain their
relative paths. Both `.svelte.d.ts` and `.d.svelte.ts` re-export bridges preserve
component contracts. JavaScript components use `.svelte.jsx` with generated JSDoc helpers and
`checkJs`; TypeScript components use `.svelte.tsx`.

The adapter defaults to runes mode, strict checking and bundler resolution.
`runes: false` selects legacy mode; `runes: null` lets each component infer it.
It reads exact Svelte, optional SvelteKit and optional TypeScript versions from
the supplied `dependencies` directory and resolves a shared compatibility profile.
An experimental Kit pin requires `allowPrerelease: true`. Unknown versions fail
before workspace creation. This is one explicit dependency root, not Node-style
ancestor or nested dependency resolution. It loads installed Svelte declarations
and `svelte-check` shims. TypeScript version metadata does not switch the Rust
checker's embedded standard libraries or emulate that TypeScript release. Its
isolated declaration copy removes the generic `*.svelte` fallback, so a missing
component cannot silently become an untyped component. Generated type references
are masked without changing source offsets. The application checkout and its
configuration are not modified or executed.

The response retains compiler diagnostics and the full checker diagnostic tree.
`ok: true` means the check completed, including when it found type errors.
The process exits 0 only for a completed error-free check; otherwise it exits 1.
Incomplete backend work has `ok: false`. No diagnostics are discarded because
they point at generated helper code.

Checker diagnostics preserve their generated `fileName`, UTF-16 `pos`/`end`,
messages, related information and nested message chains. `original` adds a
validated original URI, version, UTF-8 byte range and UTF-16 editor range when
possible. Other statuses are `unmapped`, `unprojected` and `project`. Maps are
validated against exact copied text and against the source actually loaded by
the checker, using its existing program-created hook. Early configuration errors
have no verified projection and are retained without claiming original ranges.

`svelte-ts-check` is the lower-level one-project-per-process JSON bridge:

```json
{
  "project": "/absolute/overlay/tsconfig.json",
  "projections": [
    {
      "fileName": "/absolute/overlay/Counter.svelte.tsx",
      "document": { "uri": "file:///Counter.svelte", "version": 1, "text": "..." },
      "code": "...",
      "mappings": []
    }
  ]
}
```

Projection entries are optional for ordinary TypeScript projects. An empty map
explicitly leaves generated diagnostics unmapped. The bridge forces `noEmit`,
disables incremental build-info writing, forces semantic checking even with
`noCheck:true`, and refuses emit callbacks. Panics, unported engine paths and
diagnostic serialization errors produce incomplete results.

Requests are limited to 16 MiB. The project adapter accepts up to 4,096 inputs;
the Svelte compiler limits each component to 1 MiB. Expansion beyond the checker
request limit fails explicitly before starting the checker.

This is an explicit-input prototype. It does not yet load SvelteKit projects,
generate route/environment types, process aliases or preprocessors, compile rune
modules, or provide an LSP. It rebuilds an isolated graph for each check. It does
not establish a speed or memory improvement. The original TypeScript compiler
implementation remains unchanged.
