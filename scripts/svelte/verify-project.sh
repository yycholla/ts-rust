#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 3 || "$1" == help || "$1" == --help ]]; then
  echo 'Usage: scripts/svelte/verify-project.sh NEW_EVIDENCE ORIGINAL_APP ISOLATED_CHECK_APP'
  echo 'Retained application suite plus Kit project controls, full check comparison and persistent full-project LSP.'
  exit 0
fi
cd "$(dirname "$0")/../.."
out="$1"
mkdir -- "$out"
finish() {
  rc=$?
  trap - EXIT
  python3 scripts/svelte/source-identity.py > "$out/source-after.json" || rc=1
  cmp "$out/source.json" "$out/source-after.json" || rc=1
  if ((rc==0)); then echo DONE; else echo "FAIL rc=$rc"; fi | tee "$out/result.txt"
  exit "$rc"
}
trap finish EXIT
python3 scripts/svelte/source-identity.py > "$out/source.json"
scripts/svelte/verify-application.sh "$out/retained" "$2"
node scripts/svelte/kit-project-controls.mjs "$3" "$out/controls"
python3 scripts/svelte/kit-project-probe.py "$3" "$out/native"
python3 scripts/svelte/kit-check-oracle.py "$3" "$out/official"
node scripts/svelte/project-graph-oracle.mjs "$3" "$out/native/graph.json" "$out/graph"
python3 scripts/svelte/compare-kit-checks.py "$3" "$out/native" "$out/official" "$out/comparison"
node scripts/svelte/kit-lsp-probe.mjs "$3" "$out/lsp"

node scripts/svelte/kit-lifecycle-probe.mjs "$3" "$out/lifecycle"
