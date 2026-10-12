#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == help || "${1:-}" == --help ]]; then
  echo 'Usage: scripts/svelte/build-tools.sh [help]'
  echo 'Builds native Svelte tools, separate Kit host, and tooling WASM bindings.'
  exit 0
fi
cd "$(dirname "$0")/../.."
scripts/run-cargo-capped.sh build --release --locked --manifest-path crates/ts_svelte_tools/Cargo.toml --bins
scripts/run-cargo-capped.sh build --release --locked --manifest-path crates/ts_svelte_kit/Cargo.toml
scripts/run-cargo-capped.sh build --release --locked --manifest-path crates/ts_svelte_tools/Cargo.toml --target wasm32-unknown-unknown --lib
"${WASM_BINDGEN:-wasm-bindgen}" target/wasm32-unknown-unknown/release/ts_svelte_tools.wasm --target web --out-dir npm/svelte/generated
