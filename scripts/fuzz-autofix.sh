#!/usr/bin/env bash
# Attempt a gated auto-fix for a failed fuzz target.
#
# Usage: scripts/fuzz-autofix.sh <downloaded-crash-artifacts-dir>
#
# The caller has already checked out the base branch, installed nightly,
# cargo-fuzz, symbolizer tools, and OpenCode. This script:
#   1. reproduces and classifies the first crash;
#   2. refuses to patch when the fault is in wasmtiny or cannot be attributed;
#   3. asks OpenCode to fix a Selium-owned fault;
#   4. rejects changes outside source/tests/seeds, verifies, and opens a draft PR.
#
# Environment:
#   GH_TOKEN           bot token (contents:write, pull-requests:write)
#   OPENCODE_API_KEY   provider key
#   OPENCODE_MODEL     model id (default: opencode-go/deepseek-v4.1-flash)
#   BASE_BRANCH        base branch (default: main)
#   RUN_URL            link to the failed nightly run

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

if [ $# -ne 1 ]; then
  echo "usage: $0 <downloaded-crash-artifacts-dir>" >&2
  exit 2
fi

ARTIFACTS="$1"
BASE_BRANCH="${BASE_BRANCH:-main}"
NIGHTLY="${SELIUM_NIGHTLY_TOOLCHAIN:-nightly}"
OPENCODE_MODEL="${OPENCODE_MODEL:-opencode-go/deepseek-v4.1-flash}"
RUN_URL="${RUN_URL:-}"

if [ -z "${GH_TOKEN:-}" ] || [ -z "${OPENCODE_API_KEY:-}" ]; then
  echo "fuzz-autofix: GH_TOKEN and OPENCODE_API_KEY are required" >&2
  exit 1
fi
if [ -z "${GITHUB_REPOSITORY:-}" ]; then
  echo "fuzz-autofix: GITHUB_REPOSITORY is required" >&2
  exit 1
fi
REMOTE_URL="https://x-access-token@github.com/${GITHUB_REPOSITORY}.git"

if [ ! -d "$ARTIFACTS" ]; then
  echo "fuzz-autofix: no crash artifacts at $ARTIFACTS"
  exit 0
fi

CRASH="$(find "$ARTIFACTS" -type f -print | sort | head -n 1)"
if [ -z "$CRASH" ]; then
  echo "fuzz-autofix: no crash files in $ARTIFACTS"
  exit 0
fi

ARTIFACT_DIR="$(basename "$(dirname "$CRASH")")"
TARGET=""
for candidate in hostcall_envelope multi_memory_header frame_codec module_probing; do
  case "$ARTIFACT_DIR" in
    fuzz-artifacts-"$candidate"-*) TARGET="$candidate" ;;
  esac
done
if [ -z "$TARGET" ]; then
  echo "fuzz-autofix: cannot determine the target from $ARTIFACT_DIR" >&2
  exit 1
fi

sha1_file() {
  if command -v sha1sum >/dev/null 2>&1; then
    sha1sum "$1" | cut -d' ' -f1
  else
    shasum -a 1 "$1" | cut -d' ' -f1
  fi
}

HASH="$(sha1_file "$CRASH")"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/selium-fuzz-autofix.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
LOG="$TMP/repro.log"

# Feed the token to git through an askpass helper rather than embedding it in a
# remote URL or leaving it in the checkout's credential store.
ASKPASS="$TMP/git-askpass.sh"
printf '#!/bin/sh\nprintf "%%s\\n" "$GIT_ASKPASS_TOKEN"\n' > "$ASKPASS"
chmod +x "$ASKPASS"
export GIT_ASKPASS="$ASKPASS"
export GIT_ASKPASS_TOKEN="$GH_TOKEN"
export GIT_TERMINAL_PROMPT=0

SYMBOLIZER="$(rustc +"$NIGHTLY" --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-symbolizer"
if [ -x "$SYMBOLIZER" ]; then
  export ASAN_SYMBOLIZER_PATH="$SYMBOLIZER"
fi
if [ -n "${ASAN_OPTIONS:-}" ]; then
  export ASAN_OPTIONS="$ASAN_OPTIONS:symbolize=1"
else
  export ASAN_OPTIONS="symbolize=1"
fi

echo "fuzz-autofix: reproducing $TARGET $HASH"
set +e
RUST_BACKTRACE=full cargo +"$NIGHTLY" fuzz run "$TARGET" "$CRASH" -- -runs=1 > "$LOG" 2>&1
REPRO_STATUS=$?
set -e

if [ "$REPRO_STATUS" -eq 0 ]; then
  echo "fuzz-autofix: the crash did not reproduce; not patching"
  exit 0
fi

if grep -qiE 'wasmtiny' "$LOG"; then
  OWNER=upstream
elif grep -Eq 'crates/|selium[_a-z0-9]*::|fuzz/fuzz_targets' "$LOG"; then
  OWNER=selium
else
  OWNER=unknown
fi

echo "fuzz-autofix: owner=$OWNER"
if [ "$OWNER" != "selium" ]; then
  echo "fuzz-autofix: refusing to patch a non-Selium fault; see the attached log"
  exit 0
fi

BRANCH="fuzz-autofix/$TARGET-${HASH:0:12}"
EXISTING_PR="$(gh pr list --head "$BRANCH" --state open --json number --jq '.[0].number // empty')"
if [ -n "$EXISTING_PR" ]; then
  echo "fuzz-autofix: PR #$EXISTING_PR already tracks $BRANCH; leaving it alone"
  exit 0
fi

git fetch "$REMOTE_URL" "$BASE_BRANCH"
git checkout -B "$BRANCH" FETCH_HEAD

PROMPT="$TMP/prompt.md"
{
  cat docs/fuzz-autofix-prompt.md
  echo
  echo "Target: $TARGET"
  echo "Reproducer: $CRASH"
  echo "Crash log: $LOG"
  if [ -n "$RUN_URL" ]; then
    echo "Failed run: $RUN_URL"
  fi
} > "$PROMPT"

echo "fuzz-autofix: asking opencode ($OPENCODE_MODEL) to fix $TARGET"
# Do not give the agent the GitHub write token.
env -u GH_TOKEN -u GITHUB_TOKEN -u GIT_ASKPASS -u GIT_ASKPASS_TOKEN \
  -u ACTIONS_ID_TOKEN_REQUEST_TOKEN -u ACTIONS_ID_TOKEN_REQUEST_URL \
  opencode run --standalone --auto --archived \
    --model "$OPENCODE_MODEL" \
    --file "$LOG" \
    "$(cat "$PROMPT")"

if [ -z "$(git status --porcelain)" ]; then
  echo "fuzz-autofix: opencode made no changes; not opening a PR"
  exit 1
fi

# Always include the reproducer as a named regression seed, unless an
# equivalent input is already present.
seed_present=0
for f in "fuzz/seeds/$TARGET"/*; do
  [ -f "$f" ] || continue
  if [ "$(sha1_file "$f")" = "$HASH" ]; then
    seed_present=1
    break
  fi
done
if [ "$seed_present" -eq 0 ]; then
  cp "$CRASH" "fuzz/seeds/$TARGET/autofix-$HASH"
fi

git add -A
CHANGED="$(git diff --cached --name-only)"
if [ -z "$CHANGED" ]; then
  echo "fuzz-autofix: no staged changes; not opening a PR"
  exit 1
fi

BAD=0
while IFS= read -r file; do
  [ -n "$file" ] || continue
  case "$file" in
    .github/*|scripts/*|.cargo/*|vendor/*|opencode.json|.opencode/*|rust-toolchain.toml|*/build.rs|Cargo.toml|Cargo.lock|*/Cargo.toml|*/Cargo.lock)
      echo "fuzz-autofix: forbidden path changed: $file" >&2
      BAD=1
      ;;
  esac
  case "$file" in
    crates/*|guests/*|fuzz/fuzz_targets/*|fuzz/seeds/*) ;;
    *)
      echo "fuzz-autofix: path outside the allowlist: $file" >&2
      BAD=1
      ;;
  esac
done <<< "$CHANGED"
if [ "$BAD" -ne 0 ]; then
  exit 1
fi

echo "fuzz-autofix: verifying the fix"
cargo +"$NIGHTLY" fuzz run "$TARGET" "$CRASH" -- -runs=1 > "$TMP/verify.log" 2>&1
FUZZ_TARGETS="$TARGET" FUZZ_SECONDS=5 scripts/fuzz-smoke.sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings

git config user.name "selium-fuzz-bot"
git config user.email "fuzz-bot@users.noreply.github.com"
git commit -m "Fix $TARGET fuzz crash" --quiet
git push --force "$REMOTE_URL" "$BRANCH"

BODY="$TMP/pr-body.md"
{
  echo "Automated fix attempt for a \`$TARGET\` fuzz crash."
  echo ""
  echo "- reproducer: \`$HASH\`"
  if [ -n "$RUN_URL" ]; then
    echo "- failed run: $RUN_URL"
  fi
  echo "- model: \`$OPENCODE_MODEL\`"
  echo ""
  echo "The change passed the local reproduction, the target smoke lane,"
  echo "formatting, and workspace clippy. The normal CI gate still applies;"
  echo "this PR is a draft and is not auto-merged."
} > "$BODY"

gh pr create \
  --draft \
  --base "$BASE_BRANCH" \
  --head "$BRANCH" \
  --title "[fuzz] Fix $TARGET crash" \
  --body-file "$BODY"
