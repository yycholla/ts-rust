#!/usr/bin/env bash
# Runs scripts/svelte/release-build.sh in the manylinux_2_28 container (IMAGE), as
# .github/workflows/svelte-build-release.yml does, to reproduce the release
# binary locally. The checkout is mounted read-only; cargo's target and home
# live in WORK_DIR. The output is chowned to the caller.
#
# Usage: release-build-docker.sh WORK_DIR
#   Writes WORK_DIR/out/svelte-build and WORK_DIR/out/BUILD.txt. The log's last
#   line is DONE or FAIL rc=<N> when run through systemd-run with a log.
# Env: RUST_VERSION (default 1.98.1), CARGO_BUILD_JOBS (default 12),
#      IMAGE (default: the digest the workflow pins)
set -euo pipefail
[[ ${1:-} && $1 != help && $1 != --help ]] || { sed -n '2,12p' "$0" >&2; exit 2; }
work="$(mkdir -p "$1" && cd -- "$1" && pwd)"
image=${IMAGE:-quay.io/pypa/manylinux_2_28_x86_64@sha256:4131ddd7acb9c6c966af02fe7e71edc51e078c2d41f475966697676430bfa2ae}
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
docker run --rm -v "$repo:/src:ro" -v "$work:/work" \
  -e CARGO_HOME=/work/cargo -e RUSTUP_HOME=/work/rustup -e CARGO_TARGET_DIR=/work/target \
  -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-12}" -e RUST_VERSION="${RUST_VERSION:-1.98.1}" \
  -e HOST_UID="$(id -u)" -e HOST_GID="$(id -g)" \
  "$image" bash -euo pipefail -c '
    [[ -x /work/cargo/bin/cargo ]] || curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "$RUST_VERSION" --no-modify-path
    export PATH=/work/cargo/bin:$PATH
    rustup default "$RUST_VERSION"
    status=0
    /src/scripts/svelte/release-build.sh /work/out || status=$?
    chown -R "$HOST_UID:$HOST_GID" /work
    exit $status'
