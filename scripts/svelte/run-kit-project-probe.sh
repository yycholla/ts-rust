#!/usr/bin/env bash
set -euo pipefail
if [[ $# != 2 || "$1" == help || "$1" == --help ]]; then
  echo 'Usage: scripts/svelte/run-kit-project-probe.sh APP NEW_OUTPUT'
  echo 'Run under a systemd memory-limited scope. Preserves native discovery/check evidence.'
  exit 0
fi
trap 'rc=$?; if ((rc==0)); then echo DONE; else echo "FAIL rc=$rc"; fi' EXIT
python3 scripts/svelte/kit-project-probe.py "$1" "$2"
