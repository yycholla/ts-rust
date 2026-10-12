#!/usr/bin/env python3
"""Usage: public-finalize.py EVIDENCE_DIR [EVIDENCE_DIR...]
Seal public-project evidence with end-of-run source, binary and input checks.
Does not accept a compiler revision or modify earlier evidence.
"""
import datetime
import hashlib
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main(directory):
    if (directory / "final.json").exists():
        raise ValueError("Refusing existing final evidence")
    start = json.loads((directory / "identity.json").read_text())
    accepted = json.loads((ROOT / "docs/svelte-prototype/evidence/batch-2-run-3/source.json").read_text())
    changed = [name for name, value in accepted["files"].items() if not (ROOT / name).is_file() or sha(ROOT / name) != value]
    changed_bins = [name for name, value in start["binaries"].items() if sha(ROOT / "target/release" / name) != value]
    if changed or changed_bins:
        raise ValueError(f"Identity mismatch: {changed}, {changed_bins}")
    result = {"completedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
              "acceptedSourceFingerprint": accepted["sourceFingerprint"],
              "acceptedSourceFilesChanged": changed, "binaryChanges": changed_bins,
              "compilerAcceptance": "Unchanged accepted prototype; this is additional comparison evidence, not expanded compiler acceptance",
              "harnessFinal": {str(p.relative_to(ROOT)): sha(p) for p in (ROOT / "scripts/svelte").glob("public-*") if p.is_file()},
              "projects": {}}
    for evidence in sorted(directory.iterdir()):
        if not evidence.is_dir() or not (evidence / "project.json").exists():
            continue
        repo = ROOT / "target/svelte-public-projects" / evidence.name
        before = json.loads((evidence / "snapshot-before.json").read_text())
        modified = [name for name, value in before.items() if not (repo / name).is_file() or sha(repo / name) != value]
        now_names = subprocess.check_output(["git", "ls-files", "-z"], cwd=repo).decode().split("\0")
        added = sorted(set(filter(None, now_names)) - set(before))
        base = repo / "packages/bits-ui" if evidence.name == "bits-ui" else repo
        included = str((base / "src").relative_to(repo)) + "/"
        svelte_files = [n for n in before if n.endswith(".svelte")]
        entry = {"trackedFilesChanged": modified, "trackedFilesAdded": added,
                 "commitUnchanged": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip() == json.loads((evidence / "project.json").read_text())["commit"],
                 "allTrackedComponents": len(svelte_files), "selectedSourcePrefix": included,
                 "componentsOutsideSelectedSrc": [n for n in svelte_files if not n.startswith(included)],
                 "operations": {}}
        for label in ("install", "build", "check", "lint", "supplemental-check", "native-check", "components"):
            p = evidence / (label + ".json")
            if p.exists():
                entry["operations"][label] = json.loads(p.read_text())
        if (evidence / "components/report.json").exists():
            entry["componentSummary"] = json.loads((evidence / "components/report.json").read_text())["summary"]
        for label, app in (("projectOracle", base), ("supportedOracle", Path("/home/yycholla/Documents/MoncuraHR"))):
            p = app / "node_modules/svelte/package.json"
            if p.exists():
                compiler = app / "node_modules/svelte/src/compiler/index.js"
                entry[label] = {"version": json.loads(p.read_text())["version"], "packageSha256": sha(p), "entrySha256": sha(compiler)}
        result["projects"][evidence.name] = entry
    result["evidenceSha256"] = {str(p.relative_to(directory)): sha(p) for p in sorted(directory.rglob("*")) if p.is_file()}
    (directory / "final.json").write_text(json.dumps(result, indent=2) + "\n")
    print(directory, "accepted source unchanged; binaries unchanged; tracked input changes:",
          {k:v["trackedFilesChanged"] for k,v in result["projects"].items()})


if __name__ == "__main__":
    if len(sys.argv) == 1 or any(x in ("help", "--help") for x in sys.argv[1:]):
        print(__doc__)
    else:
        for arg in sys.argv[1:]:
            main(Path(arg).resolve())
