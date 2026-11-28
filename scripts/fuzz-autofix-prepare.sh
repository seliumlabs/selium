#!/usr/bin/env bash
# Prepare a gated auto-fix for a failed fuzz target.
#
# Usage: scripts/fuzz-autofix-prepare.sh <downloaded-crash-artifacts-dir>
#
# This phase needs no GitHub write credential. It reproduces and classifies the
# crash, mints a short-lived OpenCode token just before invoking the agent,
# applies the path allowlist, verifies the result, and commits locally. A
# separate publish step mints the GitHub App token and pushes the branch.
#
# Environment:
#   GH_TOKEN           optional read-only token, for duplicate-PR checks
#   OPENCODE_MODEL     model id (default: opencode-go/deepseek-v4.1-flash)
#   AUDIENCE           OpenCode OIDC audience (default: https://opencode.ai/console)
#   OPENCODE_CLIENT_ID OpenCode OAuth client id
#   BASE_BRANCH        base branch (default: main)
#   RUN_URL            link to the failed nightly run
#   GITHUB_OUTPUT      set by Actions; receives eligible/target/branch/hash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

if [ $# -ne 1 ]; then
  echo "usage: $0 <downloaded-crash-artifacts-dir>" >&2
  exit 2
fi

ARTIFACTS="$1"
NIGHTLY="${SELIUM_NIGHTLY_TOOLCHAIN:-nightly}"
OPENCODE_MODEL="${OPENCODE_MODEL:-opencode-go/deepseek-v4.1-flash}"
OPENCODE_CLIENT_ID="${OPENCODE_CLIENT_ID:-sk_c33f0e5c-9179-4a39-987f-c94e1331afa0}"
AUDIENCE="${AUDIENCE:-https://opencode.ai/console}"
BASE_BRANCH="${BASE_BRANCH:-main}"
RUN_URL="${RUN_URL:-}"

set_output() {
  if [ -n "${GITHUB_OUTPUT:-}" ]; then
    printf '%s\n' "$1" >> "$GITHUB_OUTPUT"
  fi
}

not_eligible() {
  echo "fuzz-autofix: $1"
  set_output "eligible=false"
  exit 0
}

if [ ! -d "$ARTIFACTS" ]; then
  not_eligible "no crash artifacts at $ARTIFACTS"
fi

CRASH="$(find "$ARTIFACTS" -type f -print | sort | head -n 1)"
if [ -z "$CRASH" ]; then
  not_eligible "no crash files in $ARTIFACTS"
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
  not_eligible "the crash did not reproduce; not patching"
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
  not_eligible "refusing to patch a non-Selium fault; see the attached log"
fi

BRANCH="fuzz-autofix/$TARGET-${HASH:0:12}"
if [ -n "${GH_TOKEN:-}" ]; then
  EXISTING_PR="$(gh pr list --head "$BRANCH" --state open --json number --jq '.[0].number // empty' 2>/dev/null || true)"
  if [ -n "$EXISTING_PR" ]; then
    not_eligible "PR #$EXISTING_PR already tracks $BRANCH"
  fi
fi

git checkout -B "$BRANCH"

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

if [ -z "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:-}" ] || [ -z "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ]; then
  echo "fuzz-autofix: OIDC request environment is missing" >&2
  exit 1
fi

echo "fuzz-autofix: minting a short-lived OpenCode token"
OIDC="$(curl -sSf --oauth2-bearer "$ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "$ACTIONS_ID_TOKEN_REQUEST_URL&audience=$AUDIENCE" | jq -r .value)"
OPENCODE_TOKEN="$(curl -sSf https://opencode.ai/console/auth/oauth/token \
  -d grant_type=urn:ietf:params:oauth:grant-type:token-exchange \
  -d subject_token_type=urn:ietf:params:oauth:token-type:jwt \
  -d client_id="$OPENCODE_CLIENT_ID" \
  -d subject_token="$OIDC" | jq -r .access_token)"
if [ -z "$OPENCODE_TOKEN" ] || [ "$OPENCODE_TOKEN" = "null" ]; then
  echo "fuzz-autofix: OpenCode token exchange returned no access token" >&2
  exit 1
fi
echo "::add-mask::$OPENCODE_TOKEN"
unset OIDC ACTIONS_ID_TOKEN_REQUEST_TOKEN ACTIONS_ID_TOKEN_REQUEST_URL

echo "fuzz-autofix: asking opencode ($OPENCODE_MODEL) to fix $TARGET"
# The agent gets the model token, but no GitHub credential and no OIDC request
# environment from which it could mint another one.
env -u GH_TOKEN -u GITHUB_TOKEN -u GIT_ASKPASS -u GIT_ASKPASS_TOKEN \
  -u ACTIONS_ID_TOKEN_REQUEST_TOKEN -u ACTIONS_ID_TOKEN_REQUEST_URL \
  OPENCODE_API_KEY="$OPENCODE_TOKEN" \
  opencode run --standalone --auto --archived \
    --model "$OPENCODE_MODEL" \
    --file "$LOG" \
    "$(cat "$PROMPT")"

if [ -z "$(git status --porcelain)" ]; then
  echo "fuzz-autofix: opencode made no changes; not opening a PR" >&2
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
  echo "fuzz-autofix: no staged changes; not opening a PR" >&2
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

set_output "eligible=true"
set_output "target=$TARGET"
set_output "branch=$BRANCH"
set_output "hash=$HASH"
echo "fuzz-autofix: prepared $BRANCH; the publish step will mint the App token"
