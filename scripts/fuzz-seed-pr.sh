#!/usr/bin/env bash
# Publish seed candidates as one reviewed PR per fuzz target.
#
# Usage: scripts/fuzz-seed-pr.sh <candidates-dir>
#
# <candidates-dir> is the directory produced by `gh run download` for the
# `fuzz-seed-candidate-*` artifacts of one nightly run. Each target gets a
# deterministic branch (`fuzz-seeds/<target>`) based on the default branch, so
# a later run updates the existing PR instead of stacking a new one.
#
# Environment:
#   GH_TOKEN       bot token with contents:write and pull-requests:write
#   BASE_BRANCH    base branch (default: main)
#   RUN_URL        link to the nightly run, for the PR body
#   GITHUB_RUN_ID  run id, for the commit message (optional)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

if [ $# -ne 1 ]; then
  echo "usage: $0 <candidates-dir>" >&2
  exit 2
fi

CANDIDATES_DIR="$1"
BASE_BRANCH="${BASE_BRANCH:-main}"
RUN_ID="${GITHUB_RUN_ID:-manual}"
RUN_URL="${RUN_URL:-}"
FAILED=0

if [ -z "${GH_TOKEN:-}" ]; then
  echo "fuzz-seed-pr: FUZZ_BOT_TOKEN is not configured; skipping corpus PRs"
  exit 0
fi
if [ -z "${GITHUB_REPOSITORY:-}" ]; then
  echo "fuzz-seed-pr: GITHUB_REPOSITORY is not set" >&2
  exit 1
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/selium-seed-pr.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT
ASKPASS="$TMP/git-askpass.sh"
printf '#!/bin/sh\nprintf "%%s\\n" "$GIT_ASKPASS_TOKEN"\n' > "$ASKPASS"
chmod +x "$ASKPASS"
export GIT_ASKPASS="$ASKPASS"
export GIT_ASKPASS_TOKEN="$GH_TOKEN"
export GIT_TERMINAL_PROMPT=0
REMOTE_URL="https://x-access-token@github.com/${GITHUB_REPOSITORY}.git"

if [ ! -d "$CANDIDATES_DIR" ]; then
  echo "fuzz-seed-pr: no candidates directory at $CANDIDATES_DIR"
  exit 0
fi

git config user.name "selium-fuzz-bot"
git config user.email "fuzz-bot@users.noreply.github.com"

found=0
for dir in "$CANDIDATES_DIR"/fuzz-seed-candidate-*; do
  [ -d "$dir" ] || continue
  [ -f "$dir/meta.txt" ] || continue
  found=$((found + 1))

  target="$(sed -n 's/^target=//p' "$dir/meta.txt")"
  delta="$(sed -n 's/^delta=//p' "$dir/meta.txt")"
  files="$(sed -n 's/^files=//p' "$dir/meta.txt")"
  bytes="$(sed -n 's/^bytes=//p' "$dir/meta.txt")"
  if [ -z "$target" ]; then
    echo "fuzz-seed-pr: candidate without a target: $dir" >&2
    FAILED=1
    continue
  fi

  branch="fuzz-seeds/$target"
  echo "fuzz-seed-pr: $target -> $branch (+$delta ft, $files files, $bytes bytes)"

  git fetch origin "$BASE_BRANCH"
  git checkout -B "$branch" "origin/$BASE_BRANCH"

  mkdir -p "fuzz/seeds/$target"
  added=0
  for f in "$dir"/*; do
    [ -f "$f" ] || continue
    name="$(basename "$f")"
    if printf '%s\n' "$name" | grep -Eq '^[0-9a-f]{40}$'; then
      cp "$f" "fuzz/seeds/$target/$name"
      added=$((added + 1))
    fi
  done

  git add "fuzz/seeds/$target"
  if git diff --cached --quiet; then
    echo "fuzz-seed-pr: $target has no seed changes"
    continue
  fi

  # The seed replay is cheap and catches a candidate that crashes on this
  # branch before a PR is opened.
  if ! FUZZ_TARGETS="$target" FUZZ_SECONDS=5 scripts/fuzz-smoke.sh; then
    echo "fuzz-seed-pr: smoke lane failed for $target; not opening a PR" >&2
    git reset --hard "origin/$BASE_BRANCH"
    FAILED=1
    continue
  fi

  git commit -m "Refresh $target fuzz seeds (run $RUN_ID)" --quiet
  git push --force "$REMOTE_URL" "$branch"

  body="$(mktemp "${TMPDIR:-/tmp}/seed-pr-body.XXXXXX")"
  {
    echo "Automated seed refresh for \`$target\` from the nightly fuzz run."
    echo ""
    echo "- coverage features added: $delta"
    echo "- new files: $files"
    echo "- new bytes: $bytes"
    if [ -n "$RUN_URL" ]; then
      echo "- run: $RUN_URL"
    fi
    echo ""
    echo "Only coverage-adding, content-addressed inputs are included; existing"
    echo "named regression seeds are preserved. The branch is managed by the"
    echo "fuzz nightly workflow: a later run replaces this commit rather than"
    echo "opening a second PR."
  } > "$body"

  pr="$(gh pr list --head "$branch" --state open --json number --jq '.[0].number // empty')"
  if [ -n "$pr" ]; then
    gh pr edit "$pr" --body-file "$body"
    echo "fuzz-seed-pr: updated PR #$pr"
  else
    gh pr create \
      --base "$BASE_BRANCH" \
      --head "$branch" \
      --title "Refresh $target fuzz seeds (run $RUN_ID)" \
      --body-file "$body"
  fi
  rm -f "$body"
done

if [ "$found" -eq 0 ]; then
  echo "fuzz-seed-pr: no seed candidates in this run"
fi

exit "$FAILED"
