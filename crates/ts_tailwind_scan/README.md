# Native Tailwind snapshot extraction

`scan_sources(&[SourceSnapshot], &ScanOptions)` scans explicit UTF-8 source
snapshots using the official Tailwind CSS **4.2.1** Oxide Rust machines. It returns
candidate-looking tokens and custom-property occurrences, each grouped by source
identity/version. Candidates are not a claim that a utility will compile.
`scan_json` exposes protocol 1 using the same types and errors for native or WASM
hosts. No JavaScript implementation or subprocess is involved.

The core is sequential and uses no filesystem, network, environment, discovery,
or host callbacks. `.svelte`, HTML, JavaScript, TypeScript, Markdown, CSS and all
other upstream preprocessing extensions follow the pinned algorithms. Extensions
are explicit, case-sensitive and omit the dot. By default CSS contributes only
custom properties immediately used in `var(...)`, as in Oxide's discovered-file
scanner; explicit `Candidates` mode scans CSS as supplied content instead.

Results preserve distinct occurrence positions and input source order. Candidate
and custom-property unions are sorted and deduplicated. Ranges are half-open UTF-8
byte offsets. The scanner splits preprocessed content on newline bytes, matching
upstream. It reports processed positions separately from original positions.
Unchanged content and unchanged tokens in proven offset-preserving Svelte,
Markdown and JSON preprocessors receive exact original ranges. Other rewritten
inputs report `Unavailable`; equal lengths alone never establish a source map.

Input count, source/line/total bytes, occurrence count and extracted text bytes
have explicit limits. Embedded-language preprocessing additionally has a depth
limit of 64 and a cumulative work limit of 64 MiB per source. Exceeding a limit
returns an error without a partial success. These are deterministic size/work
bounds, not a hard wall-clock or allocator isolation guarantee.

## Upstream preservation

`vendor/` contains unchanged official Rust sources, test fixtures and MIT license.
`reference/vendor-sources.json` records the pinned Git tree, per-file Git blob and
SHA256 hashes. `reference/scanner-mod.rs` preserves the original scanner source
for the extracted dispatch/newline/CSS-mode behavior. The custom manifest for the
classification procedural macro pins build dependencies while compiling the
unchanged vendored macro. Runtime dependencies are `bstr`, `regex`, and serde;
there are no walkdir/rayon/ignore/filesystem scanner dependencies.

Upstream correctness tests remain compiled. Their existing ignored performance
tests remain ignored; do not run `--ignored` as a correctness check. Snapshot API
tests cover actual required source formats, UTF-8 provenance, CSS mode, ordering,
limits and JSON protocol. No native or WASM results are claimed until root runs
the respective checks. Format only `src/*.rs`; reformatting vendor files would
invalidate their byte-for-byte provenance.
