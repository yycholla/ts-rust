# Pinned compatibility patches

The rsvelte_core source is copied from commit
`5ed8ea3a3401b9fbbe780d3b18d6f90073291d6b` (MIT).
`rsvelte_core/UPSTREAM.json` retains every original file hash and documents local
changes. The upstream target remains 5.57.1. Exact profile support is bounded by
our recorded comparisons, not a claim of complete upstream conformance.

The retained child/reset patch avoids the newer only_child runtime helper while
preserving AST printing and source maps. A compatibility minor flows through
prepared parsing and client transforms. Before5.56, declaration tags take the
older expression-parser path. Before5.57, select defaultValue uses ordinary
attribute/property emission; newer profiles use set_default_select_value.
Unprofiled upstream entrypoints keep their default57 behavior.

The adapter rejects client development mode on older profiles because assignment
instrumentation uses a newer runtime ABI. It parses and binds emitted client and
server JavaScript, checking runtime exports against the exact manifests under
`src/profiles/`. `scripts/svelte/version-exports.mjs` generates them from pinned
installations and records package and index hashes. Static and string-computed
accesses resolve to imported symbols; namespace escapes and dynamic access fail
explicitly. Export existence is only a guard: JSDOM SSR, hydration and mount
comparisons supply separate, bounded behavioral evidence.
