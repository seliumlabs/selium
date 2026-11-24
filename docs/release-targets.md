# Release targets and the support claim

A target is **supported** only when its packaged artifact, built for that
target, starts and bootstraps a real WebAssembly guest on a runner of that
target. A green compile is not enough: the artifact must run.

`.github/workflows/publish.yml` builds one artifact per row below, then runs
`scripts/smoke-artifact.sh` against it. Each archive carries the matching
`selium_spine_demo.wasm` fixture. The smoke copies only the packaged runtime and
fixture into an isolated directory, checks `--version` and `--smoke-bootstrap`,
and never builds or reads from the source checkout. This proves the host/guest
ABI preamble and guest instantiation on the actual target.

| Target | Runner | Artifact |
| --- | --- | --- |
| `aarch64-unknown-linux-gnu` | `ubuntu-latest-arm` | `selium-runtime-component-aarch64-unknown-linux-gnu.tar.gz` |
| `x86_64-unknown-linux-gnu` | `ubuntu-latest` | `selium-runtime-component-x86_64-unknown-linux-gnu.tar.gz` |
| `aarch64-apple-darwin` | `macos-latest` | `selium-runtime-component-aarch64-apple-darwin.tar.gz` |
| `aarch64-pc-windows-msvc` | `windows-11-arm` | `selium-runtime-component-aarch64-pc-windows-msvc.zip` |
| `x86_64-pc-windows-msvc` | `windows-latest` | `selium-runtime-component-x86_64-pc-windows-msvc.zip` |

## A failed target withholds the claim

The matrix uses `fail-fast: false`, so every target reports its own result. The
`Release support matrix` job then fails unless **all** targets succeeded. A
release whose matrix is red is therefore incomplete: the failed target is not
supported, and the other targets remain accurately reported as building and
smoking. No target is silently dropped from the advertised set to make a
release look green.

## Component-only artifacts

These artifacts are the embedder **runtime component**: the host binary plus the
small golden-path guest used solely by the isolated smoke test. `sel`, the
system-guest WASM modules, and the bootstrap configuration are code-defined
assets and are not bundled here. The artifact name says `component` so it cannot
be mistaken for a turnkey platform release. When the
crates.io publication lane is enabled (tasks 12.1–12.5), the platform artifact
set will add `sel` and the matching guest WASM built from the same tag.

## State and keys

The runtime starts without a state directory (throwaway volumes, no persisted
keys). A deployment supplies `--state-dir` or `SELIUM_STATE_DIR`; on first boot
the host generates the PKI hierarchy and persists the online intermediate and
per-tenant CAs, while the root private key stays offline. The smoke runs against
a temporary state directory and never touches an operator's keys.

`sel` and the system guests are **not** mixed-version safe: the rkyv enum
indices must match. `SELIUM_ABI_VERSION` is compared in the hostcall preamble
before any request is decoded, so a mismatched guest fails loudly rather than
being mis-decoded.
