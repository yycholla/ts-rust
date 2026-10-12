#!/usr/bin/env bash
# Smoke test of a packed svelte-build npm tarball: installs it into a fresh
# `sv create` minimal app (SvelteKit 3, adapter swapped to adapter-node
# 6.0.0, as native-public.py sv-minimal-node), builds the app with the
# installed binary, starts the server and checks that / answers 200 with
# the app's HTML.
#
# Usage: release-smoke.sh TARBALL WORK_DIR
#   Needs node, npm and curl, and network access for npm.
set -euo pipefail
[[ $# == 2 && $1 != help ]] || { sed -n '2,9p' "$0" >&2; exit 2; }
tarball="$(cd -- "$(dirname -- "$1")" && pwd)/$(basename -- "$1")"
mkdir -p "$2"
work="$(cd -- "$2" && pwd)"
app="$work/app"
[[ ! -e $app ]] || { echo "error: $app exists" >&2; exit 1; }

(cd "$work" && npx --yes sv@1.1.1 create app --template minimal --types ts --no-add-ons --install npm)
cd "$app"
npm add -D --no-audit --no-fund @sveltejs/adapter-node@6.0.0
grep -q "^import adapter from '@sveltejs/adapter-auto';$" vite.config.ts
sed -i "s#^import adapter from '@sveltejs/adapter-auto';\$#import adapter from '@sveltejs/adapter-node';#" vite.config.ts
npm install --no-save --no-audit --no-fund "$tarball"
node -p 'Object.entries(Object.fromEntries(["svelte","@sveltejs/kit","vite","@sveltejs/adapter-node"].map(p=>[p,require(p+"/package.json").version]))).map(e=>e.join(" ")).join("\n")'

request="{\"op\":\"project\",\"project\":{\"root\":\"$app\",\"environment\":{\"NODE_ENV\":\"production\"}},\"outputDirectory\":\"$work/native-out\"}"
echo "$request" | NODE_ENV=production npx --no-install svelte-build > "$work/response.json"
node -e '
  const r = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8").trim().split("\n").pop());
  if (r.ok !== true || r.report?.status !== "complete") { console.error(JSON.stringify(r).slice(0, 4000)); process.exit(1); }
  console.log("native build complete:", JSON.stringify(r.timings));
' "$work/response.json"

port=${SMOKE_PORT:-43117}
PORT=$port HOST=127.0.0.1 node "$work/native-out/build/index.js" > "$work/server.log" 2>&1 &
server=$!
trap 'kill $server 2> /dev/null || true' EXIT
for _ in $(seq 50); do
  curl -sf -o "$work/index.html" "http://127.0.0.1:$port/" && break
  sleep 0.2
done
grep -q 'Welcome to SvelteKit' "$work/index.html" || { echo "error: / did not serve the app" >&2; cat "$work/server.log" >&2; exit 1; }
echo "smoke ok: / served by the native build"
