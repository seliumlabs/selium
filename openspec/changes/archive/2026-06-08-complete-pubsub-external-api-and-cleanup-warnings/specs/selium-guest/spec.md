## MODIFIED Requirements

### Requirement: Pub/Sub Generation-Change Detection
`Subscriber<T>` SHALL detect when the publisher's generation counter has advanced past the subscriber's last-read position by more than the ring buffer capacity, indicating that unread data has been overwritten.

#### Scenario: Publisher overwrites unread data
- **WHEN** a subscriber calls `recv()` and the publisher has advanced the generation counter by more than the ring buffer capacity since the subscriber's last successful read
- **THEN** the subscriber SHALL return `Error::Overwritten` with a message indicating the data was lost

#### Scenario: Normal publishing within capacity
- **WHEN** a subscriber calls `recv()` and the generation counter delta is less than or equal to the ring buffer capacity
- **THEN** the subscriber SHALL read the next available frame normally without returning `Error::Overwritten`

#### Scenario: First read after subscription
- **WHEN** a subscriber calls `recv()` for the first time (no prior `last_generation`)
- **THEN** the subscriber SHALL set `last_generation` to the current generation counter after a successful read

### Requirement: Non-Blocking Reader Poll
`StrongReader` SHALL expose a `poll_ready` method that checks whether a frame is immediately readable without blocking, suitable for async polling integrations such as Quinn.

#### Scenario: Frame is ready
- **WHEN** a caller invokes `poll_ready()` and a frame with the READY flag set is at the current read position
- **THEN** the method SHALL return `Ok(true)`

#### Scenario: No frame ready
- **WHEN** a caller invokes `poll_ready()` and no frame with the READY flag set is at the current read position
- **THEN** the method SHALL return `Ok(false)` without blocking

#### Scenario: Reader is terminated
- **WHEN** a caller invokes `poll_ready()` on a terminated reader
- **THEN** the method SHALL return `Err(Error::Terminated)`

## ADDED Requirements

### Requirement: Error::Overwritten Variant
`selium-guest`'s `Error` enum SHALL include an `Overwritten` variant for the case where a subscriber's unread data has been overwritten by the publisher.

#### Scenario: Matching on Overwritten error
- **WHEN** guest code matches on a pub/sub receive error caused by overwritten data
- **THEN** the `Error::Overwritten` variant SHALL be directly matchable
