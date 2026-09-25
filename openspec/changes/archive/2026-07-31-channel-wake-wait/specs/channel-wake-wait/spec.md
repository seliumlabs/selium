# Spec: Channel Wake/Wait

## ADDED Requirements

### Requirement: Wakeable Channel Reads

Channel readers SHALL register for generation-counter wakes when no data
is available, and SHALL be woken when a writer publishes a frame or the
last writer disconnects. A task awaiting channel data SHALL consume no
reactor CPU while parked.

#### Scenario: Reader parks and wakes on publish

- **WHEN** a task reads a channel with no ready frames and a writer later
  publishes one
- **THEN** the task is woken through the reactor and observes the frame
  without any polling interval elapsing

#### Scenario: Reader observes disconnect without spinning

- **WHEN** a task reads a channel whose last writer disconnects
- **THEN** the task is woken and observes end-of-stream without having
  polled in between

### Requirement: Wakeable Channel Writes (Park Backpressure)

Writers on Park channels SHALL park on a full ring and SHALL be woken when
blocking readers or blocking writers advance the minimum slot position.
Writers on Drop channels SHALL keep drop-on-full semantics without parking.

#### Scenario: Parked writer resumes after reader advance

- **WHEN** a writer hits BufferFull on a Park channel and a blocking reader
  later advances its slot
- **THEN** the writer is woken and its reservation attempt proceeds

### Requirement: Timer Completion

A guest `Timer` SHALL complete at or after its deadline via a host-mediated
wake, without the guest polling the clock.

#### Scenario: Sleep completes

- **WHEN** a guest awaits a `Timer` with a 50 ms deadline
- **THEN** the task is woken no earlier than the deadline and observes
  `Instant::now() >= deadline`

### Requirement: No Spin Stubs

`MappingBackend::atomic_wait32` and `atomic_notify` SHALL perform real
synchronisation on every backend; no method in the wait/notify path MAY
return a success value without effect.

#### Scenario: No silent-success stubs

- **WHEN** code calls `atomic_wait32` on any backend
- **THEN** it either blocks/parks as documented or returns an explicit
  unsupported error — never a fake success
