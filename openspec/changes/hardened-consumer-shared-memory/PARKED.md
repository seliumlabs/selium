# Parked

This proposal is future work and is not part of the current release gate. Its
paths are rebased onto the current workspace (`crates/memory`, `crates/shm`,
`crates/runtime`), but its spec deltas still need a semantic rebase onto the
current main specs before implementation starts: several `MODIFIED`
requirements predate the consolidated ring layout, and the `framed-io` delta
adds requirement names that no longer exist.

`scripts/check-openspec.sh` skips strict delta validation for a change carrying
this file, while still rejecting removed-path references. Remove this file when
the change is picked up, and let the gate validate it strictly from then on.

Tracked as the dedicated consumer-write-trust change in
`docs/known-unknowns.md`.
