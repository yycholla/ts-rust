# Native snapshot CSS pipeline

Pure Rust `bundle(&BundleInput)` uses exact `lightningcss = 1.0.0-alpha.72`.
The standalone `svelte-css` CLI accepts JSON on stdin; `--serve` processes
newline-delimited requests persistently. `--help` documents usage. Neither the
library nor CLI reads source/config files or executes JavaScript. Sources and
all stylesheet resolutions are explicit immutable input snapshots.

```json
{"op":"bundle","input":{"entry":"/src/app.css","sources":{"/src/app.css":"@import './base.css'; .logo {background:url('./logo.svg')}","/src/base.css":"body {margin:0}"},"imports":[{"from":"/src/app.css","specifier":"./base.css","target":{"kind":"file","path":"/src/base.css"}}],"assets":[{"from":"/src/app.css","url":"./logo.svg","path":"/src/logo.svg"}]}}
```

Import targets can instead be `{"kind":"external","url":"https://..."}`.
Missing resolutions fail; even an absolute filesystem path cannot bypass the
snapshot. Paths use normalized absolute POSIX spelling, making native/host
identity explicit. Source limits are 4 MiB per file, 32 MiB total, 4096 files,
32768 resolutions of each kind, and 64 MiB per protocol request.

Lightning CSS owns import ordering, repeated imports, layer/media/supports
wrappers, CSS modules and composition. Options are `minify`, `sourceMap`
(default true), `projectRoot`, `cssModules` (object with `pattern`,
`dashedIdents`, `pure`), and `targets` (browser names mapped to packed 24-bit
major/minor/patch versions). Unknown fields/targets fail. CSS modules apply to
the whole bundle; bundle ordinary Svelte scoped CSS separately from module CSS.
Extracted Svelte CSS should retain its original component path as source identity.

Results include `code`, source-map JSON, CSS-module `exports`/`references`,
`sources`, used `resolvedImports`, and detailed `dependencies`. Each dependency
retains the original URL and source filename. URL dependencies optionally carry
`resolvedPath`; absent relative paths are explicitly `requiresHostResolution`.
Absolute/data/fragment URLs and external imports carry `resolvedUrl`.

`bundle` returns a linking intermediate. `link(&BundleOutput, &[AssetLink])`
produces final CSS and a corrected source map. The JSON operation is:

```json
{"op":"link","bundle":"<the complete bundle result object>","links":[{"from":"/src/app.css","url":"./logo.svg?raw#icon","emittedUrl":"../assets/logo.123.svg"}]}
```

Replace the illustrative `bundle` string with the actual object. Each link uses
the original stylesheet's absolute `from` path and complete decoded `url`.
`emittedUrl` is chosen by the host after emitting the asset, already relative to
the final CSS file or rooted at its public URL base. It must exclude literal
query/fragment delimiters: the linker appends the original URL's `?`/`#` suffix
verbatim. Percent-encoded delimiters remain part of the path. No filesystem
lookup, hashing, asset emission or implicit directory rebasing occurs here.
Assets from nested imports retain their own original stylesheet identity.

Every non-external URL dependency requires exactly one link per `(from,url)`
identity; repeated occurrences consume that link repeatedly. External imports,
absolute/public, data and fragment URLs are restored from dependency metadata
without a supplied link. A host can explicitly classify an otherwise absolute
asset as locally resolved using `assets` during bundling; it then requires a
link. Missing, duplicate, unused and conflicting links fail atomically.

The linker parses CSS tokens and replaces only tracked URL/import tokens.
Matching text inside ordinary strings and comments stays unchanged. It escapes
output URLs as CSS strings and verifies every placeholder occurrence against
metadata. Final results include `replacementCount`, `consumedLinks`, updated
`dependencies`, CSS-module `exports`/`references` and `requiresAssetLinking:false`.
No partially linked output is returned on error.

Source maps use UTF-16 generated and original columns. A vendored printer fix
accounts for supplementary Unicode and escaped CSS-module identifiers. The
intermediate carries `mappingEncoding:"utf16-lightningcss-alpha72-patched"`;
link rejects other encodings rather than misinterpreting earlier buggy maps.
The linker shifts surviving mappings, marks replacement text as generated and
restores the preceding mapping at each replacement's end. Source/name tables,
source content and other map fields remain intact. Maps address the supplied
CSS snapshots; composition back through preprocessed Svelte is a host step.
Native dependency location metadata is preserved without asserting that its
backend byte-length-derived end coordinates are LSP ranges.

Tailwind directives/functions, including escaped at-keywords, fail explicitly;
comments and string literals containing those words remain valid. This crate
does not implement Tailwind utility generation, Sass/Less preprocessing,
PostCSS plugins, filesystem discovery, asset emission, browser loading or
Rolldown chunk/CSS association. Those remain separate host/plugin stages.

The library has no JavaScript runtime dependency. This initial crate is native;
the upstream bundler feature includes Rayon, so WASM bundler support is not
claimed without a separate target implementation/check. Root owns capped Cargo
builds. Tests cover import wrappers/order, URL origins, cross-file CSS-module
composition, external imports, strict configuration and unsupported directives.

Lightning CSS is vendored at its exact published version under
`vendor/lightningcss`, with its MPL-2.0 license and a `PATCHES.md` source/change
notice. The direct path dependency keeps the printer patch active when this
crate is embedded in another workspace. `parcel_sourcemap` is an MPL-2.0
dependency. Modified Lightning CSS source remains under MPL-2.0.
