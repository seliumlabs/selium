#!/usr/bin/env bash
# Build a reviewed seed-candidate directory for one fuzz target.
#
# Usage: scripts/fuzz-seed-candidate.sh <target> <output-dir>
#
# The candidate contains only content-addressed inputs that add fuzz coverage
# beyond the checked-in seeds. It is uploadable as a workflow artifact; the
# PR job decides whether to open or update the per-target branch.
#
# Environment:
#   FUZZ_SEED_MIN_FEATURE_DELTA  minimum increase in libFuzzer `ft` (default 1)
#   FUZZ_SEED_MAX_FILES          maximum new files in one candidate (default 100)
#   FUZZ_SEED_MAX_BYTES          maximum new bytes in one candidate (default 262144)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

if [ $# -ne 2 ]; then
  echo "usage: $0 <target> <output-dir>" >&2
  exit 2
fi

TARGET="$1"
OUT_DIR="$2"
SEEDS="fuzz/seeds/$TARGET"
CORPUS="fuzz/corpus/$TARGET"
MIN_DELTA="${FUZZ_SEED_MIN_FEATURE_DELTA:-1}"
MAX_FILES="${FUZZ_SEED_MAX_FILES:-100}"
MAX_BYTES="${FUZZ_SEED_MAX_BYTES:-262144}"
NIGHTLY="${SELIUM_NIGHTLY_TOOLCHAIN:-nightly}"

if [ ! -d "$SEEDS" ]; then
  echo "fuzz-seed-candidate: no seed directory at $SEEDS" >&2
  exit 1
fi
if [ ! -d "$CORPUS" ] || [ -z "$(find "$CORPUS" -type f -print -quit)" ]; then
  echo "fuzz-seed-candidate: no working corpus for $TARGET"
  exit 0
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/selium-seed-candidate.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

sha1_file() {
  if command -v sha1sum >/dev/null 2>&1; then
    sha1sum "$1" | cut -d' ' -f1
  else
    shasum -a 1 "$1" | cut -d' ' -f1
  fi
}

coverage_ft() {
  local dir="$1"
  cargo +"$NIGHTLY" fuzz run "$TARGET" "$dir" -- -runs=0 2>&1 |
    sed -n 's/.*DONE.*ft: \([0-9][0-9]*\).*/\1/p' |
    tail -n 1
}

# Minimise the union of the seeds and the working corpus. cmin takes one
# directory, so build the union first.
cp -R "$SEEDS"/. "$TMP"/
cp -R "$CORPUS"/. "$TMP"/
cargo +"$NIGHTLY" fuzz cmin "$TARGET" "$TMP"

OLD_FT="$(coverage_ft "$SEEDS")"
NEW_FT="$(coverage_ft "$TMP")"
if [ -z "$OLD_FT" ] || [ -z "$NEW_FT" ]; then
  echo "fuzz-seed-candidate: could not read coverage counts for $TARGET" >&2
  exit 1
fi

DELTA=$((NEW_FT - OLD_FT))
if [ "$DELTA" -lt "$MIN_DELTA" ]; then
  echo "fuzz-seed-candidate: $TARGET adds no significant coverage (ft $OLD_FT -> $NEW_FT)"
  exit 0
fi

HASHES="$TMP.hashes"
: > "$HASHES"
while IFS= read -r -d '' f; do
  sha1_file "$f" >> "$HASHES"
done < <(find "$SEEDS" -type f -print0)
sort -u -o "$HASHES" "$HASHES"

rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

ADDED=0
BYTES=0
for f in "$TMP"/*; do
  [ -f "$f" ] || continue
  hash="$(sha1_file "$f")"
  if grep -qx "$hash" "$HASHES"; then
    continue
  fi
  if [ "$ADDED" -ge "$MAX_FILES" ] || [ "$BYTES" -ge "$MAX_BYTES" ]; then
    echo "fuzz-seed-candidate: $TARGET candidate exceeds the size cap; skipping"
    rm -rf "$OUT_DIR"
    exit 0
  fi
  cp "$f" "$OUT_DIR/$hash"
  echo "$hash" >> "$HASHES"
  ADDED=$((ADDED + 1))
  BYTES=$((BYTES + $(wc -c < "$f")))
done

if [ "$ADDED" -eq 0 ]; then
  echo "fuzz-seed-candidate: $TARGET has no new content after minimisation"
  rm -rf "$OUT_DIR"
  exit 0
fi

cat > "$OUT_DIR/meta.txt" <<EOF
target=$TARGET
delta=$DELTA
files=$ADDED
bytes=$BYTES
old_ft=$OLD_FT
new_ft=$NEW_FT
EOF

echo "fuzz-seed-candidate: $TARGET +$DELTA ft, $ADDED files, $BYTES bytes"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    echo "### Seed candidate: $TARGET"
    echo ""
    echo "- coverage features: $OLD_FT -> $NEW_FT (+$DELTA)"
    echo "- new seeds: $ADDED files, $BYTES bytes"
  } >> "$GITHUB_STEP_SUMMARY"
fi
