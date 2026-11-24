# Parked

Future work. No task in this change has been started (35 unchecked), and its
deltas are not part of the current release gate: the platform ships single-host
channels, and `DESIGN-INTENT.md` defers cross-host durability with the frozen
`guests/cluster` sources.

`scripts/check-openspec.sh` and `scripts/check-traceability.sh` both skip a
change carrying this file: strict delta validation and delta-scenario coverage
apply only to changes being implemented. Remove this file when the work is
scheduled, rebase the deltas onto the then-current main specs, and let both
checks apply again.
