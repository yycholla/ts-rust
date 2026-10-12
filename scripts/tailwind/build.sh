#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == help || "${1:-}" == --help ]]; then
  echo 'Usage: scripts/tailwind/build.sh [help]'
  echo 'Build native tailwind and browser WASM bindings from the same Rust compiler.'
  echo 'Requires wasm32-unknown-unknown and wasm-bindgen-cli 0.2.129.'
  exit 0
fi
cd "$(dirname "$0")/../.."
runtime_bindgen="${WASM_BINDGEN:-wasm-bindgen}"
if [[ "$("$runtime_bindgen" --version)" != 'wasm-bindgen 0.2.129' ]]; then
  echo 'Expected wasm-bindgen-cli 0.2.129' >&2
  exit 1
fi
scripts/run-cargo-capped.sh build --release --locked --manifest-path crates/ts_tailwind/Cargo.toml --bin tailwind
scripts/run-cargo-capped.sh build --release --locked --manifest-path crates/ts_tailwind/Cargo.toml --target wasm32-unknown-unknown --lib
"$runtime_bindgen" target/wasm32-unknown-unknown/release/ts_tailwind.wasm --target web --out-dir npm/tailwind/generated
