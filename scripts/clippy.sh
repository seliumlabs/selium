#!/usr/bin/env bash
# Run Clippy under Linux on Docker — the environment CI actually lints in.
#
# Why this exists: Clippy's `unnecessary_lazy_evaluations` (and every other
# lint built on `has_significant_drop`, e.g. `or_fun_call`) is
# platform-dependent. The lint checks whether the closure body's type has a
# significant destructor before suggesting eager evaluation, and the answer
# depends on std's per-platform lock implementations:
#
#   - Linux   -> std::sync::Mutex is futex-backed: a bare atomic with no
#                `Drop` impl -> not "significant" -> the lint fires.
#   - macOS   -> std::sync::Mutex is pthread-backed with an explicit
#                `impl Drop` (pthread_mutex_destroy) -> "significant"
#                -> the lint is silently suppressed.
#
# So any `Option<T>` whose `T` transitively contains a lock primitive (e.g.
# through tokio::sync::Notify, which holds a std Mutex for its waiter list)
# lints differently across platforms. A macOS host can therefore show a
# clean clippy run for code that CI (Linux) rejects, and vice versa. This
# script runs the exact CI clippy command inside a Linux container so local
# results match CI.
#
# Usage:
#   scripts/clippy.sh                          # CI command: --workspace --all-targets -- -D warnings
#   scripts/clippy.sh -p selium-connector-http --all-targets -- -D warnings
#
# Environment overrides:
#   RUST_IMAGE       Docker image tag (default: rust:latest, CI's current stable series)
#   TOOLCHAIN        Pin a rustup toolchain inside the container (e.g. 1.98.1)
#   DOCKER_PLATFORM  Emulate a platform (e.g. linux/amd64 to bit-match CI's x86_64
#                    runners; slow under qemu — arm64 Linux already reproduces the
#                    futex-vs-pthread divergence since it is target_os-driven)
#
# Named volumes selium-clippy-{rustup,cargo,target} cache the toolchain,
# registry, and build artifacts across runs; the host target/ directory is
# never touched (CARGO_TARGET_DIR is redirected into the container volume,
# so host and container artifacts never mix).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

# ---------------------------------------------------------------------------
# Resolve the CI-style layout: workspace at <mount>/<parent>/<repo>.
# ---------------------------------------------------------------------------
GRANDPARENT="$(cd "$ROOT/../.." && pwd -P)"
PARENT_NAME="$(basename "$(cd "$ROOT/.." && pwd -P)")"
REPO_NAME="$(basename "$ROOT")"
CONTAINER_WS="/work/$PARENT_NAME/$REPO_NAME"

# ---------------------------------------------------------------------------
# Ensure the Docker daemon is up (best effort on macOS via Docker Desktop).
# ---------------------------------------------------------------------------
if ! docker info >/dev/null 2>&1; then
  if [ "$(uname -s)" = "Darwin" ]; then
    echo "clippy.sh: starting Docker Desktop..."
    open -a Docker
    for _ in $(seq 1 60); do
      docker info >/dev/null 2>&1 && break
      sleep 2
    done
  fi
  if ! docker info >/dev/null 2>&1; then
    echo "clippy.sh: Docker daemon is not running." >&2
    exit 1
  fi
fi

# ---------------------------------------------------------------------------
# Assemble the run.
# ---------------------------------------------------------------------------
RUST_IMAGE="${RUST_IMAGE:-rust:latest}"

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
  set -- --workspace --all-targets -- -D warnings
fi

TOOLCHAIN_SETUP="rustup component add clippy >/dev/null"
if [ -n "${TOOLCHAIN:-}" ]; then
  TOOLCHAIN_SETUP="rustup toolchain install '$TOOLCHAIN' --component clippy >/dev/null; export RUSTUP_TOOLCHAIN='$TOOLCHAIN'"
fi

echo "clippy.sh: image=$RUST_IMAGE workspace=$CONTAINER_WS"
echo "clippy.sh: cargo clippy $*"

# The first run seeds the named volumes from the image and downloads the
# stable toolchain + registry deps (several minutes); subsequent runs are
# incremental.
docker run --rm \
  ${PLATFORM_ARGS[@]+"${PLATFORM_ARGS[@]}"} \
  ${TTY_ARGS[@]+"${TTY_ARGS[@]}"} \
  -v "$GRANDPARENT":/work \
  -v selium-clippy-rustup:/usr/local/rustup \
  -v selium-clippy-cargo:/usr/local/cargo \
  -v selium-clippy-target:/target \
  -e WS="$CONTAINER_WS" \
  -e CARGO_INCREMENTAL=0 \
  -e CARGO_TARGET_DIR=/target \
  -e CARGO_TERM_COLOR=always \
  "$RUST_IMAGE" \
  bash -c '
    set -euo pipefail
    apt update
    apt -y install cmake
    cd "$WS"
    # Resolve the toolchain AFTER cd: a rust-toolchain.toml in the workspace
    # (e.g. channel = "stable") selects the toolchain rustup installs the
    # clippy component into.
    { '"$TOOLCHAIN_SETUP"'; }
    exec cargo clippy "$@"
  ' bash "$@"
