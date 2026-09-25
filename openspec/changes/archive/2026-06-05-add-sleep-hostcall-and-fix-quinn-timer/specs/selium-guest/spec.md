## MODIFIED Requirements

### Requirement: AsyncTimer Implementation
`selium-guest` SHALL implement `quinn::AsyncTimer` via the `Timer` type, which uses `HostcallRequest::Sleep` to provide deadline-based wakeups for Quinn's timeout management. The `Timer` type SHALL be publicly exported from `selium_guest::time`.

#### Scenario: Quinn timer reaches deadline
- **WHEN** Quinn polls a timer whose deadline has passed (i.e., `Instant::now() >= deadline`)
- **THEN** `poll` SHALL return `Poll::Ready(())`

#### Scenario: Quinn timer not yet expired
- **WHEN** Quinn polls a timer whose deadline has not yet passed
- **THEN** the timer SHALL issue a `HostcallRequest::Sleep` for the remaining duration and return `Poll::Pending`

#### Scenario: Quinn timer reset
- **WHEN** Quinn calls `reset(deadline)` on an active timer
- **THEN** any in-flight sleep operation SHALL be cancelled and the new deadline SHALL take effect

## ADDED Requirements

### Requirement: Timer Public Export
The `Timer` type SHALL be publicly exported from `selium_guest::time` so that Quinn integration code in `net/quinn.rs` can reference it.

#### Scenario: Timer accessible from crate root
- **WHEN** code imports `selium_guest::time::Timer`
- **THEN** the import SHALL resolve to the `Timer` type defined in `time.rs`

### Requirement: RuntimeInstant Implementation Location
The `impl quinn::RuntimeInstant for Instant` block SHALL reside exclusively in `time.rs` and SHALL NOT be duplicated in `net/quinn.rs`.

#### Scenario: No duplicate impl
- **WHEN** the `quinn` feature is enabled and both `time.rs` and `net/quinn.rs` are compiled
- **THEN** there SHALL be exactly one `impl quinn::RuntimeInstant for Instant` in the crate, located in `time.rs`
