## Purpose

TBD — `selium-shm` provides the shared-memory channel substrate (ring buffers, readers, writers) used by guests and the runtime. Defines the wait semantics contract that readers and writers must register for generation-counter wakes rather than busy-spin or return unwakeable pending states.

## Requirements

### Requirement: Reader/Writer Wait Semantics

`Reader`, `BlockingReader`, `Writer`, and `BlockingWriter` SHALL NOT
return an unwakeable `Poll::Pending` and SHALL NOT busy-spin. Unmet
read/write conditions SHALL register the task for a generation-counter
wake before returning `Poll::Pending`.

#### Scenario: Pending implies registered

- **WHEN** any channel `poll_read` or `poll_write` returns `Poll::Pending`
- **THEN** the calling task is registered to be woken by a later
  generation bump or writer-count change

#### Scenario: Disconnect is observable

- **WHEN** the last writer on a channel disconnects
- **THEN** the generation counter is bumped so parked readers observe
  end-of-stream

### Requirement: Layout/Plumbing Separation

`selium-shm` SHALL expose the ring protocol layout (offsets, codec,
reservation, slots) independently of the global region provider, so host
environments (kernel, runtime, tests) can drive the same protocol over
alternative `MappingBackend`s.

#### Scenario: Layout usable without a provider

- **WHEN** host code with its own `MappingBackend` drives the ring
  primitives
- **THEN** it can do so without installing or touching the global
  `RegionProvider`

#### Scenario: Public API stability

- **WHEN** downstream code imports `FrameHeader`, `RingBuf`, `Channel`,
  or `ChannelRegion`
- **THEN** existing import paths keep working (via re-exports) after the
  layout module lands
