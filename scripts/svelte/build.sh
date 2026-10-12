#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == help || "${1:-}" == --help ]]; then
  echo 'Usage: scripts/svelte/build.sh [help]'
  echo 'Build the native svelte-source CLI and import-free Rust/WASM source engine.'
  echo 'Requires Rust with wasm32-unknown-unknown, a native C linker, and the capped Cargo runner.'
  exit 0
fi
cd "$(dirname "$0")/../.."
scripts/run-cargo-capped.sh build --release --locked -p ts_svelte --bin svelte-source
scripts/run-cargo-capped.sh build --release --locked -p ts_svelte_wasm --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/ts_svelte_wasm.wasm npm/svelte/ts_svelte.wasm
