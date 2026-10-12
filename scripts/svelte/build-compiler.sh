#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == help || "${1:-}" == --help ]]; then
  echo 'Usage: scripts/svelte/build-compiler.sh [help]'
  echo 'Builds native and WASM Svelte compiler, then generates the JS bindings.'
  echo 'Requires wasm32-unknown-unknown and wasm-bindgen-cli 0.2.129.'
  echo 'Set WASM_BINDGEN to its executable path if it is not on PATH.'
  exit 0
fi
cd "$(dirname "$0")/../.."
bindgen="${WASM_BINDGEN:-wasm-bindgen}"
if [[ "$("$bindgen" --version)" != 'wasm-bindgen 0.2.129' ]]; then
  echo 'Expected wasm-bindgen-cli 0.2.129 (matches Cargo.lock)' >&2
  exit 1
fi
scripts/run-cargo-capped.sh build --release --locked --manifest-path crates/ts_svelte_compiler/Cargo.toml --bin svelte-tool
scripts/run-cargo-capped.sh build --release --locked --manifest-path crates/ts_svelte_compiler/Cargo.toml --target wasm32-unknown-unknown --lib
"$bindgen" target/wasm32-unknown-unknown/release/ts_svelte_compiler.wasm --target web --out-dir npm/svelte/generated
