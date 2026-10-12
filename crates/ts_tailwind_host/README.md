# ts_tailwind_host

Native filesystem input layer for the pure `ts_tailwind` compiler. This crate is
separate from Svelte and from the filesystem-free native/WASM compiler API.

`load_project(&HostRequest)` resolves a CSS graph, prepares native directives,
walks the resolved source roots, and returns `HostProject`: `ProjectSnapshot`,
`StylesheetOptions`, normalized source metadata, `SourceInventory`, selection,
watch dependencies (including missing paths), and visible environment-file
omissions. `compile::compile_project(&request, &options)` composes discovery,
native scanning, compilation and maps to original stylesheets. Relative asset
URLs are bound to their originating stylesheet before imports and inline themes
move declarations. The result includes asset tokens, canonical paths, byte
lengths and SHA-256 receipts for the output linker. It never writes output.

Asset-bearing CSS has `requiresAssetLinking: true` until a bundler replaces the
tokens and emits verified asset bytes. Source maps leave rewritten URL interiors
unmapped; they retain original stylesheet content and source identities. Missing
used assets fail with watch information; unused theme assets are not read.

Inputs are an absolute project root, entry filename, explicit native plugin
registrations, ordered enhanced-resolve-compatible aliases, optional absolute
package lookup roots, and bounded resource limits. CSS resolution uses `style`
export conditions, `style` main fields, and `.css` extensions. It never consults
`NODE_PATH`, evaluates configuration, or executes JavaScript plugins. Unknown
plugins, `@config`, unsupported import loaders, missing paths, invalid ignores,
cycles, and resource limits return pathful errors with accumulated dependencies.

Source traversal uses Tailwind 4.2.1's exact ignore fork and exclusion fixtures.
It follows symlinks, scans hidden inputs, disables global gitignore rules, and
honors parent/repository ignore boundaries plus explicit source overrides.
Default discovery starts at the explicit project root. Explicit source patterns
can escape it or select ignored dependency directories. Negatives win. Only
actually reachable matching source files are read; the resulting inventory's
automatic eligibility reflects the combined walk's source whitelist precedence.

`.env` and `.env.*` are never read. Automatic omissions are listed in
`unreadEnvironmentFiles`; direct references fail. Source bytes use Node-compatible
UTF-8 replacement decoding, deterministic content-derived versions, and complete
SHA-256 identities. Call again after watched changes for a fresh coherent input
generation; no resolver cache survives a call. Concurrent changes detected while
reading a file cause an explicit retry error. This is not an atomic filesystem
snapshot across all files.

Canonical symlink identities are deduplicated. If a logical symlink source and
its canonical destination have conflicting source exclusions, loading fails
explicitly; preserving both requires per-alias selection metadata in the pure
source inventory contract. Import query/fragment loaders also remain explicit
unsupported inputs. Remote/data/URL imports stay in the CSS as upstream does.

The pinned vendored ignore crate retains its upstream licenses and source.
`reference/provenance.json` records hashes and upstream paths. No filesystem
discovery is exposed from the WASM compiler: JS/browser hosts supply snapshots.
