#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == help || "${1:-}" == --help ]]; then
  echo 'Usage: TS_SVELTE_RUSTC=COMPILER TS_SVELTE_SYSROOT=SYSROOT scripts/svelte/rustc-sysroot.sh RUSTC_ARGS...'
  echo 'Optional RUSTC wrapper for Cargo build-std with an immutable compiler installation.'
  exit 0
fi
: "${TS_SVELTE_RUSTC:?Set the absolute path of the real Rust compiler}"
: "${TS_SVELTE_SYSROOT:?Set the project-local sysroot overlay}"
exec "$TS_SVELTE_RUSTC" --sysroot "$TS_SVELTE_SYSROOT" "$@"
