#!/usr/bin/env bash
# Packs the svelte-build npm package from npm/svelte-build and a release
# binary (scripts/svelte/release-build.sh output).
#
# Usage: npm-pack.sh BUILD_DIR NAME VERSION OUT_DIR
#   BUILD_DIR  holds svelte-build and BUILD.txt
#   NAME       npm package name, e.g. @<npm user>/svelte-build
#   VERSION    npm version (semver)
#   Writes OUT_DIR/package (the package dir) and OUT_DIR/<name>-<version>.tgz,
#   and prints the tarball path.
set -euo pipefail
[[ $# == 4 && $1 != help ]] || { sed -n '2,11p' "$0" >&2; exit 2; }
build=$1 name=$2 version=$3 out=$4
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$ ]] ||
  { echo "error: version $version is not semver" >&2; exit 1; }
pkg="$out/package"
rm -rf "$pkg"
mkdir -p "$pkg/bin"
install -m 755 "$build/svelte-build" "$pkg/bin/svelte-build"
cp "$build/BUILD.txt" "$pkg/BUILD.txt"
cp "$repo/npm/svelte-build/README.md" "$repo/LICENSE" "$repo/NOTICE.md" "$pkg/"
sed "s|@ts-rust/svelte-build|$name|g" "$repo/npm/svelte-build/README.md" > "$pkg/README.md"
node -e '
  const fs = require("fs");
  const [template, target, name, version] = process.argv.slice(1);
  const pkg = JSON.parse(fs.readFileSync(template, "utf8"));
  Object.assign(pkg, { name, version });
  fs.writeFileSync(target, JSON.stringify(pkg, null, 2) + "\n");
' "$repo/npm/svelte-build/package.json" "$pkg/package.json" "$name" "$version"
(cd "$out" && npm pack ./package --silent > pack.txt)
echo "$out/$(tail -1 "$out/pack.txt")"
