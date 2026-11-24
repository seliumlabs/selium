#!/usr/bin/env bash
# Dependency-ordered crates.io publication.
#
# The order is derived from Cargo's own workspace dependency graph (normal and
# build dependencies, excluding dev-only edges). Publication stops at the first
# failure, so a dependant is never published against an unpublished dependency.
# The full Linux gate must have passed for the tag being released: the caller
# passes `--gate-passed`, and the publish workflow only does so from a job that
# ran `scripts/gate.sh` successfully.
#
# Usage:
#   scripts/publish.sh v1.2.3 --gate-passed
#   scripts/publish.sh --self-test
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$ROOT"

metadata() {
  cargo metadata --no-deps --format-version 1
}

workspace_version() {
  metadata | python3 -c 'import json,sys; print(json.load(sys.stdin)["packages"][0]["version"])'
}

publication_order() {
  metadata | python3 -c '
import json
import sys

packages = json.load(sys.stdin)["packages"]
by_name = {package["name"]: package for package in packages}
remaining = {
    name: package
    for name, package in by_name.items()
    if package.get("publish") != []
}
published = set()
order = []
while remaining:
    ready = []
    for name, package in remaining.items():
        dependencies = {
            dependency["name"]
            for dependency in package["dependencies"]
            if dependency.get("path") is not None
            and dependency.get("kind") != "dev"
            and dependency["name"] in by_name
            and by_name[dependency["name"]].get("publish") != []
        }
        if dependencies <= published:
            ready.append(name)
    if not ready:
        cycle = ", ".join(sorted(remaining))
        raise SystemExit(f"publish: workspace dependency cycle among: {cycle}")
    for name in sorted(ready):
        order.append(name)
        published.add(name)
        del remaining[name]
print("\n".join(order))
'
}

if [ "${1:-}" = "--self-test" ]; then
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/selium-publish-selftest-XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  mkdir -p "$tmp/bin"
  cat >"$tmp/metadata.json" <<'JSON'
{"packages":[
  {"name":"selium-guest","version":"9.9.9","publish":null,"dependencies":[
    {"name":"selium-guest-macros","path":"/workspace/macros","kind":null},
    {"name":"selium-abi","path":"/workspace/abi","kind":null}
  ]},
  {"name":"selium-kernel","version":"9.9.9","publish":null,"dependencies":[
    {"name":"selium-service","path":"/workspace/service","kind":null},
    {"name":"selium-abi","path":"/workspace/abi","kind":null}
  ]},
  {"name":"selium-service","version":"9.9.9","publish":null,"dependencies":[
    {"name":"selium-abi","path":"/workspace/abi","kind":null}
  ]},
  {"name":"selium-abi","version":"9.9.9","publish":null,"dependencies":[]},
  {"name":"selium-guest-macros","version":"9.9.9","publish":null,"dependencies":[]}
]}
JSON
  cat >"$tmp/bin/cargo" <<STUB
#!/usr/bin/env bash
printf '%s\\n' "\$*" >>"$tmp/calls"
case "\$*" in
  metadata*) cat "$tmp/metadata.json" ;;
  *"publish -p selium-service"*)
    if [ "\${SELIUM_PUBLISH_SELFTEST_NO_FAILURE:-0}" != "1" ]; then exit 1; fi
    ;;
  *) exit 0 ;;
esac
STUB
  chmod +x "$tmp/bin/cargo"

  if ! SELIUM_PUBLISH_SELFTEST_NO_FAILURE=1 PATH="$tmp/bin:$PATH" \
    "$ROOT/scripts/publish.sh" v9.9.9 --gate-passed >"$tmp/success-out" 2>"$tmp/success-err"; then
    echo "publish: self-test FAIL: the dependency-ordered success plan failed" >&2
    cat "$tmp/success-out" "$tmp/success-err" >&2
    exit 1
  fi
  cp "$tmp/calls" "$tmp/success-calls"
  : >"$tmp/calls"

  set +e
  PATH="$tmp/bin:$PATH" "$ROOT/scripts/publish.sh" v9.9.9 --gate-passed \
    >"$tmp/out" 2>"$tmp/err"
  status=$?
  set -e

  if [ "$status" -eq 0 ]; then
    echo "publish: self-test FAIL: an injected dependency failure did not stop publication" >&2
    exit 1
  fi
  if ! grep -q "FAILED at selium-service" "$tmp/out" "$tmp/err"; then
    echo "publish: self-test FAIL: the failure point was not reported" >&2
    cat "$tmp/out" "$tmp/err" >&2
    exit 1
  fi
  if ! grep -q "release is incomplete" "$tmp/out" "$tmp/err"; then
    echo "publish: self-test FAIL: the release was not marked incomplete" >&2
    exit 1
  fi
  if grep -q "publish -p selium-kernel$" "$tmp/calls"; then
    echo "publish: self-test FAIL: a dependant was published after its dependency failed" >&2
    exit 1
  fi
  macros_line="$(grep -n "publish -p selium-guest-macros$" "$tmp/success-calls" | cut -d: -f1)"
  guest_line="$(grep -n "publish -p selium-guest$" "$tmp/success-calls" | cut -d: -f1)"
  if [ -z "$macros_line" ] || [ -z "$guest_line" ] || [ "$macros_line" -ge "$guest_line" ]; then
    echo "publish: self-test FAIL: Cargo metadata did not place the macro crate before its dependant" >&2
    cat "$tmp/success-calls" >&2
    exit 1
  fi
  if ! grep -q "publish -p selium-abi$" "$tmp/calls"; then
    echo "publish: self-test FAIL: the ordered run did not start from workspace dependencies" >&2
    exit 1
  fi
  echo "publish: self-test PASS (Cargo dependency order, macros before dependants, stop on failure)"
  exit 0
fi

TAG="${1:-}"
GATE="${2:-}"
if [ -z "$TAG" ]; then
  echo "publish: usage: $0 v<version> --gate-passed" >&2
  exit 2
fi
if [ "$GATE" != "--gate-passed" ]; then
  echo "publish: refusing: the full Linux gate has not passed for this tag (pass --gate-passed from a gated job)" >&2
  exit 2
fi

VERSION="$(workspace_version)"
if [ "$TAG" != "v$VERSION" ]; then
  echo "publish: tag $TAG does not match workspace version v$VERSION" >&2
  exit 2
fi

ORDER_OUTPUT="$(publication_order)"
ORDER=()
while IFS= read -r crate; do
  [ -z "$crate" ] || ORDER+=("$crate")
done <<<"$ORDER_OUTPUT"
for crate in "${ORDER[@]}"; do
  echo "publish: $crate $VERSION"
  if ! cargo publish -p "$crate"; then
    echo "publish: FAILED at $crate; release is incomplete. Later crates were not published." >&2
    exit 1
  fi
done
echo "publish: complete for $VERSION"
