# Native Kit project checking — batch 5

Status: accepted within the bounded scope below; independent reviewer and regression auditor PASS.

Accepted source: `8cd3f175d2c1573ba983de10f454a94740b5898eead7eb6091c35c586cbf0d22`.
Previous accepted source: application4 `66fcdaae71d772d928246a2d1f5a11331c3b9ff5e647f8e25f685588e0c51ae8`.

The Kit plugin discovers an existing synced project without executing application
configuration and emits generic checker/editor contracts. Svelte compiler,
checker and persistent tooling remain separate crates. The plugin can launch
native checking and a persistent stdio LSP directly.

The frozen candidate completes MoncuraHR checking with 3,082 roots matching the
official TypeScript graph, zero visible TypeScript errors and nine warnings
matching the official checker by file, code, severity, message and UTF-16 range.
All 3,296 native inputs match the request and official input hashes. One diagnostic
wholly inside a generated Svelte helper is preserved in `suppressedDiagnostics`;
copied source errors and other unmapped errors remain visible.

Five full-project LSP cases pass: initialize, open/check, detect an unsaved source
error, repair/pull, and reject a changed load-file snapshot. The editor process
persists; the semantic checker still runs anew for each check. The native check
stage took 15.70 seconds and the official whole check took 51.93 seconds. These
have different timing boundaries and do not establish a comparative speedup.

Final verification is `target/svelte-project-verification-2`; formatting, Clippy
and declaration checks are in `target/svelte-project-quality-6`. Verification1
passed its suites but was not accepted: independent review found launcher
lifecycle gaps, now repaired. Seven Linux lifecycle cases cover real PID
ownership, normal shutdown, EOF, idle/active/preparation SIGTERM and startup
failure cleanup. Earlier failures and exploratory runs remain recorded.

Against accepted application4, all 45 Rust tests and 64 JavaScript tests are
retained, with five Rust tests and one JavaScript test added. Also retained:
1,340 compiler comparisons, 10 checker and 10 project cases, 792 version compiler
comparisons, 64 version checking cases, 50 version controls, 37 runtime scenarios
(148 hydrations and 74 mounts), nine protocol checks, eight semantic LSP checks
and three checker-failure checks. Nine new Kit controls, five whole-project LSP
cases and seven lifecycle cases pass. Recovered, lost, absent and unrun counts
are each zero within these completed suites; no prototype gate regression was
found. Historical upstream gates and Query/Hono are unrun locally.

Native lint completes on 2,392 application files, reporting 77 findings: 68
`eqeqeq` findings and nine compiler warnings. Its scope is compiler diagnostics
plus the `eqeqeq` and `no-debugger` script rules. The persistent component session
passes 1,200 edits with stable post-warmup RSS of 13,709,312 bytes; that soak does
not exercise full-project semantic checks.

Both full production builds complete successfully from identical 2,486-file
application input manifests. The hybrid build makes 5,722 successful native
compiler calls, including all 24 application rune modules. Four unauthenticated
browser routes match the official build, with no browser exceptions or local
asset failures and passing hydrated input/password state interactions. Source,
binaries, build artifacts and original application inputs remain unchanged.
Build reports are in `docs/svelte-prototype/evidence/batch-5-builds`.

## Limits

This consumes existing Kit-generated route, environment and proxy declarations.
It does not implement native Kit sync. The copied source/test/scripts/config are
original application inputs; generated environment declarations came from the
isolated build environment, without production env values. Markdown pages remain
outside the pinned svelte-check component-root coverage.

Supported discovery is bounded and rejects unsupported configurations. Package
extends, baseUrl, references and TypeScript plugins are rejected. JavaScript
component projections require the application's allowJs setting, which is never
silently enabled. Auxiliary source discovery excludes build/static/hidden trees
apart from Kit types; imports into those trees are outside scope. Unprojectable
auxiliary components can still prevent preparation before reachability is known.
Runtime TypeScript rune modules use a native transpile fallback whose compiler
diagnostic ranges are explicitly unmapped; the WASM API reports that fallback
syntax as unsupported. No TypeScript core behavior was changed.

Full builds remain hybrid: Rust Svelte component/module compilation with official
JavaScript Kit/Vite, preprocessing, Tailwind and adapter integration. Native lint
coverage remains the named rules measured in application4, not ESLint parity.
Historical Query/Hono evidence is unchanged and is not claimed as a fresh local run.

## Use and evidence

The native CLI and launcher are `target/release/svelte-kit-project`; see the
[Kit plugin README](../../crates/ts_svelte_kit/README.md) for discovery, check and
stdio LSP commands and sibling executable requirements. An existing synced
project is required. The tested isolated checkout is
`target/svelte-project-app-2/app`.

[Batch state](state.json) records source identity, both final independent verdicts,
all measured attempts, retained/new coverage and evidence hashes. Selected reports
are preserved under `evidence/batch-5-run-2`, `evidence/batch-5-quality-6` and
`evidence/batch-5-builds`; complete raw requests and workspaces remain under the
recorded `target/` paths. Accepted application4 and the unaccepted first batch5
candidate are preserved separately.
