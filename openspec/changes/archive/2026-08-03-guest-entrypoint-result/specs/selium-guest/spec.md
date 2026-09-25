## ADDED Requirements

### Requirement: Entrypoint Result Extraction
`selium-guest` SHALL provide a `run_entrypoint_with_result` function that spawns a future onto the cooperative reactor, polls until completion, and returns the future's output.

#### Scenario: Successful future returns its output
- **WHEN** `run_entrypoint_with_result` is called with a future that resolves to `Ok(())`
- **THEN** it SHALL return `Ok(())` after the reactor parks

#### Scenario: Failing future returns its error
- **WHEN** `run_entrypoint_with_result` is called with a future that resolves to `Err(e)`
- **THEN** it SHALL return `Err(e)` after the reactor parks

#### Scenario: Panic in future aborts the guest
- **WHEN** `run_entrypoint_with_result` is called and a spawned task panics during reactor polling
- **THEN** the guest process SHALL abort (same behavior as `run_entrypoint_safely`)

### Requirement: JoinHandle Result Access
`selium-guest` SHALL expose a `pub(crate)` accessor on `JoinHandle<T>` that drains and returns the completed task's output.

#### Scenario: Result extracted after reactor parks
- **WHEN** `take_result()` is called on a `JoinHandle` whose task has completed
- **THEN** it SHALL return `Some(output)` with the task's final value

#### Scenario: Result extraction before completion
- **WHEN** `take_result()` is called on a `JoinHandle` whose task has not yet completed
- **THEN** it SHALL return `None`
