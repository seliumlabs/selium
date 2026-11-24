# Spec Delta

## ADDED Requirements

### Requirement: Guest Errors Preserve Stable ABI Causes
Guest-facing hostcall failures SHALL retain the ABI error code and message so callers can programmatically distinguish permission, quota, timeout, absence, malformed input, and unavailable resources.

#### Scenario: Immediate hostcall rejection
- **WHEN** hostcall creation returns a valid failed operation
- **THEN** the guest SHALL retrieve and drop that operation and return its structured error without leaking host operation state

#### Scenario: Quota failure is matchable
- **WHEN** the host rejects an operation with `QuotaExceeded`
- **THEN** guest code SHALL be able to match that code without parsing a display string

### Requirement: Shared Clients Are Cancellation-Safe
Dropping a request future at any await point SHALL release gates and restore or reclaim session ownership so subsequent requests can proceed and no session region leaks.

#### Scenario: Store request is cancelled
- **WHEN** a caller drops a store or control request while it awaits a reply
- **THEN** the client gate SHALL reopen and its session SHALL be safely restored or closed

### Requirement: Join Completion Cannot Be Lost
Join-handle polling SHALL register its waker and re-check completion with sufficient ordering to close the completion-registration race.

#### Scenario: Completion races poll
- **WHEN** a task completes between the join handle's first completion check and waker registration
- **THEN** the handle SHALL either return the result immediately or be woken for a subsequent poll

### Requirement: Live Table Mutation Has One Authority
Compare-and-set and version assignment SHALL be admitted by one authoritative writer or arbiter per table, never solely against independent consumer projections.

#### Scenario: Competing writers attempt CAS
- **WHEN** two writers attempt mutations from the same expected version
- **THEN** the authority SHALL admit at most one at that version and SHALL return a conflict to the other

#### Scenario: API lacks an arbiter
- **WHEN** a table is constructed without an enforceable single-writer or arbiter contract
- **THEN** compare-and-set SHALL be unavailable or rejected loudly rather than claim distributed atomicity

### Requirement: Primary Channel Patterns Use The Unified SDK Surface
The guest SDK SHALL re-export the principal channel and messaging-pattern handles needed for a first guest without requiring direct lower-crate imports.

#### Scenario: First guest uses only the SDK
- **WHEN** an application creates a typed shared-memory pub/sub channel
- **THEN** its channel, transport, and pattern types SHALL be importable from `selium-guest`
