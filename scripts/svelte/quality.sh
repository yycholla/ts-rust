#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 2 || "$1" == help || "$1" == --help ]]; then
  echo 'Usage: scripts/svelte/quality.sh NEW_EVIDENCE_DIRECTORY APP_CHECKOUT'
  echo 'Activate a Rust toolchain with rustfmt/clippy and a native linker first.'
  echo 'Checks new crates and TypeScript declarations; does not execute app config.'
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
rustc --version > "$evidence_dir/toolchain.txt"
scripts/run-cargo-capped.sh fmt --check -p ts_svelte -p ts_svelte_wasm -p ts_svelte_check 2>&1 | tee "$evidence_dir/format.log"
scripts/run-cargo-capped.sh fmt --manifest-path crates/ts_svelte_compiler/Cargo.toml --check 2>&1 | tee "$evidence_dir/compiler-format.log"
rustfmt --edition 2024 --check crates/ts_svelte_compiler/vendor/rsvelte_core/src/compiler/mod.rs crates/ts_svelte_compiler/vendor/rsvelte_core/src/toolchain.rs 2>&1 | tee "$evidence_dir/vendor-format.log"
scripts/run-cargo-capped.sh clippy --release --locked -p ts_svelte -p ts_svelte_wasm -p ts_svelte_check --all-targets --no-deps -- -D warnings 2>&1 | tee "$evidence_dir/clippy.log"
scripts/run-cargo-capped.sh clippy --release --locked --manifest-path crates/ts_svelte_compiler/Cargo.toml --all-targets --no-deps -- -D warnings 2>&1 | tee "$evidence_dir/compiler-clippy.log"
for crate in ts_svelte_tools ts_svelte_kit; do
  scripts/run-cargo-capped.sh fmt --manifest-path "crates/$crate/Cargo.toml" --check 2>&1 | tee "$evidence_dir/$crate-format.log"
  scripts/run-cargo-capped.sh clippy --release --locked --manifest-path "crates/$crate/Cargo.toml" --all-targets --no-deps -- -D warnings 2>&1 | tee "$evidence_dir/$crate-clippy.log"
done
node "$app/node_modules/typescript/bin/tsc" --noEmit --strict --target ES2022 --module NodeNext --lib ES2022,DOM npm/svelte/index.d.ts npm/svelte/compiler.d.ts npm/svelte/tooling.d.ts 2>&1 | tee "$evidence_dir/declarations.log"
