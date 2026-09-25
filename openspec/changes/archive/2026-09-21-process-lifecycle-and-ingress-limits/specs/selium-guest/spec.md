## MODIFIED Requirements

### Requirement: Entrypoint Result Extraction

`selium-guest` SHALL provide a `run_entrypoint_with_result` function that
spawns a `Result`-producing future onto the cooperative reactor, polls
until the reactor stalls, and returns the task's output if it completed or
`Ok(())` when it parked without completing. The entrypoint export must
return an exit code immediately, so a parked entrypoint (a long-running
service, or a guest waiting on host-delivered events) reports "no error
observed" and stays on the reactor, driven by later guest polls; the
function SHALL NOT panic when the entrypoint parks. When the poll-owner
entrypoint task later completes, the reactor SHALL report completion
through the poll export so the host terminates the process as a normal
exit — the completed task SHALL NOT remain on the reactor as an idle no-op.

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

#### Scenario: Late completion ends the process
- **WHEN** a previously parked entrypoint task completes during a later reactor poll
- **THEN** the reactor SHALL report completion through the poll export and the host SHALL terminate the process
- **AND** a later `Err` completion SHALL be logged through the guest log transport before the process terminates

#### Scenario: Panic in future aborts the guest
- **WHEN** `run_entrypoint_with_result` is called and a spawned task panics during reactor polling
- **THEN** the guest process SHALL abort (same behavior as `run_entrypoint_safely`)
