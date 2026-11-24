#!/usr/bin/env bash
# Repeat-run / flake wrapper for the ignored integration pack.
#
# A single green run does not mean a test is sound: the pack has historically
# failed under load in `multithreaded`, `quic_spine`, and `deploy_slice`, and
# those failures were only visible when something re-ran the pack. This wrapper
# either:
#
#   * retries exactly the tests that failed (default, `--runs 1`), reporting any
#     test that only passes on retry as FLAKY, or
#   * runs the whole pack `--runs N` times and reports any test that fails in
#     some runs and passes in others as FLAKY.
#
# A test that fails every run is reported as failing, not flaky. Exit status is
# non-zero if any run failed, so the gate cannot go green on a retry.
#
# Usage:
#   scripts/flake-check.sh                 # pack once, retry failures once
#   scripts/flake-check.sh --runs 3        # pack three times, compare
#   scripts/flake-check.sh --no-retry      # pack once, no retry
#
# The pack is the documented one:
#   cargo test -p selium-runtime --tests --no-fail-fast -- --ignored \
#       --skip soak_ --skip format_truncation_ --skip format_corrupt_ \
#       --skip format_torn_
# The soak and crash lanes are long-running and belong to the scheduled lane
# (see scripts/gate.sh --soak), not to every merge gate. The crash lane is
# skipped by its `format_` test names, not by a `crash_` prefix: that would
# also skip the failure-injection test
# `crash_and_graceful_exit_both_revoke_the_registration`, which is part of this
# pack.
#
# Written for bash 3.2 (macOS `/bin/bash`): no associative arrays, no mapfile.

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

RUNS=1
RETRY=1

while [ $# -gt 0 ]; do
  case "$1" in
    --runs)
      shift
      [ $# -gt 0 ] || {
        echo "flake-check.sh: --runs needs a count" >&2
        exit 2
      }
      RUNS="$1"
      ;;
    --no-retry) RETRY=0 ;;
    -h | --help)
      sed -n '2,32p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "flake-check.sh: unknown argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

if ! printf '%s' "$RUNS" | grep -Eq '^[1-9][0-9]*$'; then
  echo "flake-check.sh: --runs must be a positive integer, got '$RUNS'" >&2
  exit 2
fi

LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/selium-flake-XXXXXX")"
cleanup() { rm -rf "$LOG_DIR"; }
trap cleanup EXIT

# Runs the pack, tees the output to $1, and records failures and the binary
# each failing test came from.
run_pack() {
  local log="$1"
  local failed_file="$2"
  local binary_file="$3"

  cargo test -p selium-runtime --tests --no-fail-fast -- \
    --ignored --skip soak_ --skip format_truncation_ --skip format_corrupt_ \
    --skip format_torn_ 2>&1 | tee "$log"
  local status="${PIPESTATUS[0]}"

  # A failing test prints `test <name> ... FAILED` under the `Running
  # tests/<stem>.rs` line for its binary; record both so a retry can target the
  # exact binary.
  awk '
    /^Running tests\/.*\.rs / {
      stem = $2
      sub(/^tests\//, "", stem)
      sub(/\.rs$/, "", stem)
    }
    /^test .* \.\.\. FAILED$/ {
      name = $0
      sub(/^test /, "", name)
      sub(/ \.\.\. FAILED$/, "", name)
      print name "\t" stem
    }
  ' "$log" | sort -u >"$binary_file"

  cut -f1 "$binary_file" >"$failed_file"
  return "$status"
}

# Retries one test inside its own binary. Returns 0 when the retry passes.
retry_test() {
  local test_name="$1"
  local stem="$2"
  echo "flake-check: retrying $test_name in tests/$stem.rs"
  cargo test -p selium-runtime --test "$stem" -- --ignored --exact "$test_name" 2>&1 |
    tee -a "$LOG_DIR/retry.log"
  return "${PIPESTATUS[0]}"
}

FAILED=0

for ((run = 1; run <= RUNS; run++)); do
  log="$LOG_DIR/run-$run.log"
  echo
  echo "flake-check: run $run/$RUNS"
  if run_pack "$log" "$LOG_DIR/failed-$run" "$LOG_DIR/binary-$run"; then
    echo "flake-check: run $run passed"
  else
    FAILED=1
    echo "flake-check: run $run FAILED"
  fi
  sed -n 's/^test \(.*\) \.\.\. ok$/\1/p' "$log" | sort -u >"$LOG_DIR/passed-$run"
done

# Every test that failed at least once, across all runs.
cat "$LOG_DIR"/failed-* 2>/dev/null | sed '/^$/d' | sort -u >"$LOG_DIR/all-failed"

FLAKY_FILE="$LOG_DIR/flaky"
CONSISTENT_FILE="$LOG_DIR/consistent"
: >"$FLAKY_FILE"
: >"$CONSISTENT_FILE"

while IFS= read -r name; do
  [ -n "$name" ] || continue

  # Did it pass in any run?
  passed_somewhere=0
  for ((run = 1; run <= RUNS; run++)); do
    if grep -qxF "$name" "$LOG_DIR/passed-$run" 2>/dev/null; then
      passed_somewhere=1
      break
    fi
  done

  if [ "$RUNS" -gt 1 ] && [ "$passed_somewhere" -eq 1 ]; then
    echo "$name" >>"$FLAKY_FILE"
    continue
  fi

  if [ "$RUNS" -eq 1 ] && [ "$RETRY" -eq 1 ]; then
    stem="$(awk -F'\t' -v name="$name" '$1 == name { print $2; exit }' "$LOG_DIR/binary-1")"
    if [ -n "$stem" ] && retry_test "$name" "$stem"; then
      echo "$name" >>"$FLAKY_FILE"
      continue
    fi
  fi

  echo "$name" >>"$CONSISTENT_FILE"
done <"$LOG_DIR/all-failed"

FLAKY_COUNT="$(wc -l <"$FLAKY_FILE" | tr -d ' ')"
CONSISTENT_COUNT="$(wc -l <"$CONSISTENT_FILE" | tr -d ' ')"

echo
if [ "$FLAKY_COUNT" -gt 0 ]; then
  echo "flake-check: FLAKY (passed on retry or in another run):"
  sed 's/^/  - /' "$FLAKY_FILE"
fi
if [ "$CONSISTENT_COUNT" -gt 0 ]; then
  echo "flake-check: FAILING:"
  sed 's/^/  - /' "$CONSISTENT_FILE"
fi
if [ "$FAILED" -eq 0 ]; then
  echo "flake-check: PASS ($RUNS run(s), no flakes)"
else
  echo "flake-check: FAIL ($FLAKY_COUNT flaky, $CONSISTENT_COUNT consistently failing)"
fi

# A flake is a gate failure: the spec requires a flaky test to be flagged, not
# reported green.
if [ "$FAILED" -ne 0 ] || [ "$FLAKY_COUNT" -gt 0 ]; then
  exit 1
fi
