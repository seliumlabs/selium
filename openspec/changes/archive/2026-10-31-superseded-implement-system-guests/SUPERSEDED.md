# Superseded

This proposal was written against a workspace that no longer exists:

- core crates lived under `crates/core/...` (now `crates/...`),
- system guests lived under `crates/guests/...` (now `guests/...`),
- guest I/O patterns lived in a separate `selium-io` crate (now consolidated
  into `selium-guest`, with the ring/transport primitives in `selium-shm` and
  `selium-wire`),
- the external API was a standalone guest (now delivered by
  `guests/connector-quic`, `guests/connector-http`, and `guests/bridge`).

The parts that landed are the running `guests/discovery`, `guests/cluster`,
`guests/scheduler`, and `guests/supervisor` entrypoints. The remaining
correctness and verification work is tracked by
`openspec/changes/harden-platform-foundations/`; the scheduling delegation and
external surface are described by the current main specs.

The change is archived as **superseded**, not completed: its task list is kept
for history and was deliberately not synced into the main specs.
