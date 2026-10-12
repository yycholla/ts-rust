# Version compatibility prototype

Status: `svelte-versions-3` is accepted for the bounded scope below. Both the
independent reviewer and regression auditor passed the exact source. The
[accepted state](state.json) preserves the previous baseline and all attempts.

The compiler now selects an explicit, immutable compatibility profile shared by
native Rust and the JavaScript/WASM interface. Unknown versions, dependency
ranges and mutable tags fail explicitly. Omitting a profile keeps MoncuraHR's
Svelte 5.53.11 default. The native explicit-input checker reads actual installed
package versions from the supplied dependency directory.

## Version window

Registry discovery on 2026-10-10 is preserved in
[`version-discovery-1`](evidence/version-discovery-1). The window starts two
minor lines behind MoncuraHR's Svelte 5.53 and SvelteKit 2.69.

| Package | Exact profiles |
| --- | --- |
| Svelte | 5.51.5, 5.52.0, 5.53.5, 5.53.11, 5.53.13, 5.54.1, 5.55.10, 5.56.10, 5.57.2 |
| SvelteKit | 2.67.0, 2.68.0, 2.69.2, 2.69.3, 2.70.3, 3.0.1 |
| Experimental Kit | 3.0.0-next.32, with `allowPrerelease: true` |
| TypeScript comparison toolchains | 5.9.3 and 6.0.3 |

The discovery snapshot lists Svelte 5.57.2 and Kit 3.0.1 as stable latest.
Kit's `next` tag points to an older pre-3.0 release; it is not a newer beta.
There was no Kit beta tag. Svelte's stale 5.0 prerelease is outside this window.
Exact pins are deliberate: these results do not certify every intervening patch.

The earlier public-project comparison used Sonner with Svelte 5.43.5 and Bits UI
with 5.46.4. Those installed versions fall below this window. Their saved
[comparison results](public-project-comparison.md) remain separate evidence;
this batch does not extend support to those older toolchains.

Kit 3 profiles require the registered Svelte 5.57.2 profile and, when specified,
TypeScript 6.0.3. Profile metadata distinguishes Kit 2/3 configuration contracts,
library aliases and environment modules. It does not implement those contracts.
The Rust TypeScript engine and embedded libraries remain fixed; selecting a
comparison toolchain does not emulate an arbitrary TypeScript release.

Native `svelte-tool` and the JS compiler's `request()` method accept the same
profile query:

```json
{"op":"resolveCompatibility","versions":{"svelteVersion":"5.57.2","kitVersion":"3.0.1","typescriptVersion":"6.0.3"}}
```

For `compile` and `project` requests, supply those version fields under
`compatibility`. The response carries the resolved profile. Prereleases also
require `allowPrerelease: true`; they never replace the stable default.

## Behavior covered

- Version-aware parsing of declaration tags introduced in Svelte 5.56, including
  earlier-version error messages and UTF-8 diagnostic positions.
- Version-aware select `defaultValue` client and server emission at 5.57.
- Accessibility diagnostic wording across the 5.55 boundary.
- Runtime export validation against exact client/server manifests.
- Native/WASM request isolation and matching profile propagation.
- Native explicit-input checking with installed declarations, including
  TypeScript and JavaScript/JSDoc fixtures.

Client development instrumentation remains rejected on older profiles; the
latest profile has a bounded development-mode runtime comparison. The original
select rejection is replaced by stronger successful compilation, hydration,
mount, interaction and form-reset comparisons, with an explicit
[test mapping](version-test-mapping.json).

## Evidence and remaining scope

The first frozen run is preserved at [`batch-3-run-1`](evidence/batch-3-run-1).
It retained all baseline tests but failed 42 new matrix comparisons because
OXC's fatal recovery discarded the first declaration keyword. The repair
classifies only the leading token after JavaScript trivia, preserving Unicode
identifier boundaries and byte offsets. Added controls cover comments, all JS
line terminators, escaped identifiers, property names, strings and regex text.

The second frozen run, [`batch-3-run-2`](evidence/batch-3-run-2), recovered all
42 differences and retained every baseline pass. It matched 500/504 diagnostic
cases. Four newly exposed differences came from missing declaration
initializers on 5.56+: an infallible AST builder swallowed strict parse errors.
The next repair validates the declaration before either single/multiple-binding
builder and checks first-statement consumption. Additional controls cover
malformed later declarations, semicolons, automatic semicolon insertion through
comments, valid newline continuations, destructuring and TypeScript annotations.

The third frozen run, [`batch-3-run-3`](evidence/batch-3-run-3), recovered those
four differences and retained the baseline. It matched 732/774 diagnostic cases.
The 42 remaining differences exposed lost profile state during eager error
replay: a replacement parser defaulted to the newest declaration grammar.
The repair copies the selected grammar capability into that parser. An added
declaration-before-unclosed-HTML case checks version-aware error ordering.

All three failed runs remain unaccepted and preserved. The fourth frozen run,
[`batch-3-run-4`](evidence/batch-3-run-4), passes at source
`47a5db1946e372c5755cfaba98fdd00cc7460048121c263d0bec7835619a754d`.
[`batch-3-quality-7`](evidence/batch-3-quality-7) passes at the same source.

| Check | Result |
| --- | --- |
| Rust release tests | 40 passed: 34 retained, 6 new |
| JavaScript/WASM tests | 59 passed: 57 retained, 2 new |
| Retained compiler diagnostic comparisons | 1,340/1,340 matched, including 660 MoncuraHR components × two targets |
| Retained checker/project cases | 10/10 and 10/10 |
| Exact-version compiler diagnostics | 792/792 matched across nine Svelte pins |
| Explicit-input checker comparisons | 64/64 matched across sixteen installations |
| Profile and unsupported-input controls | 50/50 passed |
| Version runtime comparisons | 19/19 scenarios: 76/76 cross-hydrations and 38/38 fresh mounts |
| Original runtime controls | 4/4 hydrations and the retained development-mode rejection |
| Quality | Formatting, clippy and TypeScript declarations passed |

Source and binary identities are stable. All 17,986 recorded oracle package
files remain unchanged. The frozen source archive is included in the run.
No baseline tests were lost, absent or unrun. One previous select-rejection
control is explicitly mapped to stronger successful-behavior coverage.
The regression auditor and independent reviewer both passed this exact source;
their [verdicts](evidence/batch-3-run-4/verdicts.json) are preserved with the run.

These are bounded component/compiler and explicit-input checker profiles.
SvelteKit project discovery, generated route/environment types, preprocessors,
full application builds, native lint and persistent LSP are still pending.
Rune-module inputs remain explicitly unsupported. Checker oracle comparisons
use the same Rust projections on both sides; they are not full `svelte-check`
transformation parity. Runtime checks use JSDOM, not real browsers. No speed or
memory improvement is established by this batch. Inherited TypeScript gates,
Query and Hono remain unrun locally.

## Adding versions

Resolve release tags during setup, record exact package versions and lockfiles,
generate runtime export manifests, and compare diagnostics plus executable
behavior against each pinned official compiler. Update a shared profile only
after the matrix and retained baseline pass. Keep experimental releases opt-in
and promote them using the same checks. Package installation and recognizing a
Kit version alone do not establish framework compatibility.
