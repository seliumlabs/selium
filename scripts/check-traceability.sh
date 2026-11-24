#!/usr/bin/env bash
# Spec-scenario traceability check.
#
# The OpenSpec requirements are the contract; a test that no longer exists (or
# a requirement no test ever exercised) should be visible, not implied. This
# script parses every scenario heading in `openspec/specs/**/spec.md` plus the
# deltas of every *unparked* active change under `openspec/changes/`, collects
# the scenarios that tests claim, collects reviewed waivers, and reports any
# scenario with neither. A change's own delta scenarios are required before it
# archives, so "gate green" cannot depend on them being unsynced.
#
# Annotation format (one scenario per line, directly attached to the test that
# asserts it):
#
#     // scenario: <capability>/<requirement-slug>/<scenario-slug>
#
# An annotation counts only when it sits immediately above a test function:
# blank lines, doc comments, and attribute lines (`#[test]`,
# `#[tokio::test]`, `#[ignore]`, `#[expect(...)]`, ...) may intervene, but any
# other item (a helper, a module, another comment block, end of file) detaches
# the annotation and fails the check. Trailing declaration blocks are not
# evidence: they name scenarios without binding them to an assertion.
#
# The id is the capability directory name, then the requirement and scenario
# headings slugged (lowercase; every run of non-alphanumeric characters becomes
# a single `-`; leading/trailing `-` trimmed). For example:
#
#     ### Requirement: Host-authoritative admission
#     #### Scenario: Authority failure fails closed
#     -> discovery-registration/host-authoritative-admission/authority-failure-fails-closed
#
# A test may carry several annotations; the scenario is covered when any source
# carries it. Prefer annotating the test that *actually asserts* the outcome,
# not the test binary as a whole.
#
# Waiver format (checked-in, reviewed): a Markdown table row in
# `openspec/traceability-waivers.md`
#
#     | <scenario-id> | <reviewer> | <reason the scenario cannot be tested here> |
#
# A waiver with no reason fails the check. An annotation or waiver naming a
# scenario that does not exist fails the check as drift.
#
# Usage:
#   scripts/check-traceability.sh            # strict: fail on any uncovered scenario
#   scripts/check-traceability.sh --report   # report only (exit 0), for adoption
#   scripts/check-traceability.sh --all      # list every uncovered scenario
#   scripts/check-traceability.sh --self-test # exercise the adjacency classifier
#
# Written for bash 3.2 (macOS `/bin/bash`).

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

SPECS_DIR="openspec/specs"
WAIVERS_FILE="openspec/traceability-waivers.md"

REPORT_ONLY=0
SHOW_ALL=0
SELF_TEST=0

while [ $# -gt 0 ]; do
  case "$1" in
    --report) REPORT_ONLY=1 ;;
    --all) SHOW_ALL=1 ;;
    --self-test) SELF_TEST=1 ;;
    -h | --help)
      sed -n '2,42p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "check-traceability: unknown argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

if [ ! -d "$SPECS_DIR" ]; then
  echo "check-traceability: FAIL: $SPECS_DIR not found" >&2
  exit 1
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/selium-trace-XXXXXX")"
trap 'rm -rf "$TMP"' EXIT

KNOWN="$TMP/known"
KNOWN_MAIN="$TMP/known-main"
KNOWN_CHANGE="$TMP/known-change"
ANNOTATED="$TMP/annotated"
WAIVED="$TMP/waived"

: >"$KNOWN"
: >"$KNOWN_MAIN"
: >"$KNOWN_CHANGE"
: >"$ANNOTATED"
: >"$WAIVED"

# ---------------------------------------------------------------------------
# Scenario ids are collected from two places:
#
#   * `openspec/specs/**/spec.md` — the shipped contract, always required.
#   * `openspec/changes/<change>/specs/**/spec.md` for every *unparked* active
#     change — the change's own deltas are required too, so a change cannot
#     claim "gate green" while its new scenarios have no evidence and only
#     fail once they are synced at archive. A change carrying `PARKED.md` is
#     explicitly outside the release gate (see scripts/check-openspec.sh) and
#     is not scanned.
# ---------------------------------------------------------------------------
SPEC_ID_AWK='
    function slug(text,    out, i, c) {
      out = ""
      text = tolower(text)
      for (i = 1; i <= length(text); i++) {
        c = substr(text, i, 1)
        if (c ~ /[a-z0-9]/) {
          out = out c
        } else if (out != "" && substr(out, length(out), 1) != "-") {
          out = out "-"
        }
      }
      sub(/-$/, "", out)
      return out
    }
    /^### Requirement: / {
      requirement = substr($0, length("### Requirement: ") + 1)
      next
    }
    /^#### Scenario: / {
      scenario = substr($0, length("#### Scenario: ") + 1)
      if (requirement == "") next
      print capability "/" slug(requirement) "/" slug(scenario)
    }
'

for spec in "$SPECS_DIR"/*/spec.md; do
  [ -f "$spec" ] || continue
  capability="$(basename "$(dirname "$spec")")"
  awk -v capability="$capability" "$SPEC_ID_AWK" "$spec" >>"$KNOWN_MAIN"
done
sort -u -o "$KNOWN_MAIN" "$KNOWN_MAIN"
cat "$KNOWN_MAIN" >>"$KNOWN"

CHANGES_DIR="openspec/changes"
: >"$KNOWN_CHANGE"
if [ -d "$CHANGES_DIR" ]; then
  for change_dir in "$CHANGES_DIR"/*/; do
    [ -d "$change_dir" ] || continue
    change="$(basename "$change_dir")"
    [ "$change" = "archive" ] && continue
    [ -f "$change_dir/PARKED.md" ] && continue
    for spec in "$change_dir"specs/*/spec.md; do
      [ -f "$spec" ] || continue
      capability="$(basename "$(dirname "$spec")")"
      awk -v capability="$capability" "$SPEC_ID_AWK" "$spec" >>"$KNOWN_CHANGE"
    done
  done
fi
sort -u -o "$KNOWN_CHANGE" "$KNOWN_CHANGE"
cat "$KNOWN_CHANGE" >>"$KNOWN"
sort -u -o "$KNOWN" "$KNOWN"


# ---------------------------------------------------------------------------
# Test annotations: `// scenario: <id>` lines attached to the immediately
# following test function. Detached annotations (trailing blocks, annotations
# above helpers or modules) are reported and fail the run.
# ---------------------------------------------------------------------------
DETACHED_LIST="$TMP/detached"
: >"$DETACHED_LIST"

# Classifies every `// scenario:` line in one Rust source. Prints adjacent ids
# on stdout and `DETACHED <file>:<line> <id>` on fd 3 for detached ones.
#
# Adjacency: annotation lines, then any run of blank lines, doc comments, and
# attributes (including multi-line attribute bodies), then a test function
# carrying `#[test]`/`#[tokio::test]`/`#[rstest]`. Anything else detaches.
read -r -d '' ADJACENCY_AWK <<'AWK'
function flush(   k) {
  if (n == 0) return
  if (adjacent) {
    for (k = 1; k <= n; k++) print scenario[k]
  } else {
    for (k = 1; k <= n; k++) {
      printf "DETACHED %s:%d %s\n", FILENAME, scenario_line[k], scenario[k] > "/dev/stderr"
    }
  }
  n = 0
  adjacent = 0
  saw_test = 0
}
function is_test_attr(text) {
  return text ~ /^[[:space:]]*#\[(tokio::)?test([ (]|\])/ ||
         text ~ /^[[:space:]]*#\[rstest([ (]|\])/
}
{
  line = $0
  if (in_attribute) {
    # Multi-line attributes close on a line ending in `]` (`)]` for
    # `#[expect(...)]`/`#[command(...)]`, `]` for `#[cfg(any(...))]`).
    if (line ~ /\][[:space:]]*$/) in_attribute = 0
    next
  }
  if (line ~ /^[[:space:]]*\/\/ scenario: /) {
    n++
    scenario[n] = line
    sub(/^[[:space:]]*\/\/ scenario: */, "", scenario[n])
    sub(/[[:space:]]+$/, "", scenario[n])
    scenario_line[n] = FNR
    next
  }
  if (line ~ /^[[:space:]]*$/ || line ~ /^[[:space:]]*\/\/\//) next
  if (line ~ /^[[:space:]]*#\[/) {
    if (is_test_attr(line)) saw_test = 1
    if (line !~ /\]/) in_attribute = 1
    next
  }
  if (n > 0) {
    adjacent = (saw_test && line ~ /^[[:space:]]*(pub([^)]*)[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+[A-Za-z_]/)
    flush()
  }
}
END { flush() }
AWK

while IFS= read -r source; do
  [ -n "$source" ] || continue
  awk "$ADJACENCY_AWK" "$source" 2>>"$DETACHED_LIST" >>"$ANNOTATED"
done < <(find crates guests integration -name '*.rs' -not -path '*/target/*' -not -path '*/vendor/*' 2>/dev/null)

sort -u -o "$ANNOTATED" "$ANNOTATED"
sort -u -o "$DETACHED_LIST" "$DETACHED_LIST"
detached_count="$(wc -l <"$DETACHED_LIST" | tr -d ' ')"

# ---------------------------------------------------------------------------
# Self-test: the classifier must accept an attached annotation and reject a
# detached one. Runs the same awk program over checked-in fixtures.
# ---------------------------------------------------------------------------
if [ "$SELF_TEST" = "1" ]; then
  fixtures="scripts/traceability-fixtures"
  attached_out="$(awk "$ADJACENCY_AWK" "$fixtures/attached.rs" 2>/dev/null)"
  detached_out="$(awk "$ADJACENCY_AWK" "$fixtures/detached.rs" 2>&1 >/dev/null)"
  status=0
  if [ "$attached_out" != "example-capability/example-requirement/attached-scenario" ]; then
    echo "check-traceability: self-test FAIL: attached fixture was not accepted" >&2
    status=1
  fi
  if ! printf '%s' "$detached_out" | grep -q "DETACHED .*detached-scenario"; then
    echo "check-traceability: self-test FAIL: detached fixture was not rejected" >&2
    status=1
  fi
  if [ "$status" -eq 0 ]; then
    echo "check-traceability: self-test PASS"
  fi
  exit "$status"
fi

# ---------------------------------------------------------------------------
# Reviewed waivers: a Markdown table row `| id | reviewer | reason |`.
# ---------------------------------------------------------------------------
if [ -f "$WAIVERS_FILE" ]; then
  awk -F'|' '
    /^[[:space:]]*\|/ {
      id = $2; reviewer = $3; reason = $4
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", id)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", reviewer)
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", reason)
      if (id == "Scenario" || id ~ /^-+$/) next
      if (id == "") next
      if (reason == "") {
        printf "check-traceability: FAIL: waiver for %s has no reason\n", id > "/dev/stderr"
        exit 3
      }
      print id
    }
  ' "$WAIVERS_FILE" | sort -u >"$WAIVED"
  waiver_status="${PIPESTATUS[0]}"
  if [ "$waiver_status" -ne 0 ]; then
    exit 1
  fi
fi

# ---------------------------------------------------------------------------
# Drift: names that do not exist in the specs.
# ---------------------------------------------------------------------------
DRIFT=0
while IFS= read -r id; do
  [ -n "$id" ] || continue
  if ! grep -qxF "$id" "$KNOWN"; then
    echo "check-traceability: DRIFT: annotation names unknown scenario: $id" >&2
    DRIFT=1
  fi
done <"$ANNOTATED"

while IFS= read -r id; do
  [ -n "$id" ] || continue
  if ! grep -qxF "$id" "$KNOWN"; then
    echo "check-traceability: DRIFT: waiver names unknown scenario: $id" >&2
    DRIFT=1
  fi
done <"$WAIVED"

# ---------------------------------------------------------------------------
# Detached annotations: an annotation that is not attached to a test is not
# evidence, so it fails the check rather than silently counting.
# ---------------------------------------------------------------------------
if [ "$detached_count" -gt 0 ]; then
  echo "check-traceability: detached annotations (attach each to its asserting test):"
  sed 's/^/  /' "$DETACHED_LIST" | head -40
  if [ "$detached_count" -gt 40 ]; then
    echo "  ... and $((detached_count - 40)) more"
  fi
fi

# ---------------------------------------------------------------------------
# Coverage.
# ---------------------------------------------------------------------------
COVERED="$TMP/covered"
cat "$ANNOTATED" "$WAIVED" | sort -u >"$COVERED"
comm -23 "$KNOWN" "$COVERED" >"$TMP/missing"

known_count="$(wc -l <"$KNOWN" | tr -d ' ')"
main_count="$(wc -l <"$KNOWN_MAIN" | tr -d ' ')"
change_count="$(comm -13 "$KNOWN_MAIN" "$KNOWN_CHANGE" | wc -l | tr -d ' ')"
covered_count="$(comm -12 "$KNOWN" "$COVERED" | wc -l | tr -d ' ')"
annotated_count="$(comm -12 "$KNOWN" "$ANNOTATED" | wc -l | tr -d ' ')"
waived_count="$(comm -12 "$KNOWN" "$WAIVED" | wc -l | tr -d ' ')"
missing_count="$(wc -l <"$TMP/missing" | tr -d ' ')"

if [ "$missing_count" -gt 0 ]; then
  # Per-capability summary, so the report points at the next unit of work.
  echo "check-traceability: uncovered scenarios by capability:"
  cut -d/ -f1 "$TMP/missing" | sort | uniq -c | sort -rn |
    while read -r count capability; do
      printf '  %4d  %s\n' "$count" "$capability"
    done

  if [ "$SHOW_ALL" = "1" ] || [ "$REPORT_ONLY" = "1" ]; then
    echo "check-traceability: uncovered scenarios:"
    sed 's/^/  /' "$TMP/missing"
  else
    echo "check-traceability: (pass --all to list every uncovered scenario)"
  fi
fi

echo "check-traceability: $known_count scenarios ($main_count shipped + $change_count change delta), $annotated_count annotated, $waived_count waived, $missing_count uncovered, $detached_count detached"

if [ "$DRIFT" -ne 0 ]; then
  echo "check-traceability: FAIL (drift)"
  exit 1
fi

if [ "$detached_count" -gt 0 ]; then
  echo "check-traceability: FAIL ($detached_count detached annotations)"
  exit 1
fi

if [ "$missing_count" -gt 0 ]; then
  if [ "$REPORT_ONLY" = "1" ]; then
    echo "check-traceability: REPORT ($missing_count uncovered)"
    exit 0
  fi
  echo "check-traceability: FAIL ($missing_count uncovered)"
  exit 1
fi

echo "check-traceability: PASS"
