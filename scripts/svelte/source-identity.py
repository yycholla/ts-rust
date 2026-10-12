#!/usr/bin/env python3
"""Usage: scripts/svelte/source-identity.py [help]
Hash prototype source, tests, manifests and rules; exclude generated evidence/bins.
"""
import hashlib
import json
from pathlib import Path
import subprocess
import sys

if len(sys.argv) > 1:
    print(__doc__)
    sys.exit(0 if sys.argv[1] in ("help", "--help") else 2)

root = Path(__file__).resolve().parents[2]
paths = [root / name for name in (
    "AGENTS.md", "Cargo.toml", "Cargo.lock", "docs/svelte-prototype/plan.md", "docs/svelte-prototype/version-test-mapping.json",
)]
for directory in ("crates/ts_svelte", "crates/ts_svelte_wasm", "crates/ts_svelte_compiler", "crates/ts_svelte_check", "crates/ts_svelte_tools", "crates/ts_svelte_kit", "npm/svelte", "npm/svelte-kit", "scripts/svelte"):
    paths.extend(p for p in (root / directory).rglob("*")
                 if p.is_file() and p.suffix not in (".wasm", ".pyc")
                 and not any(part in p.parts for part in ("node_modules", "__pycache__", "generated", "target")))
files = {str(p.relative_to(root)): hashlib.sha256(p.read_bytes()).hexdigest()
         for p in sorted(paths)}
fingerprint = hashlib.sha256(json.dumps(files, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip()
print(json.dumps({"baseCommit": head, "sourceFingerprint": fingerprint, "files": files}, indent=2))
