#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 2 || "$1" == help || "$1" == --help ]]; then
  echo 'Usage: scripts/svelte/verify-compiler.sh NEW_EVIDENCE_DIRECTORY APP_CHECKOUT'
  echo 'Runs source-foundation and compiler release tests, native/WASM builds and JS tests.'
  echo 'Runs the app-pinned Svelte fixture oracle; preserves all differences and failures.'
  echo 'Activate Rust, C/WASM linkers and wasm-bindgen 0.2.129 first.'
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
  for artifact in target/release/svelte-source target/release/svelte-tool target/release/svelte-ts-check target/release/svelte-project target/release/svelte-ts-transpile npm/svelte/ts_svelte.wasm npm/svelte/generated/*; do
    if [[ -f "$artifact" ]]; then sha256sum "$artifact"; fi
  done > "$evidence_dir/binaries.sha256"
  if ((rc == 0)); then echo DONE; else echo "FAIL rc=$rc"; fi | tee "$evidence_dir/result.txt"
  exit "$rc"
}
trap finalize EXIT
python3 scripts/svelte/source-identity.py > "$evidence_dir/source.json"
rustc --version > "$evidence_dir/toolchain.txt"
node --version >> "$evidence_dir/toolchain.txt"
scripts/run-cargo-capped.sh test --release --locked -p ts_svelte -p ts_svelte_wasm 2>&1 | tee "$evidence_dir/foundation-tests.log"
scripts/run-cargo-capped.sh test --release --locked --manifest-path crates/ts_svelte_compiler/Cargo.toml 2>&1 | tee "$evidence_dir/compiler-tests.log"
scripts/run-cargo-capped.sh test --release --locked -p ts_svelte_check 2>&1 | tee "$evidence_dir/checker-tests.log"
scripts/svelte/build.sh 2>&1 | tee "$evidence_dir/foundation-build.log"
scripts/svelte/build-compiler.sh 2>&1 | tee "$evidence_dir/compiler-build.log"
scripts/run-cargo-capped.sh build --release --locked -p ts_svelte_check --bins 2>&1 | tee "$evidence_dir/checker-build.log"
node --test npm/svelte/test/source.test.mjs npm/svelte/test/compiler.test.mjs npm/svelte/test/versions.test.mjs 2>&1 | tee "$evidence_dir/js-tests.log"
node scripts/svelte/compiler-oracle.mjs --app "$app" --binary target/release/svelte-tool --out "$evidence_dir/oracle" --project-compile 2>&1 | tee "$evidence_dir/oracle.log"
node scripts/svelte/runtime-oracle.mjs --app "$app" --compiler target/release/svelte-tool --out "$evidence_dir/runtime" 2>&1 | tee "$evidence_dir/runtime.log"
node scripts/svelte/checker-oracle.mjs --app "$app" --compiler target/release/svelte-tool --checker target/release/svelte-ts-check --out "$evidence_dir/checker" 2>&1 | tee "$evidence_dir/checker.log"
node scripts/svelte/project-smoke.mjs "$app" "$evidence_dir/project" 2>&1 | tee "$evidence_dir/project.log"
