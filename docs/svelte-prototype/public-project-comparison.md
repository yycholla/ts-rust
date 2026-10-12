# Public Svelte project comparison — 2026-10-10

The native compiler emitted client/server JavaScript for all 280 selected
components. All 560 compiler diagnostic comparisons match Svelte 5.53.11;
556 match each project's own installed Svelte version. Native project checking
does not complete for any of these projects. Full native builds and ESLint,
Prettier, or oxlint equivalents are not implemented. No speedup is established.

This experiment uses the unchanged accepted compiler source
`af2ce5d3e5536af9c882d025f0a7d8600b009a1e17fdbf4a0ec5369fc7e08dd5`.
New comparison scripts do not change the compiler or its accepted scope.

## Snapshots and scope

| Project | Commit | Svelte | Scope |
| --- | --- | --- | --- |
| [RealWorld](https://github.com/sveltejs/realworld) | `9a9a759f35881a6672fc3b546564eb2a91284b07` | 5.53.5 | Complete app build/lint; 24 source components |
| [Svelte Sonner](https://github.com/wobsoriano/svelte-sonner) | `844ac255a78bcec2afb3c85df471eb5f4aa761e9` | 5.43.5 | App and library build/check/lint; 30 source components |
| [Bits UI](https://github.com/huntabyte/bits-ui) | `fd10616a873a8e6e3652e31dcc88c5c2f155a9e2` | 5.46.4 | `packages/bits-ui` build/check and 226 components; repository-wide lint |

The initial RealWorld HEAD `df796708040f5200ec572b28ab7f88ecee5794dd`
fails frozen installation: its committed lockfile repeats the `devalue@5.8.1`
mapping key. That failed snapshot and its output remain preserved. The successful
RealWorld results above use a separate, unmodified earlier worktree. HEAD's
additional 48 component comparisons against 5.53.11 pass, but are excluded from
the 560-case total to avoid counting RealWorld twice.

All installs use the committed frozen pnpm lockfiles, except the explicitly
separate RealWorld checker tool installation described below. All tracked source
and lockfile hashes remain unchanged after execution. Generated build outputs and
installed dependencies remain in isolated checkouts under
`target/svelte-public-projects/`.

Bits UI contains 627 tracked components across the repository. Its 401 components
outside the library's `src` are excluded from compiler/check comparisons; the
root lint command includes its configured repository scope. The documentation
application, browser tests and full monorepo build were not run.

## Official commands and operational measurements

| Project | Build | Check | Lint |
| --- | --- | --- | --- |
| RealWorld | PASS, 5.34 s | 0 errors, 1 warning, 1.97 s¹ | FAIL, 1.87 s; formatting in `static/conduit-theme.css` |
| Svelte Sonner | PASS, 15.89 s | PASS, 0 errors/warnings, 7.60 s | PASS, 7.11 s |
| Bits UI library | PASS, 27.93 s | PASS, 0 errors/warnings, 19.94 s | PASS, 29.15 s² |

¹ RealWorld has no `check` script or `svelte-check` dependency. Its upstream
script is recorded as unavailable. A separate installation of svelte-check
4.4.5, Svelte 5.53.5 and TypeScript 5.9.3 checked the original `jsconfig.json`
after the build generated Kit types. It reports `state_referenced_locally` in
`src/routes/editor/Editor.svelte`. This is a supplemental official-tool invocation,
not an upstream project script.

² Bits UI lint is the repository's `oxlint . && eslint .`; its build and check
use `pnpm --filter bits-ui run build/check`. Sonner's build includes Vite,
svelte-package and publint. RealWorld's lint is Prettier, not ESLint.

Check-command maximum RSS: RealWorld 388.0 MiB, Sonner 546.5 MiB, Bits UI
867.6 MiB. GNU time reports the largest process high-water mark, not simultaneous
process-tree memory. These are single operational samples on this local host,
with package-manager/process overhead, generated outputs and OS caches. Some
dependency setup overlapped earlier official operations. They are baseline
observations, not controlled performance benchmarks.

## Native results

| Project | Client/server diagnostic matches vs installed Svelte | Matches vs Svelte 5.53.11 | Project checker |
| --- | --- | --- | --- |
| RealWorld | 48/48 | 48/48 | Incomplete: requires 5.53.11 declarations |
| Svelte Sonner | 56/60 | 60/60 | Incomplete: unsupported rune module |
| Bits UI library | 452/452 | 452/452 | Incomplete: unsupported rune module |

All three compilers (native, installed Svelte, and Svelte 5.53.11) successfully
emit JavaScript in all 560 cases. A case is one component and one target
(client or server). The four Sonner differences are two components on both
targets: the newer compiler reports `state_referenced_locally` warnings for
`toast` and `durationFromToaster` in `Toast.svelte`, and `theme` in `Toaster.svelte`.
Svelte 5.43.5 reports none; the native warnings exactly match 5.53.11.

The raw component comparison uses production mode, external CSS, no
preprocessing and no configured warning filters. RealWorld sets runes=true;
the libraries infer runes mode. Diagnostics compare code, severity, message and
UTF-8 source ranges; successful cases also compare inferred runes mode. Emitted
JS/CSS hashes are recorded but output text, runtime behavior and hydration were
not compared in this experiment. The native helper guard still targets the
5.53.11 runtime, including when compared with an older compiler. Successful
compilation does not prove generated code runs against those older runtimes.

Native project attempts include all `.svelte`, `.ts`, `.js` and `.json` inputs
under the selected `src`. They omit assets, other extensions, root configuration,
generated Kit types and files outside that tree. Language selection uses the
runner's documented quoted `lang="ts"` substring heuristic. The prototype uses
its existing fixed configuration and fails before producing project diagnostics:

- RealWorld: declarations-version guard.
- Sonner: `src/lib/internal/use-document-hidden.svelte.ts`; 2 rune modules supplied.
- Bits UI: `src/lib/bits/accordion/accordion.svelte.ts`; 58 rune modules supplied.

Those sub-0.02-second failures are not completed checks and are not performance
results. Native build/lint are unsupported rather than attempted successful
commands. WASM, LSP, browser runtime and the inherited Query/Hono suites were not
run. Previously accepted tests were not rerun; source and binaries were instead
verified unchanged. No new compiler acceptance or regression-free claim is made.

## Supporting multiple versions

Recommended next design: an immutable compatibility profile per resolved project,
shared by native and WASM execution. Resolve actual installed package versions
and real module paths per workspace/package, rather than trusting dependency
ranges or a monorepo's root version. A WASM host supplies the same version and
file metadata explicitly. Include profiles in generated-output and diagnostic
cache keys.

Keep three independently tested contracts:

1. Syntax and diagnostics: feature availability, defaults, warning rules and
   messages for the selected compiler version. Use shared algorithms with
   evidence-backed conditional differences. The Sonner warnings are one measured
   difference; hiding every warning with that code would be too broad.
2. Emitted-code/runtime behavior: choose helper calls and transformations that
   the installed runtime supports. Export-name presence is insufficient: helper
   signatures, behavior, SSR and hydration must be tested too. Keep development
   and production support explicit.
3. Checking and Kit: use the project's real declarations and compatible checking
   helpers; version route/environment generation independently from Svelte.
   TypeScript diagnostic compatibility is another dimension: this experiment
   does not prove that the fixed Rust TS engine can emulate every TS version.

Start with the exact 5.43.5, 5.46.4, 5.53.5 and 5.53.11 snapshots here. Group
versions into supported ranges only after a differential matrix establishes the
relevant equivalence. Compare syntax/errors/warnings, component and rune-module
checks, SSR/client hydration and representative project builds. Unknown versions
should produce explicit unsupported/experimental status rather than silently
selecting the nearest profile. ESLint/Prettier rule and configuration support is
a separate project, not something a Svelte version profile supplies.

The immediate implementation priorities remain rune-module diagnostics, version
profiles/declaration loading, then Kit project/configuration support. Removing
the existing version guard alone would not establish any of these contracts.
The official [compiler API](https://svelte.dev/docs/svelte/svelte-compiler) exposes
component compilation and rune-module compilation as separate operations; both
need coverage in this project's support matrix.

## Evidence and reproduction

- [Main experiment, sealed results](evidence/public-projects-1/final.json)
- [RealWorld fallback, sealed results](evidence/public-realworld-1/final.json)
- [Runner](../../scripts/svelte/public-projects.py)
- [Component comparator](../../scripts/svelte/public-components.mjs)
- [Identity/evidence sealing](../../scripts/svelte/public-finalize.py)

Each directory retains package metadata, commit, lock hash, before/after tracked
file hashes, raw command logs, exit codes, timings, explicit native inputs,
complete compared compiler diagnostics and SHA-256 evidence manifests. Final
bindings verify all accepted source files and native binaries unchanged. Harness
changes made during setup are bound separately by final hashes; the component
comparator was finalized before its measured runs.

Use a new evidence directory for every reproduction. Run runner phases `setup`,
`official`, `native`; RealWorld's fallback uses the optional `realworld-repro`
project argument and the `supplemental` phase after `official`. A finalizer run
seals each directory. These tools refuse to overwrite completed evidence.
