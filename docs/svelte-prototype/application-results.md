# Full builds, native lint and persistent diagnostics

Batch `svelte-application-4`; source
`66fcdaae71d772d928246a2d1f5a11331c3b9ff5e647f8e25f685588e0c51ae8`.
Accepted as a bounded prototype after independent reviewer and regression-auditor PASS.
[Verdicts](evidence/batch-4-run-1/verdicts.json) bind this exact source.

## Separation

[Architecture](application-architecture.md) defines the crate boundaries:
`ts_svelte_compiler` owns Svelte code generation and checking projections;
`ts_svelte_tools` owns lint and persistent diagnostics; `ts_svelte_kit` owns
framework build integration. The compiler and tools do not import the Kit plugin.
`npm/svelte-kit` is separate from the JavaScript/WASM API in `npm/svelte`.
The existing TypeScript checker implementation is unchanged.

## Full application builds

Both official and Rust-backed MoncuraHR production builds completed, producing
client assets and a Node adapter server. Source, binary and original application
input hashes stayed unchanged in both final runs. The Rust trace records 5,722
successful native compilation requests: 2,698 component requests and 163 rune
module requests for each of client and server. All 24 application rune modules
appear in the trace. There is no fallback to JavaScript Svelte compilation.

The browser comparison passed `/health`, `/login`, `/oauth/login` and `/docs`,
with matching statuses/text, no JavaScript exceptions or failed local assets,
and password-visibility state changes after hydration on the login surfaces.
The OAuth route correctly returned 400 without a challenge; docs redirected to
login. These probes do not cover authenticated HR workflows or backend services.

The build is hybrid: Kit routing/types, preprocessing, TypeScript stripping,
Vite bundling, Tailwind and adapter-node still execute in JavaScript. The build
adapter validates MoncuraHR's Kit 2.69.2 and vite-plugin-svelte 6.2.4 setup;
version-profile tests do not establish full builds for every Kit version.

Final build evidence: `target/svelte-application-official-6`,
`target/svelte-application-native-6`, and `target/svelte-application-browser-5`.
Build durations are recorded for reproducibility, but concurrent host work means
these runs cannot establish a speedup or resource reduction.

## Native lint and persistent LSP

Native lint completed all 2,392 MoncuraHR source files, with 77 findings:
68 `eqeqeq`, eight `state_referenced_locally`, and one
`a11y_no_static_element_interactions`. No files were reported incomplete.

The native linter combines Svelte compiler/accessibility/CSS diagnostics with
AST-based `no-debugger` and `eqeqeq` checks. This is a named rule subset, not full
ESLint, oxlint or Prettier configuration parity.

The stdio LSP maintains document snapshots, handles UTF-16 incremental edits,
push/pull diagnostics, configuration changes, saves and closes. Explicit-project
semantic checks overlay unsaved files and refresh dependent components. A fresh
Rust checker process performs each semantic check; the LSP process persists,
but semantic checker state is not yet incremental. Compiler/lint sessions also
run through JavaScript and isolated WASM workers.

Project discovery, generated Kit route/environment types, preprocessing in the
semantic graph, full MoncuraHR semantic checking, and editor completion/hover
remain incomplete. Checker failure publishes an explicit unavailable diagnostic.

## Verification

| Suite | Retained | New passing | Lost / absent / unrun |
| --- | ---: | ---: | --- |
| Rust release tests | 40 | 5 | 0 / 0 / 0 |
| JavaScript/WASM tests | 59 | 5 | 0 / 0 / 0 |
| Component compiler comparisons | 1,340 | 0 | 0 / 0 / 0 |
| Explicit checker/project cases | 10 + 10 | 0 | 0 / 0 / 0 |
| Version compiler/checker/controls | 792 + 64 + 50 | 0 | 0 / 0 / 0 |
| Version runtime scenarios | 19 | 18 | 0 / 0 / 0 |

Recovered baseline failures: zero; the accepted baseline had no failures in these
suites. The 37 runtime scenarios passed 148 cross-hydrations and 74 fresh mounts.
The original four hydration checks and explicit development rejection also pass.
New protocol checks passed 9/9, semantic LSP 8/8, and checker failure recovery 3/3.
The persistent session passed 1,200 edits with constant sampled RSS of 16,986,112
bytes from edit 100 onward, and document/cache cleanup. This small session probe
is not a whole-project memory or latency benchmark. The Kit crate has zero Rust
unit tests; its separate JavaScript host integration test passed.

Formatting, Clippy and TypeScript declaration checks passed. All 505 frozen
source entries, 16 saved artifacts and 17,986 pinned oracle file identities are
bound to the records. Earlier failing and exploratory runs remain preserved.

Evidence: [verification](evidence/batch-4-run-1/result.txt),
[quality](evidence/batch-4-quality-4/result.txt),
[browser comparison](evidence/batch-4-builds/browser/report.json).
The saved source and binaries are under `target/svelte-application-candidate-4`.

Upstream Go-port gates and ordinary Query/Hono were unrun in this additive
prototype batch. No local completion or regression result is claimed for them;
the inherited historical state remains unchanged. Full `svelte-check` diagnostic
parity and compiler support outside the exact pinned profiles are not established.

## Reproduction

Use the toolchain documented by the existing build scripts and installed pinned
oracles. All Cargo invocations go through `scripts/run-cargo-capped.sh`.

```sh
scripts/svelte/verify-application.sh target/NEW_VERIFY /home/yycholla/Documents/MoncuraHR
scripts/svelte/quality.sh target/NEW_QUALITY /home/yycholla/Documents/MoncuraHR
target/release/svelte-lint /home/yycholla/Documents/MoncuraHR/src 5.53.11 --runes
target/release/svelte-lsp
```

The build and browser helpers print usage with `help`. Build into isolated copies
using `scripts/svelte/application-build.py`; preserve the original application.
See [tooling configuration](../../crates/ts_svelte_tools/README.md) for explicit
project initialization and [Kit host](../../crates/ts_svelte_kit/README.md).
