# SvelteKit integration plugin

This crate owns the framework host contract. It invokes an installed Kit/Vite
pipeline through `npm/svelte-kit`, which replaces component and rune-module
compilation with the Rust compiler protocol. The compiler crate never depends
on this plugin.

Current validated target: MoncuraHR's Kit 2.69.2 / Vite-plugin-svelte 6.2.4.
The loader rejects unvalidated plugin layouts and unknown compilation callsites.
Full builds retain official JavaScript preprocessing, routing, bundling,
Tailwind and Node adapter behavior. They are **hybrid builds**, not fully native
Kit builds. There is no silent fallback to the JavaScript Svelte compiler.

`ts_svelte_kit --help` documents the explicit host paths. Run Kit sync first.
The isolated comparison helper is `scripts/svelte/application-build.py`.

Native discovery, checking and editor launch consume an **existing Kit sync
snapshot** without executing configuration:

```sh
svelte-kit-project /path/to/app
svelte-kit-project check /path/to/app /new/overlay-directory runes
svelte-kit-project serve /path/to/app runes
```

Use `legacy` or `infer` instead of `runes` when appropriate. `request` and `lsp`
emit the generic checker request and editor configuration for other hosts.
The launcher requires sibling `svelte-project`, `svelte-ts-service`, `svelte-ts-check`,
`svelte-ts-transpile`, `svelte-tool` and `svelte-lsp` executables as applicable.
The plugin has no compiler or tooling crate dependency.

Discovery handles project-relative JSONC extends, aliases, rootDirs, explicit
files, include/exclude, wildcard extension priority, package/array tsconfig
extends and generated route/env declarations. It rejects project references, baseUrl and
TypeScript plugins. Limits: 4,096 inputs, 1 MiB each, 64 MiB serialized request.
Auxiliary source discovery excludes dependency/build/static/hidden directories
apart from Kit types; imports into those directories are outside this prototype.
Excluded imported sources inside the captured graph remain available to checking.
Compiler diagnostics from auxiliary sources are restricted to loaded files.
An unprojectable auxiliary component can still prevent preparation even if unused.

Native `svelte-kit-project sync ROOT` separately generates registered Kit2/Kit3
route, environment and configuration declarations from static configuration or an
explicit resolved JSON contract. It does not execute arbitrary configuration or
preprocessor callbacks. Source/config and dependency hashes bind each snapshot.
The editor launcher supplies a generic native project provider, enabling graph
rediscovery on watched files. Changed generated types still require sync.
JavaScript
component projections require the application's `allowJs` setting; it is never
silently enabled. Runtime TypeScript rune modules use the native TypeScript
transpiler; exact source-map spans are mapped back and generated helper spans
remain explicitly unmapped.
The compiler's WASM API supports erasable TypeScript module checking and reports
runtime TypeScript as unsupported; native process orchestration is not a browser API.

The `serve` launcher uses Unix exec so the editor owns the real server PID.
The native LSP handles SIGINT/SIGTERM by stopping any active checker process
group and cleaning its temporary overlay. Linux lifecycle probes cover idle and
active-check termination; Windows launcher support and uncatchable SIGKILL
cleanup are outside the validated scope.

After native `sync`, watched-file refresh regenerates owned Kit declarations and
rediscovers the project before restarting its semantic session. Open editor
buffers survive this reload. Refresh reuses the environment names recorded by
`sync`; it does not read environment values. Configuration must be statically
resolvable, and a missing native sync manifest produces an explicit error.

Native build components are under development separately from the hybrid host:
`inventory ROOT OPTIONS_JSON [--config RESOLVED_JSON]` resolves route nodes,
layouts/errors, hooks and runtime entries. `emit-client`, `emit-server`,
`emit-root`, `emit-ssr` and `emit-nodes` consume resolved JSON inputs and return generated
modules without writing application files. Bundling, preprocessing and CSS live
in separate crates. These generators do not yet constitute a complete native
Kit build or evaluate application exports/prerender routes.
