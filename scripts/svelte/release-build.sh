#!/usr/bin/env bash
# Builds the release svelte-build binary for linux-x64: a glibc build that
# needs glibc 2.28 or later (Debian 10, Ubuntu 20.04, RHEL 8 and later), the
# floor of the tsc release (npm/README.md, "Linux libc").
# .github/workflows/svelte-build-release.yml runs it in the manylinux_2_28
# container (AlmaLinux 8, glibc 2.28), so the binary links against glibc 2.28;
# it checks that no GLIBC_ symbol version is above SVELTE_BUILD_GLIBC_FLOOR.
#
# Not static musl like the tsc release (npm/README.md, "Linux libc"): the
# build uses the system allocator, and musl's malloc would slow the threaded
# bundle and compression phases, so musl timings would not measure the build.
#
# Usage: release-build.sh OUT_DIR
#   Writes OUT_DIR/svelte-build (stripped) and OUT_DIR/BUILD.txt.
# Env: SVELTE_BUILD_GLIBC_FLOOR  default 2.28
#      CARGO_BUILD_JOBS          passed to cargo as usual
set -euo pipefail
[[ ${1:-} && $1 != help && $1 != --help ]] || { sed -n '2,16p' "$0" >&2; exit 2; }
out=$1
floor=${SVELTE_BUILD_GLIBC_FLOOR:-2.28}
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
manifest="$repo/crates/ts_svelte_build/Cargo.toml"

mkdir -p "$out"
cargo build --release --locked --manifest-path "$manifest" --bin svelte-build
target_dir="$(cargo metadata --format-version 1 --no-deps --manifest-path "$manifest" |
  python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
cp "$target_dir/release/svelte-build" "$out/svelte-build"
strip "$out/svelte-build"

needed="$(objdump -T "$out/svelte-build" | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -1)"
if [[ $(printf '%s\n' "$floor" "$needed" | sort -V | tail -1) != "$floor" ]]; then
  echo "error: svelte-build needs GLIBC_$needed, above the floor $floor" >&2
  exit 1
fi
interpreter="$(readelf -l "$out/svelte-build" | sed -n 's/.*Requesting program interpreter: \(.*\)]/\1/p')"
[[ $interpreter == /lib64/ld-linux-x86-64.so.2 ]] ||
  { echo "error: unexpected program interpreter $interpreter" >&2; exit 1; }

# The binary answers the capabilities op without reading a project.
echo '{"op":"capabilities"}' | "$out/svelte-build" > "$out/capabilities.json"
python3 -c 'import json,sys; assert json.load(open(sys.argv[1]))["ok"] is True' "$out/capabilities.json"

{
  echo "git: $(git -c safe.directory="$repo" -C "$repo" rev-parse HEAD)"
  echo "rustc: $(rustc --version)"
  echo "glibc floor: $floor (needs GLIBC_$needed)"
  echo "sha256: $(sha256sum "$out/svelte-build" | cut -d' ' -f1)"
} > "$out/BUILD.txt"
cat "$out/BUILD.txt"
