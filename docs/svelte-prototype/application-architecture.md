# Svelte and plugin boundaries

The Svelte compiler has no dependency on SvelteKit or build-host integration.
Native and WASM interfaces call the same Rust implementation where supported.

| Crate | Responsibility |
| --- | --- |
| `ts_svelte` | Documents, UTF-8/UTF-16 positions, copied-span mappings and compatibility profiles |
| `ts_svelte_compiler` | Component/rune-module compilation and checking projections; native/WASM |
| `ts_svelte_mapping` | Checker diagnostics, editor responses and source-map translation |
| `ts_svelte_check` | Explicit-project checking through the unchanged TypeScript core |
| `ts_svelte_semantic` | Persistent TypeScript Session over a supplied filesystem; native/WASI |
| `ts_svelte_tools` | Native lint, fixes, syntax features, persistent LSP and pure WASM project frontend |
| `ts_svelte_format` | Isolated Rust formatter and native/WASM JSON protocol |
| `ts_svelte_preprocess` | Native Markdown/Svelte preprocessing and exact source mappings |
| `ts_svelte_transpile` | Native TypeScript syntax transformation for executable modules |
| `ts_svelte_css` | Native CSS transformation, dependency linking and source maps |
| `ts_svelte_bundle` | Native Rolldown graph with Svelte/preprocessing/CSS plugins |
| `ts_svelte_kit` | Versioned Kit configuration, route types, environment/build generators and adapter contracts |
| `ts_svelte_build` | Native server-first framework build orchestration; incomplete phases remain explicit |
| `ts_svelte_compress` | Framework-independent pure Rust gzip/Brotli codecs and WASM interface |
| `ts_tailwind` | Separate Tailwind compiler implementation in development; no completion claim |

`npm/svelte` exposes the compiler, document tooling, formatter, semantic host and
composed project interface. The project interface owns a Svelte frontend worker
and persistent TypeScript semantic worker, synchronizing complete snapshots
before publishing new mappings. Both Node and browser synthetic component cases
have passed. Native LSP likewise keeps its TypeScript service alive across edits
and reloads Kit-generated route inputs after watched changes.

`npm/svelte-kit` retains the earlier explicitly hybrid adapter: installed Kit/Vite
orchestrate the build while Rust compiles components and rune modules. Its saved
application results do not prove completion of the newer native build pipeline.
Native orchestration lives separately in `ts_svelte_build`, using the bundler and
pure Kit generators as libraries. Framework metadata must describe real emitted
artifacts; unexecuted prerendering, unsupported transforms and unknown runtime
metadata remain requirements, not successful empty outputs.

The compiler, formatter, bundler and Kit packages use separate Cargo workspaces
to keep independently pinned OXC dependencies out of the TypeScript core. The
formatter has a different OXC source revision and therefore crosses the native
editor boundary through its executable JSON protocol. Svelte/Kit changes do not
modify the TypeScript core or its existing WASM ABI.

Earlier application acceptance remains in [application-results.md](application-results.md).
Current implementation checks are tracked separately in `state.json` under
`activeDevelopment`. Full native SvelteKit, Tailwind, lint/formatter ecosystem
parity and performance improvements have not been established.
