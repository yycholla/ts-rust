# Shared source-map composition

`ts_svelte_mapping` composes rune diagnostics through TypeScript transpilation
without depending on the Svelte compiler, checker or editor host. The native
project checker and persistent editor use the same implementation. It has no
filesystem/process APIs and is suitable for native and WebAssembly callers.

`map_diagnostic(&Snapshot, generated_text, map_json, diagnostic)` accepts a flat
version-3 map with `sourcesContent` matching the original snapshot. Successful
results retain generated coordinates under `generated`, replace original
`bytes`/`range`, and set `mapping` to `native-ts-transpile-mapped`. Generated or
ambiguous text stays visible with null original coordinates and
`native-ts-transpile-unmapped`. Malformed maps, stale contents and inconsistent
coordinates return an error that callers must preserve as incomplete analysis.

Mapping extends anchors only through identical bounded text or complete lexical
units. It does not guess locations from the nearest source-map point. Indexed
source maps are explicitly unsupported. The six focused tests cover Unicode,
parameter properties, helpers, multiline ranges and invalid maps.
