# Rust JavaScript runtime bindings

Build with `scripts/js-runtime/build.sh`. The generated browser module exports
`js_runtime_json` over the same persistent Rust protocol as native `js-runtime
--serve`. Initialize the wasm-bindgen module, then pass JSON requests for
`create`, `evaluate`, `readExport`, `invokeExport` and `dispose`.

Run evaluation inside a dedicated worker when integrating an editor or browser
application: execution is synchronous at this boundary. A worker/module instance
owns its sessions. Explicit module source and resolution snapshots provide all
code; filesystem, environment, network and SvelteKit APIs are not installed.

This package is separate from the Svelte compiler and framework integration.
The Rust runtime README documents execution limits and supported result values.
