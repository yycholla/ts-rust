#!/usr/bin/env python3
"""Usage: native-public.py setup|install|load|official|native NAME... | list

Run prominent public SvelteKit projects through the native build and the
official `vite build` on isolated copies under target/svelte-native-public/.

  setup    clone at the recorded commit (or HEAD, then record it), install from
           the project's lockfile, run any listed preparation step
  install  reinstall dependencies (frozen lockfile) and rerun preparation
  load     native project loader only (requestOnly): what is unported
  official package build script equivalent: `vite build` in the app directory
  native   svelte-build project op into a new output directory

Evidence (commands, logs, GNU time metrics, loader requirements) is kept in
target/svelte-native-public/NAME/evidence; nothing is published. Each step's
log ends with DONE or FAIL rc=<N>.
"""
import json, os, shutil, subprocess, sys, time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
BASE = REPO / "target/svelte-native-public"
TIME = "/run/current-system/sw/bin/time"
NATIVE = REPO / "target/release/svelte-build"
SV_DEMO = ["npx", "--yes", "sv@1.1.1", "create", "repo", "--template", "demo",
           "--types", "ts", "--no-add-ons", "--install", "npm"]
# app: SvelteKit app directory inside the checkout; prepare: commands run in
# the checkout root after install (workspace packages the app imports).
PROJECTS = {
    "sv-demo": {"generate": SV_DEMO, "app": "."},
    # Kit 3 template variants the native build can package: adapter-node 6.0.0
    # (the Kit 3 major) on both sides, and for the demo `<enhanced:img>`
    # (sharp image processing, not ported) replaced by a plain image import.
    "sv-demo-node": {"generate": SV_DEMO, "app": ".", "adapterNode": "6.0.0", "edits": [
        ["vite.config.ts", "import { enhancedImages } from '@sveltejs/enhanced-img';\n", ""],
        ["vite.config.ts", "\t\tenhancedImages(),\n", ""],
        ["src/routes/+page.svelte", "\timport Counter from './Counter.svelte';\n",
         "\timport Counter from './Counter.svelte';\n\timport welcome from '#lib/images/svelte-welcome.png';\n"],
        ["src/routes/+page.svelte",
         '<enhanced:img class="welcome-image" src="#lib/images/svelte-welcome.png" alt="Welcome" />',
         '<img class="welcome-image" src={welcome} alt="Welcome" />'],
    ]},
    "sv-minimal-node": {"generate": ["npx", "--yes", "sv@1.1.1", "create", "repo", "--template", "minimal",
                                     "--types", "ts", "--no-add-ons", "--install", "npm"],
                        "app": ".", "adapterNode": "6.0.0"},
    # The committed pnpm-lock.yaml has a duplicated key (upstream), so pnpm
    # cannot read it: install without it and record what resolved.
    "realworld": {"git": "https://github.com/sveltejs/realworld.git", "app": ".", "dropLockfile": True},
    # Same commit with adapter-node swapped in on both sides, so the native
    # build (adapter-node only) and the browser comparison can run.
    "realworld-node": {"git": "https://github.com/sveltejs/realworld.git", "app": ".", "dropLockfile": True,
                       "adapterNode": "5.5.4"},
    "bits-ui-docs": {"git": "https://github.com/huntabyte/bits-ui.git", "app": "docs",
                     "prepare": [["run", "--filter", "bits-ui", "build"]]},
    "shadcn-svelte-docs": {"git": "https://github.com/huntabyte/shadcn-svelte.git", "app": "docs"},
    "svelte-dev": {"git": "https://github.com/sveltejs/svelte.dev.git", "app": "apps/svelte.dev"},
}
ENV_KEYS = ("PATH", "HOME", "NIX_LD", "NIX_LD_LIBRARY_PATH", "LD_LIBRARY_PATH")


def env():
    values = {k: os.environ[k] for k in ENV_KEYS if k in os.environ}
    # NODE_ENV is set per build step: during installs, production would make
    # npm and pnpm skip devDependencies, where SvelteKit apps keep their tools.
    values.update({"CI": "1", "DO_NOT_TRACK": "1"})
    return values


def run(evidence, label, cwd, command, timeout=1800, stdin=None, extra_env=None):
    """Run under GNU time; return (status, log path)."""
    log, metrics = evidence / f"{label}.log", evidence / f"{label}.time"
    full_env = env()
    if extra_env:
        full_env.update(extra_env)
    (evidence / f"{label}.command.json").write_text(json.dumps({"cwd": str(cwd), "command": command}) + "\n")
    with log.open("wb") as out:
        try:
            status = subprocess.run([TIME, "-v", "-o", str(metrics), *command], cwd=cwd, env=full_env,
                                    stdin=stdin, stdout=out, stderr=subprocess.STDOUT, timeout=timeout).returncode
        except subprocess.TimeoutExpired:
            status = 124
    with log.open("a") as out:
        out.write("DONE\n" if status == 0 else f"FAIL rc={status}\n")
    return status, log


def package_manager(root, drop_lockfile=False):
    if drop_lockfile:
        package = json.loads((root / "package.json").read_text())
        version = package.get("packageManager", "pnpm@10").split("@", 1)[1].split("+")[0]
        return ["npm", "exec", "--yes", f"--package=pnpm@{version}", "--", "pnpm"], ["install", "--no-frozen-lockfile"]
    if (root / "pnpm-lock.yaml").exists():
        package = json.loads((root / "package.json").read_text())
        version = package.get("packageManager", "pnpm@10").split("@", 1)[1].split("+")[0]
        return ["npm", "exec", "--yes", f"--package=pnpm@{version}", "--", "pnpm"], ["install", "--frozen-lockfile"]
    if (root / "package-lock.json").exists():
        return ["npm"], ["ci", "--no-audit", "--no-fund"]
    raise RuntimeError(f"No supported lockfile in {root}")


def paths(name):
    project = BASE / name
    evidence = project / "evidence"
    evidence.mkdir(parents=True, exist_ok=True)
    return project, project / "repo", evidence


def setup(name, spec):
    project, repo, evidence = paths(name)
    if repo.exists():
        raise RuntimeError(f"{repo} exists; setup runs once")
    if "git" in spec:
        status, _ = run(evidence, "clone", project, ["git", "clone", "--depth", "1", spec["git"], "repo"])
    else:
        status, _ = run(evidence, "generate", project, spec["generate"])
    if status:
        return status
    return install(name, spec)


def install(name, spec):
    """(Re)install dependencies and run preparation steps; record versions."""
    project, repo, evidence = paths(name)
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo, text=True).strip() \
        if (repo / ".git").exists() else None
    record = {"name": name, "source": spec.get("git") or spec["generate"], "commit": commit, "app": spec["app"]}
    if spec.get("dropLockfile"):
        lock = repo / "pnpm-lock.yaml"
        if lock.exists():
            lock.rename(evidence / "pnpm-lock.yaml.upstream")
        record["lockfile"] = "dropped: upstream lockfile unreadable"
    tool, arguments = package_manager(repo, spec.get("dropLockfile", False))
    index = next_index(evidence, "install")
    status, _ = run(evidence, f"install-{index}", repo, tool + arguments)
    if status:
        return status
    for step_index, step in enumerate(spec.get("prepare", [])):
        status, _ = run(evidence, f"prepare-{index}-{step_index}", repo, tool + step)
        if status:
            return status
    app = repo / spec["app"]
    if spec.get("adapterNode"):
        version = spec["adapterNode"]
        status, _ = run(evidence, f"adapter-node-{index}", app, tool + ["add", "-D", f"@sveltejs/adapter-node@{version}"])
        if status:
            return status
        # Kit 2 configures the adapter in svelte.config.js, Kit 3 in vite.config.
        config = next(path for path in (app / "svelte.config.js", app / "vite.config.ts", app / "vite.config.js")
                      if path.exists())
        text = config.read_text()
        lines = [line for line in text.splitlines() if line.startswith("import adapter from '@sveltejs/adapter-")]
        if len(lines) != 1:
            raise RuntimeError("Expected one `import adapter from '@sveltejs/adapter-*'` line")
        config.write_text(text.replace(lines[0], "import adapter from '@sveltejs/adapter-node';"))
        record["variant"] = f"adapter replaced by @sveltejs/adapter-node@{version}: {lines[0]}"
    # Recorded source edits for a variant: (app-relative file, old, new), each
    # old text present exactly once.
    for file, old, new in spec.get("edits", []):
        path = app / file
        text = path.read_text()
        if text.count(old) != 1:
            raise RuntimeError(f"Edit target not found exactly once in {file}: {old!r}")
        path.write_text(text.replace(old, new))
    if spec.get("edits"):
        record["edits"] = spec["edits"]
    record["versions"] = {}
    for package in ("svelte", "@sveltejs/kit", "vite", "@sveltejs/vite-plugin-svelte", "@sveltejs/adapter-node"):
        manifest = app / "node_modules" / package / "package.json"
        if manifest.exists():
            record["versions"][package] = json.loads(manifest.read_text())["version"]
    (evidence / "project.json").write_text(json.dumps(record, indent=2) + "\n")
    return 0


def native_request(name, spec, output=None, request_only=False):
    project, repo, evidence = paths(name)
    request = {"op": "project", "project": {"root": str(repo / spec["app"]), "environment": {"NODE_ENV": "production"},
                                            "versionName": f"native-public-{name}"}}
    if request_only:
        request["requestOnly"] = True
    if output:
        request["outputDirectory"] = str(output)
    return request


def load(name, spec):
    project, repo, evidence = paths(name)
    request = evidence / "load.request.json"
    request.write_text(json.dumps(native_request(name, spec, request_only=True)))
    status, log = run(evidence, "load", repo, [str(NATIVE)], stdin=request.open("rb"))
    text = log.read_text().splitlines()
    response = json.loads(text[-2]) if len(text) >= 2 and text[-2].startswith("{") else None
    summary = {"ok": response and response.get("ok"), "error": response and response.get("error"),
               "requirements": response and response.get("requirements")}
    (evidence / "load.summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps({"name": name, **summary}, indent=1))
    return 0 if response and response.get("ok") else 1


def next_index(evidence, prefix):
    index = 1
    while (evidence / f"{prefix}-{index}.log").exists():
        index += 1
    return index


def official(name, spec):
    project, repo, evidence = paths(name)
    app = repo / spec["app"]
    index = next_index(evidence, "official")
    for stale in (".svelte-kit", "build", ".vercel", ".netlify"):
        shutil.rmtree(app / stale, ignore_errors=True)
    status, _ = run(evidence, f"official-{index}", app, ["node", str(app / "node_modules/vite/bin/vite.js"), "build"],
                    extra_env={"NODE_ENV": "production"})
    output = evidence / f"official-{index}-output"
    if (app / ".svelte-kit/output").exists():
        shutil.copytree(app / ".svelte-kit/output", output / "kit-output", symlinks=True)
    if (app / "build").exists():
        shutil.copytree(app / "build", output / "build", symlinks=True)
    return status


def native(name, spec):
    project, repo, evidence = paths(name)
    index = next_index(evidence, "native")
    output = evidence / f"native-{index}-output"
    request = evidence / f"native-{index}.request.json"
    request.write_text(json.dumps(native_request(name, spec, output=output)))
    status, log = run(evidence, f"native-{index}", repo, [str(NATIVE)], stdin=request.open("rb"),
                      extra_env={"NODE_ENV": "production"})
    return status


def main():
    if len(sys.argv) < 2 or sys.argv[1] in ("help", "--help"):
        print(__doc__)
        return 0 if len(sys.argv) > 1 else 2
    if sys.argv[1] == "list":
        print("\n".join(PROJECTS))
        return 0
    phase, names = sys.argv[1], sys.argv[2:] or list(PROJECTS)
    step = {"setup": setup, "install": install, "load": load, "official": official, "native": native}[phase]
    failures = 0
    for name in names:
        started = time.monotonic()
        status = step(name, PROJECTS[name])
        print(f"{phase} {name}: {'ok' if status == 0 else f'rc={status}'} ({time.monotonic() - started:.0f}s)", flush=True)
        failures += status != 0
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
