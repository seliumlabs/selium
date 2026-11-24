#!/usr/bin/env bash
# Short, reproducible fuzz lane.
#
# Runs every hardening fuzz target against the checked-in seed corpus for a
# bounded wall-clock budget. This is a smoke lane, not a replacement for long
# fuzzing: it catches a target that crashes on its seeds or immediately after,
# and keeps the corpus and harness compiling in CI.
#
# Usage:
#   scripts/fuzz-smoke.sh                  # default budget per target
#   FUZZ_SECONDS=60 scripts/fuzz-smoke.sh  # longer local run
#   FUZZ_TARGETS="frame_codec" scripts/fuzz-smoke.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

FUZZ_SECONDS="${FUZZ_SECONDS:-15}"
FUZZ_TARGETS="${FUZZ_TARGETS:-hostcall_envelope multi_memory_header frame_codec module_probing}"
MAX_LEN="${FUZZ_MAX_LEN:-4096}"
NIGHTLY="${SELIUM_NIGHTLY_TOOLCHAIN:-nightly}"

if ! command -v cargo-fuzz >/dev/null 2>&1; then
  echo "fuzz-smoke: install cargo-fuzz (cargo install cargo-fuzz --locked)" >&2
  exit 2
fi
if ! rustc +"$NIGHTLY" --version >/dev/null 2>&1; then
  echo "fuzz-smoke: install the pinned nightly with rustup toolchain install $NIGHTLY" >&2
  exit 2
fi

for target in $FUZZ_TARGETS; do
  echo "fuzz-smoke: $target (${FUZZ_SECONDS}s, max_len=${MAX_LEN})"
  # `-runs=0` would only replay the corpus; the time budget keeps the lane
  # short while still allowing short discoveries after seed replay.
  cargo +"$NIGHTLY" fuzz run "$target" -- \
    -max_total_time="$FUZZ_SECONDS" \
    -max_len="$MAX_LEN" \
    -rss_limit_mb=2048 \
    -timeout=10
done

echo "fuzz-smoke: all targets passed"
