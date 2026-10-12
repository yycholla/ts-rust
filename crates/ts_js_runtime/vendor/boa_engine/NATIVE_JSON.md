# Native JSON intrinsic repair

This patch applies to `boa_engine` 0.22.0, whose original source identities and
licenses are retained in `NATIVE_REACTIONS_UPSTREAM.json`, `LICENSE-MIT`, and
`LICENSE-UNLICENSE`. That original hash manifest is unchanged. Apply
`NATIVE_JSON.patch` alongside `NATIVE_REACTIONS.patch`; they touch separate files.

The modified file is `src/builtins/json/mod.rs`.

- Original SHA-256: `0062888a5f7c09d62c352937a82bef141cbc45867e6965605dd2dc92c5405e62`
- Patched SHA-256: `22db5d511edc0174ccc45e397555d862848f1cb478ef7e577fd6f0e0d3291011`

The existing JSON parser first materialized a `serde_json::Value`, rejecting
valid JavaScript JSON numbers outside Rust finite-number bounds and strings with
unpaired UTF-16 surrogates. It also converted the input to a Rust `String` before
Boa's own JSON-mode parser and evaluation. Grammar validation now uses serde's
`IgnoredAny`, while Boa receives the unchanged UTF-16 code units. No JavaScript
fallback or alternate value evaluator is introduced. The former 128-level
serde validation nesting boundary is retained explicitly before recursive AST
processing (128 nested arrays/objects are rejected, as before).

The validation-only lossy UTF-8 copy cannot change grammar acceptance: a raw lone
surrogate and U+FFFD are both legal non-control JSON string content and neither
is a JSON token, whitespace, ASCII escape marker, or hexadecimal digit. Escapes
are validated by serde_json's `ignore_escape`, which still requires exactly four
hexadecimal digits after `\u`. Input values and reviver source text never use
this lossy copy. `from_str` still requires consumption of the complete document.

Reviver parse records now preserve UTF-16 keys and primitive source text, capture
the actual evaluated values before the first reviver callback, and use SameValue
rather than reparsing source into Rust strings/numbers. This also distinguishes
positive/negative zero and prevents source records leaking into replaced object
or array subtrees. Duplicate properties retain the last parse node while normal
ECMAScript property order comes from the evaluated object.

Authoritative algorithms:
<https://tc39.es/ecma262/multipage/structured-data.html#sec-json.parse>
<https://tc39.es/ecma262/multipage/structured-data.html#sec-internalizejsonproperty>

Regression tests are in `../../src/json_intrinsic_tests.rs`; register with
`#[cfg(test)] mod json_intrinsic_tests;` in the runtime crate. Existing Fetch Body
JSON tests exercise the captured intrinsic after application code poisons global
JSON methods. The implementation agent did not run Cargo; root owns verification.
`JSON.rawJSON` is unchanged by this bounded repair.
