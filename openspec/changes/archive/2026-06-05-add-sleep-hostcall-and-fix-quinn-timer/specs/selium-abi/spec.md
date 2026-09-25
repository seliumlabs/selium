## ADDED Requirements

### Requirement: Sleep Hostcall Variant
`HostcallRequest` SHALL include a `Sleep { millis: u64 }` variant. The runtime SHALL compute `deadline = Instant::now() + Duration::from_millis(millis)` and store the operation in `HostOperationState::SleepWait { deadline }`. When polled, the runtime SHALL return `CompletionState::Ready(HostcallOutput::Empty)` if `Instant::now() >= deadline`, or `CompletionState::Pending` otherwise.

#### Scenario: Sleep operation created
- **WHEN** the runtime dispatches `HostcallRequest::Sleep { millis: 500 }`
- **THEN** the operation SHALL be stored with a `SleepWait` state whose `deadline` is at least 500ms after the current `Instant::now()`

#### Scenario: Sleep operation polled before deadline
- **WHEN** a `SleepWait` operation is polled and `Instant::now() < deadline`
- **THEN** the poll SHALL return `CompletionState::Pending { operation_id }`

#### Scenario: Sleep operation polled after deadline
- **WHEN** a `SleepWait` operation is polled and `Instant::now() >= deadline`
- **THEN** the poll SHALL return `CompletionState::Ready(HostcallOutput::Empty)`
