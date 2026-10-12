# Framework-independent Svelte tooling

This crate owns native lint, persistent document sessions and a stdio LSP server.
It depends on the Svelte compiler; it does not import the SvelteKit plugin.

- `svelte-lint PATH [SVELTE_VERSION] [--runes|--legacy]`: recursive native lint with compiler/a11y/CSS
  diagnostics plus default AST-based `no-debugger` and `eqeqeq` script rules. Exit 1 means
  diagnostics, 2 means incomplete lint. This is not oxlint/ESLint/Prettier parity.
- `svelte-service`: persistent newline-JSON document sessions and lint.
- `svelte-lsp`: Content-Length JSON-RPC over stdio, UTF-16 incremental edits,
  compiler/lint push and pull diagnostics, configuration invalidation and close
  cleanup. Only implemented capabilities are advertised.
- `ts-rust-svelte/tooling`: the same document-session/lint API through Rust/WASM
  in an isolated disposable worker. It does not run the native project checker.

LSP `initializationOptions` accepts `compatibility`, `runes`, and optionally an
explicit `project` with `root`, `dependencies`, `components: [{path,typescript}]`,
`files: [path]`, and `runes`. Component/file paths are relative to `root`.
The server combines unsaved documents with disk inputs and updates a persistent
native TypeScript project after changes, saves and closes. This refreshes consumer
diagnostics without edits to consumers. A sibling `svelte-ts-service` process owns
the reused TypeScript Session; `svelte-project` prepares the checking overlay once
per project generation. Project discovery and generated framework types remain
the plugin's responsibility.
Generated/unmapped diagnostics remain visible at line zero with explicit metadata.

With a project, installed dependency versions and `project.runes` are the single
configuration authority. Conflicting session settings are rejected; dependency
version changes require project rediscovery or reinitialization. Encoded file URIs resolve to editor
buffers, with one open URI per canonical source. Inputs must remain within the
project root; reserved paths, duplicates and symlink escapes are rejected.
Checks allow at most 1 MiB per source and 64 MiB per serialized project request,
64 MiB of checker output, 30 seconds for preparation and 60 seconds per semantic
request. Incomplete checks publish an
explicit diagnostic. Project diagnostics publish together after checking, without
an intermediate publication that clears type errors. Pull results omit result IDs
because dependency changes can affect documents whose versions did not change.

A framework plugin can launch `svelte-lsp --project-config FILE` with the same
generic project contract. Optional snapshot guards bind generated declarations,
configuration, dependency versions and directory rosters. An optional
`projectProvider: {executable, arguments}` names a host-selected native discovery
command returning the same explicit JSON contract. Watched-file notifications
rediscover through that command, validate its root and settings, then replace the
semantic project while retaining open buffers. Failed rediscovery publishes an
incomplete-check diagnostic and prevents stale semantic answers. Without a
provider, stale generated inputs require sync and reinitialization.
The Kit-specific discovery and launcher live in `ts_svelte_kit`.

Project editor methods include hover, completions/resolve, definitions,
references/rename, signatures, highlights, symbols, selection/folding ranges,
semantic tokens, import code actions, inlay hints and linked editing. Copied
projection spans and transpilation source maps retain original coordinates;
unmapped generated edits are rejected atomically. Native template syntax provides
linked tags, symbols, folds and selections even without a TypeScript project.

JSON lint requests and session configuration accept `lintConfig: {rules: {...}}`.
Enabled rules/options are validated against the implemented native registry;
unavailable type-aware/filesystem/framework contexts fail explicitly. Original
source fixes and suggestions feed `sessionCodeActions` and LSP quickfix /
`source.fixAll.svelte` actions. Fixes validate current versions and coordinates;
conflicting fix groups are excluded from fix-all. These additions are development
work, not a claim of complete ESLint or Svelte language-server parity.
