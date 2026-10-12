#!/usr/bin/env python3
"""Usage: public-projects.py OUT_DIR setup|official|native|supplemental [PROJECT_NAME]

Reproduce public project operations without modifying the compiler. Run phases in
order, using a new evidence directory. Commands, logs, metrics, and source hashes
are retained, including failures. Dependencies use frozen project lockfiles.
"""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
PROJECTS = ROOT / "target/svelte-public-projects"
NAMES = ("realworld", "svelte-sonner", "bits-ui")
TIME = "/run/current-system/sw/bin/time"


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def snapshot(directory):
    files = subprocess.check_output(["git", "ls-files", "-z"], cwd=directory).decode().split("\0")
    return {name: sha(directory / name) if (directory / name).is_file() else None
            for name in files if name}


def run(out, label, cwd, command, timeout=300, stdin=None):
    log = out / (label + ".log")
    metrics = out / (label + ".time")
    if log.exists():
        raise ValueError(f"Refusing existing evidence: {log}")
    start = time.monotonic()
    with log.open("w") as stream:
        child = subprocess.Popen([TIME, "-f", "%e %M %U %S", "-o", str(metrics), "--", *command],
                                 cwd=cwd, stdout=stream, stderr=subprocess.STDOUT,
                                 stdin=subprocess.PIPE if stdin is not None else subprocess.DEVNULL,
                                 start_new_session=True, env={**os.environ, "CI": "true", "GOPORT_LAUNCH": "0"})
        timed_out = False
        try:
            child.communicate(stdin, timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
    result = {"command": command, "cwd": str(cwd), "exitCode": child.returncode,
              "timeout": timed_out, "timeoutSeconds": timeout,
              "wallSeconds": round(time.monotonic() - start, 3), "log": log.name}
    if metrics.exists():
        for line in metrics.read_text().splitlines():
            parts = line.split()
            if len(parts) == 4:
                try:
                    result.update(timeSeconds=float(parts[0]), maxRssKiB=int(parts[1]),
                                  userSeconds=float(parts[2]), systemSeconds=float(parts[3]))
                except ValueError:
                    pass
    save(out / (label + ".json"), result)
    print(label, json.dumps(result), flush=True)
    return result


def pnpm(version, *args):
    return ["npm", "exec", "--yes", "--package=pnpm@" + version, "--", "pnpm", *args]


def main(out, phase, names=NAMES):
    if phase == "setup":
        out.mkdir(parents=True, exist_ok=False)
        accepted = json.loads((ROOT / "docs/svelte-prototype/evidence/batch-2-run-3/source.json").read_text())
        changed = [name for name, expected in accepted["files"].items()
                   if not (ROOT / name).is_file() or sha(ROOT / name) != expected]
        if changed:
            raise RuntimeError(f"Accepted source changed: {changed}")
        save(out / "identity.json", {"acceptedSourceFingerprint": accepted["sourceFingerprint"],
             "acceptedManifestFilesChanged": changed,
             "harness": {str(p.relative_to(ROOT)): sha(p) for p in (ROOT / "scripts/svelte").glob("public-*") if p.is_file()},
             "binaries": {p.name: sha(p) for p in (ROOT / "target/release").glob("svelte-*") if p.is_file() and p.suffix != ".d"},
             "node": subprocess.check_output(["node", "--version"], text=True).strip(),
             "timingScope": "Single operational sample; GNU time max RSS is largest child high-water mark, not process-tree total; no speedup claim."})
    elif not (out / "identity.json").exists():
        raise ValueError("Run setup first")
    for name in names:
        repo = PROJECTS / name
        evidence = out / name
        package = json.loads((repo / "package.json").read_text())
        version = package["packageManager"].split("@")[1]
        if phase == "setup":
            evidence.mkdir()
            save(evidence / "snapshot-before.json", snapshot(repo))
            save(evidence / "project.json", {"repository": subprocess.check_output(["git", "remote", "get-url", "origin"], cwd=repo, text=True).strip(),
                 "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip(),
                 "package": package, "lockSha256": sha(repo / "pnpm-lock.yaml")})
            run(evidence, "install", repo, pnpm(version, "install", "--frozen-lockfile"), timeout=600)
        elif phase == "official":
            if json.loads((evidence / "install.json").read_text())["exitCode"] != 0:
                save(evidence / "official-unrun.json", {"reason": "Dependency installation failed"})
                continue
            # Bits UI package is the scoped build/check target; lint stays monorepo-wide.
            for task in ("build", "check", "lint"):
                if task not in package.get("scripts", {}):
                    save(evidence / (task + ".json"), {"status": "unavailable", "reason": "No upstream script"})
                    continue
                args = ["--filter", "bits-ui", "run", task] if name == "bits-ui" and task != "lint" else ["run", task]
                run(evidence, task, repo, pnpm(version, *args), timeout=300)
            versions = {}
            base = repo / "packages/bits-ui" if name == "bits-ui" else repo
            for pkg in ("svelte", "svelte-check", "typescript", "@sveltejs/kit", "vite", "eslint"):
                p = base / "node_modules" / pkg / "package.json"
                if not p.exists():
                    p = repo / "node_modules" / pkg / "package.json"
                versions[pkg] = json.loads(p.read_text())["version"] if p.exists() else None
            save(evidence / "versions.json", versions)
            save(evidence / "snapshot-after-official.json", snapshot(repo))
        elif phase == "native":
            base = repo / "packages/bits-ui" if name == "bits-ui" else repo
            sources = sorted((base / "src").rglob("*"))
            components, files, excluded = [], [], []
            for p in sources:
                if not p.is_file():
                    continue
                rel = str(p.relative_to(base))
                if p.suffix == ".svelte":
                    text = p.read_text()
                    components.append({"path": rel, "document": {"uri": p.as_uri(), "version": 1, "text": text},
                                       "typescript": 'lang="ts"' in text or "lang='ts'" in text})
                elif p.suffix in (".ts", ".js", ".json"):
                    files.append({"path": rel, "text": p.read_text()})
                else:
                    excluded.append(rel)
            request = {"workspace": str(evidence / "native-workspace"), "dependencies": str(base / "node_modules"),
                       "components": components, "files": files}
            save(evidence / "native-input.json", request)
            save(evidence / "native-scope.json", {"base": str(base), "components": len(components), "files": len(files),
                 "runeModules": [f["path"] for f in files if f["path"].endswith((".svelte.ts", ".svelte.js"))],
                 "excludedSrcFiles": excluded, "outsideSrc": "All files outside the selected src tree are excluded",
                 "languageDetection": "Exact lang=ts quoted attribute substring heuristic; only an explicit-input attempt, not project parity",
                 "fullBuild": "unsupported: no native bundler/Kit/adapter pipeline",
                 "lint": "unsupported: no ESLint/Prettier/oxlint implementation",
                 "check": "Explicit src inputs; native fixed config, no Kit generated types, preprocessing or alias configuration"})
            run(evidence, "native-check", ROOT, [str(ROOT / "target/release/svelte-project")],
                stdin=json.dumps(request).encode(), timeout=180)
            run(evidence, "components", ROOT, ["node", "scripts/svelte/public-components.mjs", str(base), str(evidence / "components")], timeout=300)
            save(evidence / "snapshot-after-native.json", snapshot(repo))
        elif phase == "supplemental":
            if name != "realworld-repro":
                raise ValueError("Supplemental check is scoped to RealWorld, which has no check script")
            tool = PROJECTS / "realworld-check-tools"
            if tool.exists():
                raise ValueError("Refusing to reuse an existing supplemental tool installation")
            version = json.loads((repo / "node_modules/svelte/package.json").read_text())["version"]
            installed = run(evidence, "supplemental-install", ROOT,
                ["npm", "install", "--prefix", str(tool), "--ignore-scripts", "--no-audit", "--no-fund",
                 "svelte@" + version, "svelte-check@4.4.5", "typescript@5.9.3"])
            if installed["exitCode"] == 0:
                save(evidence / "supplemental-tools.json", {"lockSha256": sha(tool / "package-lock.json"),
                     "svelte": version, "svelte-check": "4.4.5", "typescript": "5.9.3",
                     "scope": "Explicit svelte-check invocation; not an upstream project script"})
                run(evidence, "supplemental-check", repo,
                    ["node", str(tool / "node_modules/svelte-check/bin/svelte-check"),
                     "--workspace", str(repo), "--tsconfig", str(repo / "jsconfig.json")])


if __name__ == "__main__":
    if len(sys.argv) not in (3, 4) or sys.argv[2] not in ("setup", "official", "native", "supplemental"):
        print(__doc__)
        sys.exit(0 if any(a in ("help", "--help") for a in sys.argv[1:]) else 2)
    names = (sys.argv[3],) if len(sys.argv) == 4 else NAMES
    if any(n not in (*NAMES, "realworld-repro") for n in names):
        raise ValueError("Unknown project name")
    main(Path(sys.argv[1]).resolve(), sys.argv[2], names)
