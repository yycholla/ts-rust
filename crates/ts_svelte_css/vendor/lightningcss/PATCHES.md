# Vendored source notice

Source: `lightningcss` 1.0.0-alpha.72, published by Devon Govett and Parcel
contributors, https://github.com/parcel-bundler/lightningcss. The source archive
is preserved in `target/svelte-css-upstream/lightningcss.crate` for this checkout.
The upstream MPL-2.0 license is preserved in `LICENSE`; modified source files
remain subject to MPL-2.0. This is a local dependency patch, not an upstream release.

The library source, package manifests and license/readme are retained. Upstream
CLI test/benchmark/example fixtures are not copied or claimed as locally run.

Local changes:

- `src/printer.rs`: count generated source-map columns in UTF-16 code units,
  matching cssparser's original coordinates and source-map consumers.
- CSS-module identifier serialization records the emitted escaped spelling's
  width, including escaped patterns and supplementary Unicode characters.

The enclosing crate tests the corrected printer and subsequent asset-link map
shifts. No CSS compilation semantics are intentionally changed.
