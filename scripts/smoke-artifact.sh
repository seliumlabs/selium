#!/usr/bin/env bash
# Smoke a packaged runtime artifact without a source checkout.
#
# The package must contain both the runtime and its target-independent golden
# path guest. The script copies just those files into an isolated directory and
# runs there; it never invokes Cargo or reads a workspace target directory.
#
# Usage:
#   scripts/smoke-artifact.sh <path-to-selium-runtime> [path-to-smoke-wasm]
#   scripts/smoke-artifact.sh --self-test
set -euo pipefail

absolute_path() {
  local path="$1"
  local directory
  directory="$(cd "$(dirname "$path")" && pwd -P)"
  printf '%s/%s\n' "$directory" "$(basename "$path")"
}

if [ "${1:-}" = "--self-test" ]; then
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/selium-smoke-selftest-XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  mkdir -p "$tmp/package"
  cat >"$tmp/package/selium-runtime" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
printf 'pwd=%s args=%s\n' "$PWD" "$*" >>"$SMOKE_TEST_LOG"
if [ "${1:-}" = "--version" ]; then
  echo "selium-runtime self-test"
  exit 0
fi
for argument in "$@"; do
  case "$argument" in
    --app)
      app="next"
      ;;
    *)
      if [ "${app:-}" = "next" ]; then
        wasm="${argument#*path=}"
        wasm="${wasm%%,entrypoint=*}"
        [ -f "$wasm" ] || {
          echo "stub: guest is not packaged: $wasm" >&2
          exit 1
        }
        app="seen"
      fi
      ;;
  esac
done
[ "${app:-}" = "seen" ]
STUB
  chmod +x "$tmp/package/selium-runtime"
  printf '\0asm\1\0\0\0' >"$tmp/package/selium_spine_demo.wasm"
  export SMOKE_TEST_LOG="$tmp/calls"

  script="$(absolute_path "${BASH_SOURCE[0]}")"
  (
    cd /
    "$script" "$tmp/package/selium-runtime" >"$tmp/out"
  )
  if grep -q "$tmp/package/selium_spine_demo.wasm" "$tmp/calls"; then
    echo "smoke-artifact: self-test FAIL: the runtime read the original fixture path" >&2
    cat "$tmp/calls" >&2
    exit 1
  fi
  if ! grep -q "guest is not packaged\|path=" "$tmp/calls"; then
    echo "smoke-artifact: self-test FAIL: no guest bootstrap was attempted" >&2
    exit 1
  fi

  set +e
  "$script" "$tmp/package/selium-runtime" "$tmp/package/missing.wasm" \
    >"$tmp/missing-out" 2>"$tmp/missing-err"
  status=$?
  set -e
  if [ "$status" -eq 0 ]; then
    echo "smoke-artifact: self-test FAIL: an unpackaged guest was accepted" >&2
    exit 1
  fi
  echo "smoke-artifact: self-test PASS (isolated runtime and packaged guest, no Cargo/source tree)"
  exit 0
fi

BIN="${1:-}"
WASM="${2:-}"
if [ -z "$BIN" ]; then
  echo "smoke-artifact: usage: $0 <path-to-selium-runtime> [path-to-smoke-wasm]" >&2
  exit 2
fi
if [ ! -f "$BIN" ]; then
  echo "smoke-artifact: FAIL: artifact is missing: $BIN" >&2
  exit 1
fi
BIN="$(absolute_path "$BIN")"
if [ -z "$WASM" ]; then
  WASM="$(dirname "$BIN")/selium_spine_demo.wasm"
fi
if [ ! -f "$WASM" ]; then
  echo "smoke-artifact: FAIL: package the smoke guest beside the runtime (or pass its path): $WASM" >&2
  exit 1
fi
WASM="$(absolute_path "$WASM")"

STATE="$(mktemp -d "${TMPDIR:-/tmp}/selium-smoke-XXXXXX")"
trap 'rm -rf "$STATE"' EXIT
PACKAGE="$STATE/package"
mkdir -p "$PACKAGE"
cp "$BIN" "$PACKAGE/$(basename "$BIN")"
cp "$WASM" "$PACKAGE/selium_spine_demo.wasm"
BIN="$PACKAGE/$(basename "$BIN")"
WASM="$PACKAGE/selium_spine_demo.wasm"
chmod +x "$BIN"

cd "$PACKAGE"
echo "smoke-artifact: isolated version check"
"$BIN" --version

echo "smoke-artifact: isolated bootstrap check (no source checkout)"
"$BIN" \
  --state-dir "$STATE/state" \
  --smoke-bootstrap \
  --app "name=smoke,path=${WASM},entrypoint=spine_demo"

echo "smoke-artifact: PASS ($BIN)"
