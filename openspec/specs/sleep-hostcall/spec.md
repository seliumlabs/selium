# sleep-hostcall Specification

## Purpose
TBD - created by archiving change add-sleep-hostcall-and-fix-quinn-timer. Update Purpose after archive.
## Requirements
### Requirement: Sleep Hostcall Variant
`selium-abi` SHALL define a `Sleep { millis: u64 }` variant on `HostcallRequest` that requests an asynchronous timed sleep. The host SHALL complete the operation after at least `millis` milliseconds have elapsed, returning `HostcallOutput::Empty`.

#### Scenario: Sleep hostcall round-trip
- **WHEN** a guest encodes `HostcallRequest::Sleep { millis: 500 }` and the runtime processes it
- **THEN** the hostcall SHALL be created with status `PENDING`
- **AND** the operation SHALL complete with `HostcallOutput::Empty` after at least 500ms have elapsed

#### Scenario: Sleep with zero duration
- **WHEN** a guest encodes `HostcallRequest::Sleep { millis: 0 }` and the runtime processes it
- **THEN** the hostcall SHALL be created with status `PENDING`
- **AND** the operation SHALL complete with `HostcallOutput::Empty` on the next poll

#### Scenario: Sleep operation dropped before completion
- **WHEN** a guest creates a `Sleep` operation and drops it before the duration elapses
- **THEN** the host SHALL clean up the operation without error and without waking the guest task

### Requirement: Sleep Hostcall is Async-Only
The `Sleep` hostcall SHALL always return `PENDING` on creation and SHALL NOT be completable via the synchronous `hostcall_ready` path. The guest SHALL use `hostcall_async` to issue sleep operations.

#### Scenario: Synchronous sleep call returns error
- **WHEN** a guest issues `Sleep` via the synchronous `hostcall_ready` path
- **THEN** the call SHALL return a pending error indicating the async API must be used
