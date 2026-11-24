#!/usr/bin/env bash
# Strict OpenSpec and stale-change check.
#
# Two properties the release gate needs beyond the traceability check:
#
#  1. Every *active* change validates strictly (`openspec validate --strict`),
#     so a malformed delta or a duplicate requirement cannot land.
#  2. No active change still asserts a path that this workspace has deleted or
#     renamed. A stale proposal is worse than a missing one: it reads as
#     current intent and sends contributors to code that no longer exists.
#     Archived changes are historical and are not checked.
#
# Usage:
#   scripts/check-openspec.sh          # validate every active change
#   scripts/check-openspec.sh --list   # list active changes only
#
# Written for bash 3.2 (macOS `/bin/bash`).

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

CHANGES_DIR="openspec/changes"
LIST_ONLY=0
[ "${1:-}" = "--list" ] && LIST_ONLY=1

if [ ! -d "$CHANGES_DIR" ]; then
  echo "check-openspec: FAIL: $CHANGES_DIR not found" >&2
  exit 1
fi

# Paths that existed when older proposals were written and no longer do.
# Kept explicit (rather than "any missing path") so a proposal may still name
# files it plans to create.
STALE_PATTERNS=(
  "crates/core/"
  "crates/guests/"
  "crates/libs/"
  "ARCHITECTURE.md"
  "SUMMARY.md"
  "io/region.rs"
)

STATUS=0
ACTIVE=0
for dir in "$CHANGES_DIR"/*/; do
  [ -d "$dir" ] || continue
  name="$(basename "$dir")"
  [ "$name" = "archive" ] && continue
  ACTIVE=$((ACTIVE + 1))
  if [ "$LIST_ONLY" = "1" ]; then
    echo "$name"
    continue
  fi

  if [ -f "$dir/PARKED.md" ]; then
    if [ "$LIST_ONLY" = "0" ]; then
      echo "check-openspec: parked (strict delta validation skipped): $name"
    fi
  elif ! openspec validate "$name" --strict --no-interactive; then
    echo "check-openspec: FAIL: change '$name' is not valid under --strict" >&2
    STATUS=1
  fi

  for pattern in "${STALE_PATTERNS[@]}"; do
    hits="$(grep -rn --include='*.md' -F "$pattern" "$dir" 2>/dev/null || true)"
    if [ -n "$hits" ]; then
      echo "check-openspec: FAIL: change '$name' asserts removed path '$pattern':" >&2
      printf '%s\n' "$hits" >&2
      STATUS=1
    fi
  done
done

if [ "$LIST_ONLY" = "1" ]; then
  exit 0
fi

if [ "$ACTIVE" -eq 0 ]; then
  echo "check-openspec: FAIL: no active changes found" >&2
  exit 1
fi

if [ "$STATUS" -ne 0 ]; then
  echo "check-openspec: FAIL ($ACTIVE active changes)" >&2
  exit 1
fi

echo "check-openspec: PASS ($ACTIVE active changes, strict)"
