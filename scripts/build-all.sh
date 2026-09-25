#!/usr/bin/env bash
# Build every crate in ./crates/ with the default (host) target, then every
# crate in ./guests/ and ./integration/ with the wasm32-unknown-unknown target.
# Finally, rebuild the net-demo guest with genuine wasm atomics (nightly +
# shared memory), which the ignored `fastpath_wake` test requires. The atomics
# artifact is
# written to a distinct `selium_net_demo_atomics.wasm` (cargo can only emit
# `selium_net_demo.wasm` for a crate named selium-net-demo, so the atomics
# module is copied aside), and the plain `selium_net_demo.wasm` is restored
# afterward — two artifact flavours cannot safely share one output path.
#
# Only workspace members are built: cargo metadata is used to skip crates that
# exist on disk but are not (yet) listed in the root workspace `members`.
# Package names are read from the `name` key in each crate's [package] section.

set -euo pipefail

# `--atomics-only` builds just the nightly atomics net-demo guest. That is
# what `scripts/test.sh -- --ignored` runs inside the container: the plain
# guests are built on the fly by their own tests via `read_guest_wasm`, so
# only the atomics flavour (which no test builds for itself) needs seeding.
ATOMICS_ONLY=0
if [[ "${1:-}" == "--atomics-only" ]]; then
  ATOMICS_ONLY=1
  shift
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Workspace package manifest paths, from cargo metadata. Used only for
# membership checks (e.g. guests/cluster is on disk but not a member yet).
META="$(cargo metadata --no-deps --format-version 1)"

# Prints the package name declared in a Cargo.toml's [package] section.
read_crate_name() {
  local manifest="$1"
  awk '
    /^\[package\]/ { in_pkg = 1; next }
    /^\[/          { in_pkg = 0 }
    in_pkg && /^name[[:space:]]*=/ {
      sub(/^[^"]*"/, "", $0)   # drop everything up to the opening quote
      sub(/".*$/, "", $0)     # drop from the closing quote onward
      print
      exit
    }
  ' "$manifest"
}

# True if the given absolute manifest path is a member of the workspace.
# Quoted sections of a [[ ]] pattern are matched literally, so glob
# characters in the path are safe.
is_workspace_member() {
  local manifest="$1"
  [[ "$META" == *"\"manifest_path\":\"$manifest\""* ]]
}

# Usage: build_dir <base-dir> [extra cargo args...]
# Finds every crate under <base-dir> (recursively) and builds all workspace
# members among them in a single cargo invocation.
build_dir() {
  local base="$1"
  shift
  local base_abs="$ROOT/$base"
  local specs=()

  [ -d "$base_abs" ] || {
    echo "error: $base_abs not found" >&2
    exit 1
  }

  while IFS= read -r manifest; do
    if ! is_workspace_member "$manifest"; then
      echo "skipping $manifest (not a workspace member)" >&2
      continue
    fi
    local name
    name="$(read_crate_name "$manifest")"
    if [ -z "$name" ]; then
      echo "error: no package name found in $manifest" >&2
      exit 1
    fi
    specs+=("-p" "$name")
  done < <(find "$base_abs" -name Cargo.toml -type f | sort)

  if [ "${#specs[@]}" -eq 0 ]; then
    echo "error: no crates found under $base" >&2
    exit 1
  fi

  echo "Building ${#specs[@]} crates from $base${*:+ with: $*}"
  cargo build "${specs[@]}" "$@"
}

# ---------------------------------------------------------------------------
# Atomics guest build
# ---------------------------------------------------------------------------
# The ignored `fastpath_wake` test needs a net-demo guest whose module both
# (a) declares memory 0 shared with a maximum and (b) emits a genuine
# `memory.atomic.notify` on the ring generation word. Matching CI's
# shared-page-fastpath job:
#
# - `+atomics` must cover the ENTIRE dependency graph, not just the crates
#   that emit atomics. wasm-ld refuses a `--shared-memory` link unless every
#   object it consumes was compiled with `+atomics` (or `+bulk-memory`), and
#   cargo has no per-package `-C target-feature` scoping, so RUSTFLAGS cannot
#   be narrowed to selium-memory/selium-shm. Every crate in the invocation
#   inherits the flag by requirement, not by mistake.
#
#   The observable consequence is rustc's
#     warning: unstable feature specified for `-Ctarget-feature`: `atomics`
#   emitted once per crate (cargo folds the rest into "1 duplicate").
#   `+atomics` is a real but still-unstable target feature, and the warning
#   is expected/harmless. It is a rustc session diagnostic rather than a
#   named lint, so it cannot be `-A<lint>`-allowed; `-Awarnings` would also
#   mask genuine diagnostics, so it is deliberately NOT used. Leave it noisy.
#
# - `-Zbuild-std` rebuilds std with the same `+atomics` feature. The prebuilt
#   wasm std was compiled without it, and whichever std a `--shared-memory`
#   link picks up must carry the feature or wasm-ld rejects the link. This is
#   the part that needs the `rust-src` rustup component.
#
# - `--shared-memory` / `--max-memory` declare memory 0 shared with a maximum
#   (the wasmtiny validator requirement). They only affect the final link.
#
# - `.cargo/config.toml` appends its own wasm32 `getrandom_backend="custom"`
#   rustflag; it is combined with (never replaced by) the RUSTFLAGS below.
ATOMICS_RUSTFLAGS="\
-C target-feature=+atomics,+bulk-memory,+mutable-globals \
-C link-arg=--shared-memory \
-C link-arg=--max-memory=1073741824"

build_atomics_guest() {
  local target="wasm32-unknown-unknown"
  local sysroot
  # Respect `CARGO_TARGET_DIR` (set to the container's persistent `/target`
  # volume by scripts/test.sh); fall back to the workspace-local `target/`.
  local target_root="${CARGO_TARGET_DIR:-$ROOT/target}"

  if ! rustc +nightly --version >/dev/null 2>&1; then
    echo "error: the atomics guest build needs a nightly toolchain" >&2
    echo "  rustup toolchain install nightly --component rust-src --target $target" >&2
    exit 1
  fi

  sysroot="$(rustc +nightly --print sysroot)"
  if [ ! -f "$sysroot/lib/rustlib/src/rust/library/Cargo.toml" ]; then
    echo "error: the atomics guest build needs the rust-src component (for -Zbuild-std)" >&2
    echo "  rustup component add rust-src --toolchain nightly" >&2
    exit 1
  fi

  # The plain guest build above may have left a non-atomics artifact at the
  # shared output path; removing it first forces cargo to re-emit the cached
  # atomics module even when its fingerprint is otherwise still fresh.
  echo "Building atomics net-demo guest"
  rm -f "$target_root/$target/debug/selium_net_demo.wasm"

  RUSTFLAGS="$ATOMICS_RUSTFLAGS" \
    cargo +nightly build -Zbuild-std=std,panic_abort \
      --target "$target" \
      -p selium-net-demo \
      --features selium-guest/nightly-wasm-atomics

  # Cargo can only emit `selium_net_demo.wasm` for a crate named
  # selium-net-demo, and two flavours (plain vs atomics) cannot safely share
  # one output path. Keep the atomics module under its own name for
  # `fastpath_wake`, then restore the plain module that `net_wake` (and the
  # host demo) expect. The preceding build overwrote the plain artifact in
  # place, so removing it forces cargo to re-emit it.
  cp "$target_root/$target/debug/selium_net_demo.wasm" \
    "$target_root/$target/debug/selium_net_demo_atomics.wasm"
  rm -f "$target_root/$target/debug/selium_net_demo.wasm"
  cargo build --target "$target" -p selium-net-demo
}

# ---------------------------------------------------------------------------
# Multithreaded (mt-demo) guest build
# ---------------------------------------------------------------------------
# The ignored `sdk_mt_demo_guest_runs_two_cpu_bound_tasks_in_parallel` test
# needs the `selium-mt-demo` guest built with the same atomics target as the
# net-demo atomics flavour, PLUS `--export=__stack_pointer`. That export is the
# wasm-threads shadow-stack convention: the runtime's engine resolves the
# module's stack-pointer global from it and gives each concurrent invocation
# its own shadow stack, so two workers entering one shared instance do not
# overlap frames. Without the export the engine cannot identify the stack
# global and leaves the module on the shared stack (unsafe to enter
# concurrently). The atomics module is kept at a distinct
# `selium_mt_demo_atomics.wasm` so it does not collide with a plain build.
build_mt_demo_guest() {
  local target="wasm32-unknown-unknown"
  local target_root="${CARGO_TARGET_DIR:-$ROOT/target}"

  if ! rustc +nightly --version >/dev/null 2>&1; then
    echo "error: the mt-demo atomics build needs a nightly toolchain" >&2
    echo "  rustup toolchain install nightly --component rust-src --target $target" >&2
    exit 1
  fi

  echo "Building atomics mt-demo guest"
  rm -f "$target_root/$target/debug/selium_mt_demo.wasm"

  RUSTFLAGS="$ATOMICS_RUSTFLAGS -C link-arg=--export=__stack_pointer" \
    cargo +nightly build -Zbuild-std=std,panic_abort \
      --target "$target" \
      -p selium-mt-demo \
      --features selium-guest/nightly-wasm-atomics

  cp "$target_root/$target/debug/selium_mt_demo.wasm" \
    "$target_root/$target/debug/selium_mt_demo_atomics.wasm"
}

if [ "$ATOMICS_ONLY" = "1" ]; then
  build_atomics_guest
  build_mt_demo_guest
  exit 0
fi

build_dir crates
build_dir guests --target wasm32-unknown-unknown
build_dir integration --target wasm32-unknown-unknown
build_atomics_guest
build_mt_demo_guest
