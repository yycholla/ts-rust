#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == help || "${1:-}" == --help ]]; then
  echo 'Usage: scripts/svelte/build-semantic.sh [Cargo build options]'
  echo 'Build the persistent semantic WASI reactor and copy it to npm/svelte/generated.'
  echo 'Requires Rust with wasm32-wasip1; toolchain, caps and RUSTFLAGS come from the environment.'
  exit 0
fi
cd "$(dirname "$0")/../.."
scripts/run-cargo-capped.sh build --release --locked \
  --manifest-path crates/ts_svelte_semantic/Cargo.toml --lib --target wasm32-wasip1 "$@"
mkdir -p npm/svelte/generated
cp target/wasm32-wasip1/release/ts_svelte_semantic.wasm npm/svelte/generated/ts_svelte_semantic.wasm
