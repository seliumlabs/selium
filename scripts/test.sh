#!/usr/bin/env bash
# Run cargo test under Linux on Docker — the environment CI actually tests in.
#
# Why this exists: parts of the workspace only compile or behave on Linux.
# The os_wait_word module (Stage 2 wait-words) is cfg'd to
# linux/windows/freebsd — those tests never even build on a macOS host, and
# the futex-backed implementation can only be exercised on Linux. More
# broadly, std lock primitives differ per platform (see scripts/clippy.sh),
# so a green local test run does not imply a green Linux CI run. This script
# runs the exact CI test command inside a Linux container so local results
# match CI.
#
# The container layout mirrors CI: the workspace sits two levels below the
# mount point so the [patch.crates-io] `wasmtiny = { path = "../../wasmtiny" }`
# path resolves exactly as it does on the runner (proto/arch3 + wasmtiny).
#
# Usage:
#   scripts/test.sh                               # CI command: --workspace --all-targets
#   scripts/test.sh -p selium-kernel --lib        # one package, like CI's stage 2 job
#   scripts/test.sh -p selium-kernel --features stage2-wait-words --test wait_notify
#
# Environment overrides:
#   RUST_IMAGE       Docker image tag (default: rust:1.98, CI's current stable series)
#   TOOLCHAIN        Pin a rustup toolchain inside the container (e.g. 1.98.1)
#   DOCKER_PLATFORM  Emulate a platform (e.g. linux/amd64 to bit-match CI's x86_64
#                    runners; slow under qemu — arm64 Linux is usually enough,
#                    but wait-word/futex behaviour can differ per architecture)
#
# Named volumes cache state across runs: the toolchain and registry volumes
# are shared with scripts/clippy.sh (selium-clippy-rustup / -cargo), while
# selium-test-target is separate — test builds are full codegen and do not
# share units with clippy's metadata-only checks. The host target/ directory
# is never touched, so host and container artifacts never mix.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

# ---------------------------------------------------------------------------
# Resolve the CI-style layout: workspace at <mount>/<parent>/<repo>, with the
# wasmtiny checkout at <mount>/wasmtiny so `../../wasmtiny` resolves.
# ---------------------------------------------------------------------------
GRANDPARENT="$(cd "$ROOT/../.." && pwd -P)"
PARENT_NAME="$(basename "$(cd "$ROOT/.." && pwd -P)")"
REPO_NAME="$(basename "$ROOT")"
CONTAINER_WS="/work/$PARENT_NAME/$REPO_NAME"

if [ ! -d "$GRANDPARENT/wasmtiny" ]; then
  if [ -n "${WASMTINY_DIR:-}" ] && [ -d "$WASMTINY_DIR" ]; then
    WASMTINY_MOUNT=(-v "$(cd "$WASMTINY_DIR" && pwd -P)":/work/wasmtiny)
    echo "test.sh: using wasmtiny checkout at $WASMTINY_DIR"
  else
    echo "test.sh: $GRANDPARENT/wasmtiny not found." >&2
    echo "test.sh: the workspace [patch.crates-io] wasmtiny path dependency" >&2
    echo "test.sh: (../../wasmtiny) needs it. Point WASMTINY_DIR at a" >&2
    echo "test.sh: wasmtiny checkout, or place one next to $(cd "$ROOT/.." && pwd -P)." >&2
    exit 1
  fi
else
  WASMTINY_MOUNT=()
fi

# ---------------------------------------------------------------------------
# Ensure the Docker daemon is up (best effort on macOS via Docker Desktop).
# ---------------------------------------------------------------------------
if ! docker info >/dev/null 2>&1; then
  if [ "$(uname -s)" = "Darwin" ]; then
    echo "test.sh: starting Docker Desktop..."
    open -a Docker
    for _ in $(seq 1 60); do
      docker info >/dev/null 2>&1 && break
      sleep 2
    done
  fi
  if ! docker info >/dev/null 2>&1; then
    echo "test.sh: Docker daemon is not running." >&2
    exit 1
  fi
fi

# ---------------------------------------------------------------------------
# Assemble the run.
# ---------------------------------------------------------------------------
RUST_IMAGE="${RUST_IMAGE:-rust:1.98}"

PLATFORM_ARGS=()
if [ -n "${DOCKER_PLATFORM:-}" ]; then
  PLATFORM_ARGS=(--platform "$DOCKER_PLATFORM")
fi

TTY_ARGS=()
if [ -t 1 ]; then
  TTY_ARGS=(-t)
fi

# Default to the exact CI invocation; otherwise pass through the given args.
if [ $# -eq 0 ]; then
  set -- --workspace --all-targets
fi

TOOLCHAIN_SETUP="rustup component add clippy >/dev/null"
if [ -n "${TOOLCHAIN:-}" ]; then
  TOOLCHAIN_SETUP="rustup toolchain install '$TOOLCHAIN' --component clippy >/dev/null; export RUSTUP_TOOLCHAIN='$TOOLCHAIN'"
fi

# The ignored wasm-guest integration tests (spine, discovery, net_wake,
# control_plane, dns/quic spines, ...) build their guests for
# wasm32-unknown-unknown on the fly; the QUIC guests pull `ring`, whose C
# build needs clang; and fastpath_wake needs the atomics net-demo guest,
# which is a nightly + build-std build (see scripts/build-all.sh). CI's
# `test` job provisions none of these (the dedicated wasm jobs do), so
# install/build them here only when ignored tests are requested.
IGNORED_SETUP=":"
for _arg in "$@"; do
  if [[ "$_arg" == "--ignored" || "$_arg" == "--include-ignored" ]]; then
    IGNORED_SETUP="rustup target add wasm32-unknown-unknown >/dev/null; apt-get update -qq >/dev/null 2>&1 && apt-get install -y -qq clang >/dev/null 2>&1; rustup toolchain install nightly --component rust-src --target wasm32-unknown-unknown >/dev/null; scripts/build-all.sh --atomics-only"
    break
  fi
done

echo "test.sh: image=$RUST_IMAGE workspace=$CONTAINER_WS"
echo "test.sh: cargo test $*"

# The first run seeds the named volumes and builds the dependency tree with
# codegen (several minutes); subsequent runs are incremental.
docker run --rm \
  ${PLATFORM_ARGS[@]+"${PLATFORM_ARGS[@]}"} \
  ${TTY_ARGS[@]+"${TTY_ARGS[@]}"} \
  -v "$GRANDPARENT":/work \
  ${WASMTINY_MOUNT[@]+"${WASMTINY_MOUNT[@]}"} \
  -v selium-clippy-rustup:/usr/local/rustup \
  -v selium-clippy-cargo:/usr/local/cargo \
  -v selium-test-target:/target \
  -e WS="$CONTAINER_WS" \
  -e CARGO_INCREMENTAL=0 \
  -e CARGO_TARGET_DIR=/target \
  -e CARGO_TERM_COLOR=always \
  "$RUST_IMAGE" \
  bash -c '
    set -euo pipefail
    cd "$WS"
    # Resolve the toolchain AFTER cd: a rust-toolchain.toml in the workspace
    # (e.g. channel = "stable") selects the toolchain rustup installs the
    # clippy component into.
    { '"$TOOLCHAIN_SETUP"'; }
    { '"$IGNORED_SETUP"'; }
    exec cargo test "$@"
  ' bash "$@"
