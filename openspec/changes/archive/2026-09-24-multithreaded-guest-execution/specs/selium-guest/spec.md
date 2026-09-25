# Spec Delta

## MODIFIED Requirements

### Requirement: Reactor Parking and Wake Sources

The guest executor SHALL run runnable tasks to completion or until they park, and independent runnable tasks SHALL be permitted to advance concurrently across the guest's worker threads. A task that parks on a channel generation counter SHALL consume no worker while parked, and SHALL be woken when its registered generation advances — via the host mailbox or an in-guest futex wait — never via self-scheduled repolling. The executor SHALL NOT spin while only parked tasks remain.

#### Scenario: Reactor stalls on channel waits

- **WHEN** all runnable tasks complete and remaining tasks wait on channel generation counters
- **THEN** `poll_reactor` returns rather than spinning, and the next generation advance re-runs the affected tasks and resumes the waiters

#### Scenario: Channel wait resumes on generation advance

- **WHEN** a parked task's registered generation counter advances
- **THEN** the task SHALL be woken and SHALL observe the new data without a polling interval elapsing

## ADDED Requirements

### Requirement: Concurrent Task Execution

`spawn` SHALL accept futures that are `Send + 'static`, so a spawned task capsule MAY be executed on, and migrated between, any of the guest's worker threads. Independent runnable tasks SHALL be permitted to run concurrently across the guest's workers rather than strictly one after another.

#### Scenario: Independent tasks advance in parallel

- **WHEN** a guest spawns two independent CPU-bound tasks and the guest has more than one worker
- **THEN** both tasks SHALL be able to progress concurrently on distinct workers

#### Scenario: Spawned future must be Send

- **WHEN** guest code calls `spawn` with a future that is not `Send`
- **THEN** the call SHALL fail to compile
