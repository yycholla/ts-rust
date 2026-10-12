# Native Kit bundler plugin

This crate connects the pure `ts_svelte_kit::build_remote` transform to published
Rolldown **1.2.13**. It depends on neither the Svelte component compiler nor
`ts_svelte_bundle`. The build orchestrator supplies resolved configuration and
inserts `RemotePlugin` after component preprocessing using the generic bundler's
`generate_with_plugins` API.

```rust,ignore
let plugin = RemotePlugin::new(options)?;
let state = plugin.state();
let bundle = ts_svelte_bundle::generate_with_plugins(request, vec![plugin.into_plugin()]).await?;
let facts = bundle.chunks.iter().map(|chunk| EmittedFacade {
    facade_module_id: chunk.facade_module_id.clone(),
    file_name: chunk.file_name.clone(),
    exports: chunk.exports.clone(),
}).collect::<Vec<_>>();
let remotes = state.emitted_chunks(&facts)?;
```

`RemotePluginOptions` requires exact registered Kit version, absolute normalized
POSIX root/module identities, graph target, remote feature flag and module
extensions. Kit3 remote files inside `node_modules` require explicit per-module
package-peer evidence. The plugin performs no package/filesystem/config lookup.
`typescript` is an explicit resolved native lowering policy, not an implicit
second tsconfig discovery mechanism.

`seeds` force known client-only remote modules into the production server graph.
The plugin emits their namespace virtual entries during `build_start`; the host
need not add duplicate ordinary source entries. Other remotes are discovered
from real loaded modules during `transform`. Hash collisions, conflicting source
snapshots, unknown virtual entries and missing emitted facades fail explicitly.
Each plugin instance is for one build, including one target; create another
instance for the client phase or a subsequent watch rebuild.

All TypeScript/TSX remote sources are lowered natively before Kit appends its
server instrumentation. Existing JS from earlier component/rune transforms is
not recompiled. Returned maps address the current hook input. Earlier load maps
remain in Rolldown's map chain and must not be passed back as already-combined
maps. Generated client proxies return an empty source map when mapping is enabled,
so they cannot inherit stale mappings into private server source.

Retain `RemoteState`: `records()` gives deterministic hash-ordered actual module
identities, transform status and emitted references; `diagnostics()` retains
native preprocessing details and framework failures, including failed bundles.
`emitted_chunks()` accepts actual chunk facade/name/export facts and returns
original source identities plus actual output filenames. It does not invent
`chunks/remote-HASH.js` paths. Server facade chunks must retain their default
namespace export; emitted entries explicitly request strict signatures.

After server generation, use those actual filenames in the SSR manifest's remote
loader map. Initialize the native framework runtime, analyze routes, then analyze
remote exports in that same session. Feed the evaluated results into client
`metadata`, keyed by remote hash, preserving export order and the six registered
remote kinds. Missing or stale metadata is an error, even for otherwise valid
source syntax. Runtime `dynamic` values remain the orchestrator's responsibility
for subsequent prerendering/tree-shaking; this plugin does not execute handlers
or pretend that prerendered remotes have been removed.

The nine focused unit tests exercise the native transform stages, source maps,
identity state and facade mapping with synthetic inputs. They do not execute an
application or substitute for the orchestrator's full bundling integration tests.
