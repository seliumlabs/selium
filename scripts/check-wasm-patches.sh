#!/usr/bin/env bash
# Guards the wasm TLS guests against silent ring/getrandom/web-time patch
# regressions.
#
# The workspace pins `ring` to a fixed upstream rev via git, patches
# `getrandom` with a local two-line fork, and replaces `web-time` with the
# vendored in-tree crate `vendor/web-time` (see [patch.crates-io] in
# Cargo.toml). If any of those entries stops applying — an entry is removed,
# or a stale Cargo.lock survives a re-added entry (cargo only warns
# "patch ... was not used") — cargo falls back to registry crates that either
# compile JS-backed code into the wasm guests (ring, web-time) or fail the
# wasm build outright (getrandom):
#
# - registry ring 0.17.x pulls getrandom 0.2 with its browser/Node-detecting
#   JS RNG (`__wbindgen_placeholder__` imports),
# - registry web-time compiles its JS `Date`/`Performance` bindings directly,
# - registry getrandom 0.4.3 hits the WEB_CRYPTO compile error under the
#   `custom` backend (rust-random/getrandom#840, closed upstream as
#   "not planned") — the fork moves the const into `error.rs`.
#
# The ring and web-time cases still build but produce wasm that cannot
# instantiate in wasmtiny (the step 4 import scan catches them); the getrandom
# case fails the wasm build this script drives directly.
#
# Run from the workspace root.

set -euo pipefail

fail() {
    echo "check-wasm-patches: FAIL: $1" >&2
    exit 1
}

# 1. ring must resolve to the pinned git rev, not the registry.
if grep -A2 '^name = "ring"$' Cargo.lock | grep -q 'source = "registry'; then
    fail "ring resolved from crates.io (registry ring 0.17.x pulls getrandom 0.2 \
and its JS-backed RNG). The [patch.crates-io] ring entry is not applying. Fix: \
re-add the \`ring\` git pin and run \`cargo update -p ring\`"
fi

# 2. getrandom 0.2 must not appear in the wasm connector graphs.
for guest in selium-connector-http selium-connector-quic; do
    if cargo tree --target wasm32-unknown-unknown -p "$guest" -i getrandom@0.2 >/dev/null 2>&1; then
        fail "getrandom 0.2 (JS-backed RNG) found in the $guest wasm graph. \
Check the ring patch pin in Cargo.toml and Cargo.lock."
    fi
done

# 3. cargo must not report the ring patch as unused (stale lock).
if cargo tree -p selium-connector-http 2>&1 | grep -q 'patch `ring.*was not used'; then
    fail "cargo reports the ring patch as unused. Fix: cargo update -p ring"
fi

# 4. Guest wasm modules must import nothing but the `selium` hostcall module:
#    any JS binding (`__wbindgen_placeholder__`, `__wbg_*`, ...) or other
#    foreign import makes the module unloadable in wasmtiny.
command -v wasm-tools >/dev/null 2>&1 ||
    fail "wasm-tools not found; install with: cargo install wasm-tools"

cargo build --target wasm32-unknown-unknown -p selium-connector-http -p selium-connector-quic >/dev/null

for guest in selium-connector-http selium-connector-quic; do
    artifact="target/wasm32-unknown-unknown/debug/${guest//-/_}.wasm"
    [ -f "$artifact" ] || fail "wasm artifact missing: $artifact"

    foreign=$(wasm-tools print "$artifact" 2>/dev/null |
        grep '(import' |
        grep -vc '"selium"' || true)

    if [ "$foreign" -ne 0 ]; then
        fail "$artifact contains $foreign non-selium import(s) (e.g. JS \
bindings from a regressed web-time/getrandom/ring resolution). Inspect with: \
wasm-tools print $artifact | grep '(import'"
    fi
done

echo "check-wasm-patches: ok"
