#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 2 || "$1" == help || "$1" == --help ]]; then
  echo 'Usage: scripts/svelte/verify-application.sh NEW_EVIDENCE_DIRECTORY APP_CHECKOUT'
  echo 'Runs retained version/compiler checks, separate tools native/WASM checks, lint, persistent LSP and soak probes.'
  echo 'Run quality.sh and isolated full builds/browser comparisons separately. Requires installed exact oracles and WASM toolchain.'
  exit 0
fi
cd "$(dirname "$0")/../.."
evidence_dir="$1"
app="$2"
mkdir -- "$evidence_dir"
finalize() {
  rc=$?
  trap - EXIT
  if [[ -f "$evidence_dir/tools-artifacts.sha256" ]]; then sha256sum -c "$evidence_dir/tools-artifacts.sha256" > "$evidence_dir/tools-artifacts-verified.log" || rc=1; fi
  python3 scripts/svelte/source-identity.py > "$evidence_dir/source-after.json" || rc=1
  cmp "$evidence_dir/source.json" "$evidence_dir/source-after.json" || rc=1
  if ((rc == 0)); then echo DONE; else echo "FAIL rc=$rc"; fi | tee "$evidence_dir/result.txt"
  exit "$rc"
}
trap finalize EXIT
python3 scripts/svelte/source-identity.py > "$evidence_dir/source.json"
scripts/svelte/build-tools.sh 2>&1 | tee "$evidence_dir/tools-build.log"
scripts/run-cargo-capped.sh test --release --locked --manifest-path crates/ts_svelte_tools/Cargo.toml 2>&1 | tee "$evidence_dir/tools-tests.log"
scripts/run-cargo-capped.sh test --release --locked --manifest-path crates/ts_svelte_kit/Cargo.toml 2>&1 | tee "$evidence_dir/kit-tests.log"
sha256sum target/release/svelte-lsp target/release/svelte-lint target/release/svelte-service target/release/ts_svelte_kit target/release/svelte-kit-project npm/svelte/generated/ts_svelte_tools* > "$evidence_dir/tools-artifacts.sha256"
scripts/svelte/verify-versions.sh "$evidence_dir/versions" "$app"
node --test npm/svelte/test/tooling.test.mjs npm/svelte-kit/test/*.test.mjs 2>&1 | tee "$evidence_dir/tooling-js-tests.log"
node scripts/svelte/application-probe.mjs "$evidence_dir/protocol"
node scripts/svelte/lsp-project-probe.mjs "$evidence_dir/lsp-project"
node scripts/svelte/lsp-failure-probe.mjs "$evidence_dir/lsp-failure"
node scripts/svelte/session-soak.mjs "$evidence_dir/soak"
node scripts/svelte/lint-probe.mjs "$app" "$evidence_dir/lint"
