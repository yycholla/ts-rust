# Svelte / MoncuraHR prototype

## Authorization and scope

The fork owner requested native Rust compilation, checking and LSP diagnostics
for MoncuraHR, then added on 2026-10-10:

> ts-rust should support wasm I think, we should support wasm and javascript.
> This is a slop/ai build prototype and thought experiment. I trust your judgement
> completely on this project as you will be the one touching it.

This follows the proposal to establish fork-local acceptance rules, preserve
upstream history, establish a fresh baseline, and add Svelte parity checks.
It authorizes the scoped process below; it does not make historical test results
local results or authorize publishing, production deployment, or changing
MoncuraHR's source to make a check pass.

## Current development direction

The user's latest instruction supersedes intermediate prototype audit and
benchmark requirements: continue to full parity without milestone check-ins;
benchmarking, user trials and independent audits are deferred until parity.
Implement features continuously with targeted correctness tests. Preserve the
accepted batch5 evidence and report unsupported behavior honestly. Root retains
integration/runtime ownership; separate production modules may be delegated.
No publication or original application mutation is implied.

## Local execution and acceptance

- Root owns production edits and the runtime queue. One independent reviewer and
  one regression auditor review each completed prototype batch. They do not edit
  production code or expectations. Their reports name the source and scope.
- Keep `docs/typechecker-state/current.json` unchanged. Append authorization notes
  through `scripts/state record note`. Prototype status lives in this directory.
- The inherited R188 artifacts and `target/read-production-window-v2.cjs` are
  absent in this clone. For prototype work, bounded source reads (at most 1,000
  lines per read, with current filesystem metadata) replace the missing guard.
  The missing historical evidence does not block additive prototype crates.
- Before changing TypeScript checker, loader, emitter or LSP behavior, establish
  a reproducible local baseline at the unchanged source, record toolchains and
  pins, and compare the affected upstream suites before and after. Until then,
  keep the existing TypeScript implementation and WASM ABI unchanged.
- New independent crates require release tests, formatting, lint, native/WASM
  cross-target validation and JS integration tests when relevant. Missing tools
  or unavailable runs must be reported as unrun, never passing. Focused results
  establish only the stated prototype scope, not Svelte or TypeScript parity.
- Keep the existing capped Cargo runner. Use local resources; inherited remote
  machines, credentials, push rules and workflow queues do not apply to this
  fork. Do not push or publish without a request. Preserve dirty work.
- MoncuraHR is read-only. Do not execute its configuration merely to inspect it.
  Baselines and deliberate-error cases belong in isolated fixtures/copies.
- Record unsupported features explicitly. No silent loss of diagnostics, stub
  `any` declarations presented as checking, or claimed speedups without measures.

## Targets

One Rust implementation should serve a native CLI/library and a WebAssembly
module callable from JavaScript/TypeScript in Node and browser workers. The JS
package is an interface to Rust/WASM, not a second Svelte implementation.
Application inputs include both JavaScript and TypeScript scripts, `.svelte`,
and rune modules (`.svelte.js` and `.svelte.ts`). Compiled application output is
JavaScript and CSS; compiling the tooling to WASM does not turn Svelte apps into
WASM applications.

Existing `ts_wasm` already provides TypeScript compilation/checking through a
host filesystem. Its documentation explicitly excludes LSP/watch and process
plugins. Browser LSP-like requests therefore need a persistent in-memory project
API and worker transport later, rather than launching a process content mapper.

## MoncuraHR inventory (read-only, 2026-10-10)

Checkout: `/home/yycholla/Documents/MoncuraHR`. Lockfile versions: Svelte 5.53.11,
SvelteKit 2.69.2, svelte-check 4.4.5, TypeScript 5.9.3, Vite 7.3.1,
vite-plugin-svelte 6.2.4, adapter-node 5.5.4.

Under `src`: 660 `.svelte` files, 24 `.svelte.ts`/`.svelte.js` files, 1,707 other
TypeScript files and one other JavaScript file. This is a filesystem inventory,
not a resolved project graph. Config forces runes mode and enables mdsvex for
`.md`; Vite also uses Tailwind and Sentry. Keep these requirements visible in
the eventual build target. TypeScript 5.9 and ts-rust's Go pin differ: classify
version differences explicitly rather than attributing all differences to Svelte.

## Milestones

1. Shared source-document and mapping foundation, callable natively and from JS
   through WASM. Correct UTF-8/Rust to UTF-16/LSP offsets, CRLF, incremental edits,
   and exact mapped diagnostic ranges. No Svelte checking claim at this stage.
2. Svelte parser and analysis matching the application's pinned compiler:
   script/module/style/template regions, runes, snippets and error recovery.
   A shared AST feeds checking and executable code generation separately.
3. Native checking transformation with virtual TypeScript/JavaScript and source
   mappings, plus compiler/a11y/CSS diagnostics. Compare with pinned Svelte tools.
   Mere script extraction is insufficient: template uses and component contracts
   must participate in checking.
4. SvelteKit project graph, route type generation, aliases, environment modules,
   and preprocessing contracts. Unsupported JS preprocessors remain explicit
   until ported or intentionally provided by the host.
5. CLI and persistent editor diagnostics over the same project snapshots; then
   hover, completion, navigation and rename. CLI parity does not imply LSP parity.
6. Client and server JS/CSS compilation, runtime/hydration tests, then the build
   orchestration required by MoncuraHR (bundling, mdsvex, Tailwind, instrumentation,
   asset handling and Node adapter compatibility).

## Evidence and performance

Pin the app snapshot, lockfile, Svelte compiler/tools, TS oracle and Rust source.
Compare diagnostics by file, code, severity, message and range; compare editor
responses after edits and repeated requests. Keep retained/recovered/lost/absent/
unrun results separate. For compilation, add SSR and browser hydration/interaction
tests; textually identical emitted JS alone is not the only correctness measure.

Measure cold checks, warm checks, edit-to-diagnostic latency, peak RSS, sustained
editor growth, WASM memory and load cost on the same host. Initial aspirational
targets are 2x faster checking and 50% less peak memory than the existing
MoncuraHR check, without diagnostics loss. They are not measured results. Native
and WASM targets need separate measurements.

## References

- [Svelte language-tools architecture](https://github.com/sveltejs/language-tools/blob/master/docs/internal/overview.md)
- [Svelte compiler API](https://svelte.dev/docs/svelte/svelte-compiler)
- [Existing WASM interface](../../npm/wasm/README.md)

The upstream language-tools architecture transforms components to virtual JS/TS
and maps positions; its checking representation is not executable application
output. The exact Rust integration points remain to be validated against this
checkout before altering TypeScript core code.
