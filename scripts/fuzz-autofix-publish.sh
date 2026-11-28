#!/usr/bin/env bash
# Publish a prepared fuzz auto-fix.
#
# Usage: scripts/fuzz-autofix-publish.sh
#
# Run after scripts/fuzz-autofix-prepare.sh has committed a branch locally.
# This is the only auto-fix phase that needs a GitHub App installation token,
# so it is minted immediately before this short push/PR step.
#
# Environment:
#   GH_TOKEN          GitHub App installation token (contents/pull-requests write)
#   GITHUB_REPOSITORY owner/repo
#   TARGET            fuzz target
#   BRANCH            prepared branch
#   HASH              reproducer hash
#   BASE_BRANCH       base branch (default: main)
#   RUN_URL           failed-run link
#   OPENCODE_MODEL    model display in the PR body

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

if [ -z "${GH_TOKEN:-}" ] || [ -z "${GITHUB_REPOSITORY:-}" ]; then
  echo "fuzz-autofix-publish: GH_TOKEN and GITHUB_REPOSITORY are required" >&2
  exit 1
fi
if [ -z "${TARGET:-}" ] || [ -z "${BRANCH:-}" ] || [ -z "${HASH:-}" ]; then
  echo "fuzz-autofix-publish: TARGET, BRANCH and HASH are required" >&2
  exit 1
fi

BASE_BRANCH="${BASE_BRANCH:-main}"
RUN_URL="${RUN_URL:-}"
OPENCODE_MODEL="${OPENCODE_MODEL:-opencode-go/deepseek-v4.1-flash}"

git rev-parse --verify "$BRANCH^{commit}" >/dev/null

TMP="$(mktemp -d "${TMPDIR:-/tmp}/selium-fuzz-publish.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
ASKPASS="$TMP/git-askpass.sh"
printf '#!/bin/sh\nprintf "%%s\\n" "$GIT_ASKPASS_TOKEN"\n' > "$ASKPASS"
chmod +x "$ASKPASS"
export GIT_ASKPASS="$ASKPASS"
export GIT_ASKPASS_TOKEN="$GH_TOKEN"
export GIT_TERMINAL_PROMPT=0
REMOTE_URL="https://x-access-token@github.com/${GITHUB_REPOSITORY}.git"

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

EXISTING_PR="$(gh pr list --head "$BRANCH" --state open --json number --jq '.[0].number // empty' 2>/dev/null || true)"
if [ -n "$EXISTING_PR" ]; then
  gh pr edit "$EXISTING_PR" --body-file "$BODY"
  echo "fuzz-autofix-publish: updated PR #$EXISTING_PR"
else
  gh pr create \
    --draft \
    --base "$BASE_BRANCH" \
    --head "$BRANCH" \
    --title "[fuzz] Fix $TARGET crash" \
    --body-file "$BODY"
fi
