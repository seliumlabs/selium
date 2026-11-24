#!/usr/bin/env bash
# Structural self-test for the release workflow's all-targets-before-upload
# barrier and its packaged smoke fixture. The executable behaviour is covered
# by publish.sh --self-test and smoke-artifact.sh --self-test.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
python3 - "$ROOT/.github/workflows/publish.yml" <<'PY'
from pathlib import Path
import sys

workflow = Path(sys.argv[1]).read_text()
checks = {
    "matrix archives are staged, not released per target": "name: stage verified artifact",
    "an aggregate support barrier exists": "support-matrix:",
    "asset upload depends on the complete matrix": "needs: support-matrix",
    "the smoke guest is packaged": "selium_spine_demo.wasm",
    "assets download only from staged target artifacts": "pattern: release-*",
}
for description, needle in checks.items():
    if needle not in workflow:
        raise SystemExit(f"release workflow self-test FAIL: {description}")
if workflow.count("softprops/action-gh-release") != 1:
    raise SystemExit("release workflow self-test FAIL: release upload must have one aggregate call site")
if workflow.index("needs: support-matrix") > workflow.index("softprops/action-gh-release"):
    raise SystemExit("release workflow self-test FAIL: the support barrier does not precede upload")
print("release workflow self-test PASS (all targets stage before one aggregate upload)")
PY
