#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 1 || "$1" == help || "$1" == --help ]]; then
  echo 'Usage: scripts/svelte/verify.sh NEW_EVIDENCE_DIRECTORY'
  echo 'Runs release tests, builds native and WASM, and compares JS/WASM with native.'
  echo 'Refuses an existing evidence directory. Activate a Rust/C/WASM toolchain first.'
  echo 'Rustfmt and Clippy are separate checks (see npm/svelte/README.md).'
  exit 0
fi
cd "$(dirname "$0")/../.."
evidence_dir="$1"
mkdir -- "$evidence_dir"
trap 'rc=$?; if ((rc != 0)); then echo "FAIL rc=$rc" | tee "$evidence_dir/result.txt"; fi' EXIT
python3 scripts/svelte/source-identity.py > "$evidence_dir/source.json"
rustc --version > "$evidence_dir/toolchain.txt"
node --version >> "$evidence_dir/toolchain.txt"
scripts/run-cargo-capped.sh test --release --locked -p ts_svelte -p ts_svelte_wasm 2>&1 | tee "$evidence_dir/rust-tests.log"
scripts/svelte/build.sh 2>&1 | tee "$evidence_dir/build.log"
node --test npm/svelte/test/source.test.mjs 2>&1 | tee "$evidence_dir/js-tests.log"
python3 scripts/svelte/source-identity.py > "$evidence_dir/source-after.json"
cmp "$evidence_dir/source.json" "$evidence_dir/source-after.json"
sha256sum target/release/svelte-source npm/svelte/ts_svelte.wasm > "$evidence_dir/binaries.sha256"
echo DONE | tee "$evidence_dir/result.txt"
