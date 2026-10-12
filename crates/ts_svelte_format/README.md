# Native formatter

`ts_svelte_format` formats Svelte 5 components, JavaScript/TypeScript (including
JSX/TSX and rune modules), CSS/SCSS/Less/PostCSS, and JSON/JSONC/JSON5 entirely in
Rust. It does not evaluate JavaScript configuration, load plugins, read project
files or execute external formatters.

The pure library API is `format(source, filename, &ResolvedOptions)` or
`format_document(snapshot, filename, &ResolvedOptions)`. A successful result has
the formatted `code`, `changed`, original `uri` and `version`, and either no edits
or one complete original-document replacement. Editor ranges use UTF-16; `bytes`
uses original UTF-8 offsets. Parse/option errors return no replacement edits.

The `svelte-format` CLI reads one JSON request from stdin. `--serve` processes
newline-delimited JSON requests persistently. `--help` prints usage. Neither mode
writes source files.

```json
{"op":"format","document":{"uri":"file:///Component.svelte","version":1,"text":"<h1>Hello</h1>"},"options":{"printWidth":80,"tabWidth":2}}
```

Responses are `{"ok":true,"result":...}` or `{"ok":false,"error":{"code":...,"message":...}}`.
`{"op":"capabilities"}` describes extensions and limitations. An explicit
`filename` can override language selection from the document URI. Source is
limited to 1 MiB; the request limit allows worst-case JSON string escaping.

On WebAssembly the `formatRequest(jsonString)` export uses the identical
protocol. A JavaScript host can call that export or the persistent native CLI.
No separate JavaScript formatting implementation is maintained.

Resolved options include `printWidth`, `tabWidth`, `useTabs`, `singleQuote`,
`semi`, `trailingComma` (`all`/`es5`/`none`), `quoteProps`
(`as-needed`/`consistent`/`preserve`), `arrowParens` (`always`/`avoid`),
`bracketSpacing`, `bracketSameLine`, `singleAttributePerLine`, `endOfLine`
(`lf`/`crlf`/`cr`/`auto`), `sortImports` (boolean), `svelteAllowShorthand`,
`svelteIndentScriptAndStyle`, and `svelteSortOrder`. Unknown options and invalid
values fail. Hosts must resolve overrides, ignore rules and presets themselves.
Options that apply to one language follow that language's formatter behavior.

This uses rsvelte revision `5ed8ea3a3401b9fbbe780d3b18d6f90073291d6b` and OXC
formatter revision `60fa13878c3808413268c462ddadbceaf71c38bb`. The standalone
workspace deliberately patches OXC parser/AST crates to the formatter's same
Git revision, matching rsvelte's upstream workspace. These patches must not
silently replace the checker/tools workspace dependencies. Keep integration
behind the JSON/WASM boundary until their dependency identities are reconciled.

Full Prettier/plugin parity is not established. Tailwind sorting, package.json
key sorting, range formatting and executable configuration are not provided.
Unsupported style languages (including indented Sass/Stylus) fail explicitly.
CSS parser failures propagate instead of returning an unchanged style body as
if formatting succeeded. The backend can conservatively preserve JavaScript
whose private-brand-check parentheses cannot be safely rewritten.

Root owns Cargo builds and target checks through `scripts/run-cargo-capped.sh`.
The native library tests cover supported language idempotency, option handling,
original UTF-16 edit ranges, BOM/line endings, errors and protocol behavior.
