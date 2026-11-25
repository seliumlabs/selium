#!/usr/bin/env bash
# Short, reproducible fuzz lane.
#
# Runs every hardening fuzz target against the checked-in seed corpus for a
# bounded wall-clock budget. This is a smoke lane, not a replacement for long
# fuzzing: it catches a target that crashes on its seeds or immediately after,
# and keeps the corpus and harness compiling in CI.
#
# Checked-in seeds live in fuzz/seeds/<target>/; inputs discovered by a run are
# written to the gitignored fuzz/corpus/<target>/ so a run never dirties the
# tree. Failure triage and the local reproduction workflow are documented in
# docs/fuzzing-playbook.md.
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
WIRE_MAX_LEN=4096
MODULE_MAX_LEN=262144
NIGHTLY="${SELIUM_NIGHTLY_TOOLCHAIN:-nightly}"

if ! command -v cargo-fuzz >/dev/null 2>&1; then
  echo "fuzz-smoke: install cargo-fuzz (cargo install cargo-fuzz --locked)" >&2
  exit 2
fi
if ! rustc +"$NIGHTLY" --version >/dev/null 2>&1; then
  echo "fuzz-smoke: install the pinned nightly with rustup toolchain install $NIGHTLY" >&2
  exit 2
fi

# Wire parsers are bounded at 4 KiB; module_probing accepts modules up to
# 256 KiB. FUZZ_MAX_LEN overrides both for a one-off run.
default_max_len() {
  case "$1" in
    module_probing) echo "$MODULE_MAX_LEN" ;;
    *) echo "$WIRE_MAX_LEN" ;;
  esac
}

for target in $FUZZ_TARGETS; do
  max_len="${FUZZ_MAX_LEN:-$(default_max_len "$target")}"
  echo "fuzz-smoke: $target (${FUZZ_SECONDS}s, max_len=${max_len})"
  mkdir -p "fuzz/corpus/$target"
  # The first corpus directory is writable and gitignored; the checked-in
  # seeds are passed as read-only extra corpora.
  corpora=("fuzz/corpus/$target")
  if [ -d "fuzz/seeds/$target" ]; then
    corpora+=("fuzz/seeds/$target")
  fi
  # `-runs=0` would only replay the corpus; the time budget keeps the lane
  # short while still allowing short discoveries after seed replay.
  cargo +"$NIGHTLY" fuzz run "$target" "${corpora[@]}" -- \
    -max_total_time="$FUZZ_SECONDS" \
    -max_len="$max_len" \
    -rss_limit_mb=2048 \
    -timeout=10
done

echo "fuzz-smoke: all targets passed"
