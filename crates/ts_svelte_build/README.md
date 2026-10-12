# Native SvelteKit build orchestration

`ts_svelte_build` joins the independent Kit generators and native Rolldown/Svelte
compiler plugin. It does not execute application modules, JavaScript config,
adapter JavaScript, or PostCSS. Tailwind compilation uses the separate native
compiler and host crates when explicitly configured. Native adapter-node packaging is a
separate Rust phase. It does not write output by default.

The implemented pipeline covers Kit 2 and Kit 3 split ESM with statically
resolved metadata and explicitly disabled prerendering:

1. Validate a resolved `build_plan::ResolvedNativeBuildInput` and generate the
   conservative server/client framework modules in memory.
2. Build the actual server graph with the native compiler, TypeScript lowering,
   extracted CSS, CSS URL asset linking, and source maps.
3. Check supplied analysis resolution targets against actual server graph edges.
   Analyze the original snapshots used by the build. Unknown runtime values stay
   unknown and prevent client optimization.
4. Generate the optimized client graph from the analyzed load flags and the
   actual server build identity; build it natively when CSR is enabled.
5. Run the shared framework guards on complete server/client module graphs,
   including modules removed by tree shaking and CSS virtual modules. Preserve
   diagnostic import chains and separate import/configuration/runtime statuses.
   Opaque externals produce an incomplete result; missing graph edges fail.
6. Finalize Kit 3 manifest variables and owned sentinel receipts using actual
   emitted client inventory and completed empty prerender results; update code
   and source maps atomically. Unknown results cannot finalize.
7. Derive node modules, stylesheets, fonts, dynamic dependency groups, preload
   metadata, and the SSR manifest from actual emitted chunk identities. Copy only
   explicitly inventoried static assets and public CSS assets to client output.
8. Build configured service workers with native graph guards and derived cache
   inventories. Package selected adapter-node output through native rebundling,
   source-map composition, gzip and Brotli compression.

`build(request).await` returns a `BuildReport`. `status: "complete"` means the
requested framework artifact phases completed, with no unresolved requirement;
no deployment or application runtime execution is implied. Completed server and
client phase artifacts are retained when a later stage fails or remains
incomplete. `serverBuildId` hashes the actual server result, not a caller claim.
The compiler and bundler remain framework neutral.

## Resolved request

The library accepts `BuildRequest` with these fields:

- `adapterNode`: optional resolved adapter-node 5.5.4 runtime templates, options,
  package metadata and `serverMapBase` (`projectRoot/dist` for this emitter).
  Selecting the adapter requires these inputs; templates are authenticated.
- `serviceWorkerOptions`: optional static-file filter decisions, original static
  inventory order and code-splitting override. Hashed cache entries derive from
  the actual client build rather than caller guesses.
- `tailwind`: optional native CSS entry configuration passed to both server and
  client graphs: `entries`, optional discovery `root`, native `plugins`, CSS
  `aliases`, `modulePaths`, and resource `limits`. JavaScript plugins/configuration
  are not executed. File assets and maps retain their original imported bases.
- `kit`: the existing `ts_svelte_kit::build_plan::ResolvedNativeBuildInput`. Leave
  `clientAnalysis` absent; this pipeline derives it from its own server build.
- `compiler`: `svelteVersion`, optional `typescriptVersion`, optional `runes`,
  `minify` (false), `sourcemap` (true), `options` (compiler options), optional
  `tsconfig`, optional resolved `mdsvex`, and native `typescript` transform
  options, plus `allowPrerelease` (false unless explicitly selected). Unknown
  fields/options are errors.
- `moduleExtensions`: required resolved Kit module extensions, including dots
  (normally `[".js", ".ts"]`). These are configuration facts, not inferred from
  whichever files happened to be loaded.
- `externalExposure`: optional exact external-ID map with tagged classifications:
  `{"kind":"public","evidence":"..."}`, `{"kind":"serverOnly"}`, or
  `{"kind":"unknown"}`. No package receives public status automatically.
  `node:` builtins cannot be reclassified public; resolve a real browser
  replacement instead. Unspecified externals remain unknown.
- `guardRealPaths` and `guardResolutions`: optional explicit canonical path and
  original resolver/span observations. Native filesystem observations must agree
  with supplied realpaths. Resolver spans are never fabricated from module IDs.
- `modules`: absolute module ID to `{source, imports}` for every universal,
  server and endpoint module and its statically analyzed dependency snapshots.
  `imports` maps the original specifier to its resolved module ID. Generated
  `$env/static/public` and `$env/static/private` imports are filled from the
  exact native server inputs and checked against the actual loaded graph. Other
  mappings are explicit host resolution facts, checked against direct edges;
  the host must not swap specifier identities that happen to share a target set.
- `sources`: optional additional absolute component/Markdown source snapshots.
  Conflicting snapshots and collisions with framework-generated files fail.
- `staticAssets`: relative public filename to `{source, mimeType}`, where source
  is base64 bytes. `staticAssetsComplete` explicitly attests that this is the full
  static asset inventory, including when it is empty. No implicit scan occurs.

The installed project and dependency files are read by the bundler for sources
not overlaid by snapshots. Every framework module source remains in memory;
there is no temporary mutation of the application checkout. Environment values
come only from the resolved Kit request.

The CLI reads one JSON request from stdin, or newline-delimited requests with
`--serve`:

```json
{"op":"capabilities"}
```

Build shape (replace `resolved_request` with the object described above):

```text
{"op":"build","request":resolved_request}
```

The response contains `ok` and `report`. All artifact bytes use base64, with keys
relative to the output root (`server/...` and `client/...`). Generated sources,
compiler diagnostics, full bundle metadata, unresolved requirements, and phase
outcomes remain separately inspectable. The single-request process exits 1 for
incomplete/failed builds. Requests are bounded to 96 MiB.

An explicit `outputDirectory` absolute path requests materialization only after a
complete build. The directory must not already exist, and its parent must
exist. `write_new` validates and decodes all paths/bytes before creating it;
existing outputs are never replaced. An I/O failure can leave the newly created
partial directory for inspection. Returned artifact generation and filesystem
materialization are separate APIs.

## Current boundaries

Single/inline builds, hash-router builds, full prerender execution and adapters
other than adapter-node remain explicit requirements. Remote modules use the
separate `ts_svelte_kit_bundle` native plugin: server wrappers, emitted facade
identities, source-bound native metadata, and metadata-driven client transforms.
Remote prerender exports remain incomplete until their inputs and responses have
actually been generated. Instrumentation ordering, service-worker binary asset URLs and
Kit 3 worker manifests have native implementations. Kit 3 instrumentation with
adapter-node 5.5.4 remains unsupported by that adapter contract. The native Kit feature transform collects retained markers after tree shaking,
removes their executable calls and updates emitted source maps. Per-chunk labels
are returned in `trackedFeatures`. Dynamic schema/configuration/evaluation needs
are never replaced with guessed metadata. CSS inlining uses the default zero
threshold; there is no supplied nonzero threshold to silently discard.

This orchestration crate targets the native filesystem host. Its component
compiler and pure Kit generator dependencies separately expose WASM interfaces;
the filesystem/Rolldown host is not advertised as a browser WASM build pipeline.
Full SvelteKit parity and performance claims require additional implementation.

Focused tests cover output path/conflict validation, immutable source overlays,
static versus dynamic emitted graph edges, absent versus empty CSS metadata,
CSS URL font origins, dynamic public environment identity, and rejecting
non-JSON function values as executable metadata. Additional async cases generate
real native bundles before validating CSS virtual traversal, dead private
imports, external classifications, exact environment aliases, static environment
analysis and dynamic-value uncertainty.

An ignored full-pipeline fixture test requires the explicit
`TS_SVELTE_BUILD_KIT_FIXTURE` variable pointing to a read-only project containing
Kit 2.69.2 and Svelte 5.53.11 dependencies. Run that test with `--ignored`; it
creates a separate tiny application, shares dependencies by symlink, and checks
server → analysis → optimized client → manifests. It requires a complete report with no outstanding requirements. No application or
configuration JavaScript is executed. Root owns Cargo execution.

The ignored Kit 3 full-pipeline fixture takes `TS_SVELTE_BUILD_KIT3_FIXTURE`
pointing to installed Kit 3.0.1/Svelte 5.57.2 dependencies. It exercises actual
`$app/manifest` references, the fixed client manifest chunk, source-map updates
and completed all-CSR-false output. The Kit 2 fixture also packages adapter-node
5.5.4 and checks gzip/Brotli artifacts. These tiny development fixtures do not
establish full application parity or performance.

When static analysis needs execution, the report retains an isolated preliminary
server manifest and an `executionSnapshot`. Its `bootstrap` contains actual
emitted ESM, exact import resolutions, explicit environment values and server
asset bytes for the separate `ts_svelte_runtime` host. Potential external or
computed import requirements are reported without inventing module resolutions.
Preparing this snapshot does not execute it or complete runtime analysis.

`build_with_runtime(request, evaluator)` continues the build using actual server
metadata. The native CLI accepts an explicit `runtimeBinary` absolute path to
this project's `svelte-runtime` executable. Its process transport initializes
the emitted snapshot, evaluates route metadata, disposes the session, and
validates every returned route/node against the emitted build identity before
client bundling resumes. `resolvedEntries` contains evaluated entry URLs
separately from the exported callback markers. Without an evaluator, `build`
retains its static-only behavior and reports dynamic requirements.
