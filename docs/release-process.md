# Release process (lockstep)

Publication is purposefully not enabled yet. The crates.io lane in
`.github/workflows/publish.yml` carries `if: false` until tasks 12.1–12.5 of
`harden-platform-foundations` are satisfied: the workspace still has
repository-only patches (`wasmtiny`, `wasmtiny-aotc`, the `web-time` fork, the
`getrandom`/`ring` wasm patches), and a crate published from packaged sources
would resolve them differently downstream.

## What a release does today

1. A `v<workspace-version>` tag starts `Publish project artifacts`.
2. The `Release gate` job runs `scripts/gate.sh` — the same Linux gate merge
   requires. A tag that fails the gate produces nothing.
3. The artifact matrix builds the runtime and packages the matching golden-path
   WASM fixture for every advertised target. `scripts/smoke-artifact.sh` copies
   only those two packaged files into an isolated directory and runs version
   plus guest-bootstrap checks there, with no Cargo invocation or source-tree
   path. Its `--self-test` proves that isolation and rejects a missing fixture.
4. Each successful matrix row stages its archive as a workflow artifact.
   `Release support matrix` fails unless every target built and smoked, so the
   release's support claim is withheld instead of overstated.
5. Only after the complete matrix passes does `Upload the complete release`
   download the staged archives and upload GitHub release assets. A failed or
   cancelled target therefore publishes no partial platform release. Assets are
   component-only and named accordingly.
   `scripts/check-release-workflow.sh` keeps this staging barrier and packaged
   fixture wired into the gate.

## When the crates.io lane is enabled

`scripts/publish.sh v<version> --gate-passed`:

- refuses a tag that does not equal `v<workspace version>`;
- refuses to run without the gate marker from the release-gate job;
- derives publication order from `cargo metadata` normal/build dependency edges
  (including `selium-guest-macros` before `selium-guest`) and stops at the first
  failure, so a dependant is never published against an unpublished dependency
  and the release is reported incomplete (`scripts/publish.sh --self-test`
  proves both the generated order and stop-on-failure behaviour with a stubbed
  `cargo`).

## Lockstep compatibility

`SELIUM_ABI_VERSION` is the compatibility contract between the host and the
guest SDK. Host and guest packages are released together under the same tag;
guests built against a different ABI are rejected at the hostcall preamble with
an explicit mismatch error. There is no mixed-version compatibility window, so
a downstream consumer must build its guest and its host from the same release.

## Rehearsal

A rehearsal tag exercises exactly the release path above with the crates.io
step still disabled: gate, artifacts, smoke, support matrix, assets. Enabling
crates.io is a separate reviewed change once 12.1–12.5 are checked and the
patched dependencies are published under Selium-owned packages.
