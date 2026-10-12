#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == help || "${1:-}" == --help ]]; then
  echo 'Usage: scripts/svelte/build-formatter.sh [help]'
  echo 'Builds the isolated native formatter and its WASM worker bindings.'
  echo 'Requires wasm32-unknown-unknown and wasm-bindgen-cli 0.2.129.'
  exit 0
fi
cd "$(dirname "$0")/../.."
bindgen="${WASM_BINDGEN:-wasm-bindgen}"
if [[ "$("$bindgen" --version)" != 'wasm-bindgen 0.2.129' ]]; then
  echo 'Expected wasm-bindgen-cli 0.2.129' >&2
  exit 1
fi
scripts/run-cargo-capped.sh build --release --locked --manifest-path crates/ts_svelte_format/Cargo.toml --bin svelte-format
scripts/run-cargo-capped.sh build --release --locked --manifest-path crates/ts_svelte_format/Cargo.toml --target wasm32-unknown-unknown --lib
"$bindgen" target/wasm32-unknown-unknown/release/ts_svelte_format.wasm --target web --out-dir npm/svelte/generated
