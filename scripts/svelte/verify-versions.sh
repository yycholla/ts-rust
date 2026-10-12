#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 2 || "$1" == help || "$1" == --help ]]; then
  echo 'Usage: scripts/svelte/verify-versions.sh NEW_EVIDENCE_DIRECTORY APP_CHECKOUT'
  echo 'Runs retained native/WASM/JS checks, exact-version diagnostics and runtime comparisons.'
  echo 'Requires the installations created by version-setup.py and the build-compiler.sh toolchain.'
  echo 'Run quality.sh separately with rustfmt and clippy installed.'
  exit 0
fi
cd "$(dirname "$0")/../.."
evidence_dir="$1"
app="$2"
mkdir -- "$evidence_dir"
finalize() {
  rc=$?
  trap - EXIT
  python3 scripts/svelte/source-identity.py > "$evidence_dir/source-after.json" || rc=1
  cmp "$evidence_dir/source.json" "$evidence_dir/source-after.json" || rc=1
  if ((rc == 0)); then echo DONE; else echo "FAIL rc=$rc"; fi | tee "$evidence_dir/result.txt"
  exit "$rc"
}
trap finalize EXIT
python3 scripts/svelte/source-identity.py > "$evidence_dir/source.json"
scripts/run-cargo-capped.sh build --release --locked --manifest-path crates/ts_svelte_compiler/Cargo.toml --bin svelte-tool 2>&1 | tee "$evidence_dir/native-build.log"
node scripts/svelte/version-runtime.mjs "$evidence_dir/runtime"
scripts/svelte/verify-compiler.sh "$evidence_dir/regression" "$app"
node scripts/svelte/version-matrix.mjs --out "$evidence_dir/matrix"
