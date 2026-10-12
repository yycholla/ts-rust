#!/usr/bin/env python3
"""Create a bounded declaration-only snapshot for synthetic WASM project cases."""
import argparse
import json
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("node_modules", type=Path)
parser.add_argument("output", type=Path, help="New JSON output; existing evidence is never replaced")
args = parser.parse_args()
files = {}
total = 0
for package in ["svelte", "svelte-check", "csstype", "@types/estree", "@types/estree-jsx"]:
    base = args.node_modules / package
    if not base.is_dir():
        if package in ["svelte", "svelte-check"]:
            raise SystemExit(f"Missing required package: {package}")
        continue
    real = base.resolve(strict=True)
    for path in sorted(base.rglob("*")):
        relative = path.relative_to(base)
        if "node_modules" in relative.parts or not path.is_file():
            continue
        if path.name != "package.json" and not path.name.endswith((".d.ts", ".d.mts", ".d.cts")):
            continue
        if not path.resolve(strict=True).is_relative_to(real):
            raise SystemExit(f"Declaration escapes package boundary: {package}/{relative}")
        size = path.stat().st_size
        total += size
        if size > 1024 * 1024 or total > 12 * 1024 * 1024:
            raise SystemExit("Declaration snapshot exceeds limits")
        files[f"/project/node_modules/{package}/{relative.as_posix()}"] = path.read_text()
with args.output.open("x") as output:
    json.dump(files, output, ensure_ascii=False)
    output.write("\n")
print(json.dumps({"files": len(files), "sourceBytes": total, "output": str(args.output)}))
