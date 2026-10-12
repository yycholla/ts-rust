# Native Markdown / Svelte preprocessing

`preprocess(source, filename, &Options)` returns a generated Svelte component,
frontmatter metadata, a standard v3 source map with embedded source, and exact
byte-copy spans. Nonmatching extensions return `None`. No JavaScript
configuration, plugin, process, filesystem or environment access occurs.

The compatibility target is mdsvex 0.12.7, using the same native CommonMark/GFM
approach as the pinned rsvelte preprocessing module. This implementation adds
strict YAML error handling, timestamp/quoted scalar distinctions, aliases and
merge keys, safe metadata exports, Svelte source protection and source maps.
The Markdown parser is pinned comrak 0.55.0 with native GFM tables, autolinks,
task lists, strikethrough and configurable smart typography.

Use `options_from_json` for resolved mdsvex-shaped options. Unknown options and
active remark/rehype plugins, custom frontmatter parsers and layouts fail
explicitly. Default highlighting supports unlabelled/plain-text code, with
static escaped HTML equivalent in rendered content to mdsvex's raw-HTML
optimisation. Named Prism languages fail until native matching grammars exist;
callers may explicitly select `highlight: false`. Arbitrary Markdown nested
inside raw HTML remains unsupported. CommonMark and smart typography edge cases
have not been established as identical to remark 8/retext's complete behavior.

Only unchanged source receives exact mappings. HTML wrappers, metadata exports,
entity escapes, normalized code and smart typography are deliberately unmapped;
consumers must retain unmapped diagnostics rather than guess original ranges.
WASM compatibility is a design constraint, not a claim that a target build has
already been run. This crate has no browser or Node runtime dependency.
