## ADDED Requirements

### Requirement: Raw discovery handle accessor

`selium-guest` SHALL expose `Context::raw_handle()`, returning the raw
`u64` discovery handle the context was constructed from. System guests
that must forward the handle to child guests they spawn can therefore pass
it along without rebuilding a context or reconstructing the raw value from
other state.

#### Scenario: Guest forwards discovery handle to a spawned child

- **WHEN** a system guest (for example bridge-server) spawns a child guest whose own entrypoint requires the discovery handle
- **THEN** the parent SHALL read the handle via `Context::raw_handle()` and pass it as the child's first entrypoint argument
- **AND** the parent SHALL NOT need to retain a separate copy of the handle

## MODIFIED Requirements

### Requirement: Entrypoint Result Extraction

`selium-guest` SHALL provide a `run_entrypoint_with_result` function that
spawns a `Result`-producing future onto the cooperative reactor, polls
until the reactor stalls, and returns the task's output if it completed or
`Ok(())` when it parked without completing. The entrypoint export must
return an exit code immediately, so a parked entrypoint (a long-running
service, or a guest waiting on host-delivered events) reports "no error
observed" and stays on the reactor, driven by later guest polls; the
function SHALL NOT panic when the entrypoint parks.

#### Scenario: Successful future returns its output

- **WHEN** `run_entrypoint_with_result` is called with a future that resolves to `Ok(())`
- **THEN** it SHALL return `Ok(())` after the reactor parks

#### Scenario: Failing future returns its error

- **WHEN** `run_entrypoint_with_result` is called with a future that resolves to `Err(e)`
- **THEN** it SHALL return `Err(e)` after the reactor parks
- **AND** the error SHALL be logged through the guest log transport when the task completes

#### Scenario: Entrypoint parks without completing

- **WHEN** the entrypoint future parks (a long-running service loop or a wait on host-delivered events) and the reactor stalls before the task completes
- **THEN** `run_entrypoint_with_result` SHALL return `Ok(())` (exit code 0)
- **AND** the task SHALL remain on the reactor, driven by later polls
- **AND** a later `Err` completion SHALL be logged through the guest log transport, since the already-returned exit code can no longer carry it

#### Scenario: Panic in future aborts the guest

- **WHEN** `run_entrypoint_with_result` is called and a spawned task panics during reactor polling
- **THEN** the guest process SHALL abort (same behavior as `run_entrypoint_safely`)