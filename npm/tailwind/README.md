# Rust Tailwind JavaScript interface

Build native and WASM artifacts using `scripts/tailwind/build.sh`.
Import the generated WASM module, initialize it with its WASM bytes, then call
`tailwind_json(JSON.stringify(request))`. Responses are JSON strings with `ok`
and `result` or `error` fields. Run compilation in a worker for responsive UIs.

Operations: `capabilities`, `prepare`, `build`, `buildProject`, `scan`, `dispose`.
`prepare` takes explicit CSS snapshots, import resolutions, plugin identities
and stylesheet options. It returns a session ID reused by `build` or
`buildProject`. Each build replaces the previous candidate/source set.
`buildProject` performs native source selection, extraction and CSS generation
over the supplied inventory. It performs no filesystem or network access.

All compilation executes in Rust. This package contains no JavaScript Tailwind
implementation. See the Rust crate README for version pins and remaining gaps.
