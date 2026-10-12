#!/usr/bin/env bash
# One-time setup of npm trusted publishing for the svelte-build package. Run it by hand, logged in
# to npm (`npm login`) as the account that will own the package, with 2FA on (npm asks for a code).
#
#   1. Publishes 0.0.0-placeholder (dist-tag `placeholder`) when the package is not on npm yet:
#      npm can only trust a workflow for a package that exists.
#   2. Trusts .github/workflows/svelte-build-release.yml of the repo in the GitHub environment
#      `npm` to publish it. When the package already trusts something else, it stops and prints
#      the revoke command.
# npm drops a new trust that publishes nothing in 2 days, so run this shortly before the first
# svelte-build-v<version> tag. The workflow then publishes with OIDC: no npm token or secret.
#
# usage: npm/svelte-build/trust-setup.sh NAME [REPO]
#   NAME  the package name, as the repository variable SVELTE_BUILD_NPM_NAME
#   REPO  default yycholla/ts-rust
# needs npm 11.15.0 or later (npm trust)
set -euo pipefail
[[ ${1:-} && $1 != help && $1 != --help ]] || { sed -n '2,17p' "$0" >&2; exit 2; }
name=$1
repo=${2:-yycholla/ts-rust}
file=svelte-build-release.yml

npm_version=$(npm --version)
[[ $(printf '%s\n' 11.15.0 "$npm_version" | sort -V | head -1) == 11.15.0 ]] ||
  { echo "npm $npm_version: npm trust needs 11.15.0 or later (npm install -g npm@11)" >&2; exit 1; }
echo "npm user: $(npm whoami)"

if npm view "$name" version > /dev/null 2>&1; then
  echo "$name is on npm"
else
  dir=$(mktemp -d)
  cat > "$dir/package.json" << JSON
{
  "name": "$name",
  "version": "0.0.0-placeholder",
  "description": "Native SvelteKit build (ts-rust prototype). Placeholder: not released yet.",
  "license": "MIT AND Apache-2.0",
  "repository": { "type": "git", "url": "git+https://github.com/$repo.git" }
}
JSON
  (cd "$dir" && npm publish --access public --tag placeholder)
  rm -rf "$dir"
  echo "$name: placeholder 0.0.0-placeholder published"
fi

# `npm trust list --json` prints one JSON object per trust.
state=$(npm trust list "$name" --json | node -e '
  const text = require("fs").readFileSync(0, "utf8").trim();
  const trusts = text ? JSON.parse(`[${text.replace(/}\s*{/g, "},{")}]`) : [];
  const ok = trusts.some(t => t.type === "github" && t.repository === process.argv[1] &&
    t.file === process.argv[2] && t.environment === "npm" &&
    (t.permissions ?? []).includes("createPackage"));
  console.log(ok ? "ok" : trusts.length ? "other" : "none");
' "$repo" "$file")
case $state in
  none)
    npm trust github "$name" --file "$file" --repo "$repo" --env npm --allow-publish --yes ;;
  other)
    echo "$name trusts another repo, workflow, environment or permission:" >&2
    npm trust list "$name" >&2
    echo "revoke it with: npm trust revoke $name --id <id>, then run this again" >&2
    exit 1 ;;
esac
npm trust list "$name"
