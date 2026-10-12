# Native Tailwind compiler

Pure Rust Tailwind **4.2.1** compilation over explicit snapshots, separate from
Svelte and filesystem discovery. The same implementation serves native Rust,
the `tailwind` process, and WASM callers.

`stylesheet::prepare_stylesheet(snapshot, options)` resolves supplied CSS import
edges and prepares themes, custom utilities, variants, CSS functions, `@apply`,
source directives and native plugins. Its immutable `build(candidates)` replaces
the candidate set on each call, so removing a class removes its generated CSS.
Output includes real CSS/AST, generated and invalid candidates, unsupported
operations, remaining compatibility passes and source metadata.

`project::compile_project` composes source selection, the pinned Rust Oxide
extractor in `ts_tailwind_scan`, and stylesheet compilation. Hosts supply explicit
canonical file identities and discovery facts (automatic eligibility, binary
classification, blocked ancestors). `source_selection` applies source roots,
positive/negative directives and pinned glob rules. It returns the actual
selected inputs and normalized watch patterns; it never silently scans every file.

The native plugin registry implements `@tailwindcss/typography` **0.5.19**. Supply
a `PluginRegistration` whose implementation is `@tailwindcss/typography`, version
is `0.5.19`, and specifier matches the CSS `@plugin` directive. `className` and
`target` options are supported. Other plugins and executable JS configurations
fail explicitly. Plugin callbacks are Rust functions stored separately from the
Svelte compiler; no Node or JavaScript engine is used here.

`tailwind --serve` accepts newline-delimited JSON; without `--serve` it accepts
one request. `protocol::Service` supports `capabilities`, `prepare`, `build`,
`buildProject`, `scan` and `dispose`. Prepared sessions are bounded to eight.
Requests/responses are bounded to 16 MiB. WASM exports the same persistent
protocol as `tailwind_json`; the stateless parser/compiler interface remains
available as `tailwind_foundation_json`.

Source offsets are UTF-8 bytes. `snapshot::position` converts exact boundaries
to UTF-16 positions. Oxide extraction reports original ranges only where its
preprocessor provenance establishes exact correspondence.

## Provenance and remaining work

MIT-licensed pinned sources, hashes and attribution live in `reference/`,
`typography_provenance.json`, and the scanner crate's vendor records. References
are not executed at runtime. Input/recursion/allocation limits are explicit.

The compiler lowers dynamic color-mix values to compatibility fallbacks and
feature-query overrides; constant color mixing is left to the CSS optimizer.
It does not claim arbitrary JavaScript plugin/configuration compatibility, full
CSS optimization/source-map parity, or support for unimplemented utility cases.
Unknown registrations never become invented successful CSS. Filesystem hosting
and bundler integration are separate work; the pure compiler performs no I/O.
