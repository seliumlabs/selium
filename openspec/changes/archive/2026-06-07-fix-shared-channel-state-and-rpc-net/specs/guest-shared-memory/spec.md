## MODIFIED Requirements

### Requirement: Native Atomic Access to Shared Regions
Guests SHALL access shared region data and coordination fields using native WASM load, store, and atomic instructions at the page offset returned by `alloc_region` or `attach_region`.

#### Scenario: Guest reads shared data via load
- **WHEN** a guest executes `i32.load` at an address within a mapped shared region
- **THEN** the value SHALL be read directly from the shared memory without host intervention

#### Scenario: Guest writes shared data via store
- **WHEN** a guest executes `i32.store` at an address within a writable shared region
- **THEN** the value SHALL be written directly to the shared memory without host intervention

#### Scenario: Guest uses atomic wait on shared region
- **WHEN** a guest executes `memory.atomic.wait32` on an address within a mapped shared region
- **THEN** the instruction SHALL block until `memory.atomic.notify` is called on that address by another instance

#### Scenario: Guest uses atomic notify on shared region
- **WHEN** a guest executes `memory.atomic.notify` on an address within a mapped shared region
- **THEN** the instruction SHALL wake waiters blocked on `memory.atomic.wait32` at that address

#### Scenario: Guest CAS on shared coordination field
- **WHEN** a guest executes `memory.atomic.rmw.cmpxchg` on the shared `next_tail` field
- **THEN** the CAS SHALL be visible to all other processes attached to the same region, enabling cross-process writer coordination

## ADDED Requirements

### Requirement: Shared Region Coordination Layout
Every shared region used for messaging SHALL include cross-process coordination fields in page 0 at fixed offsets, enabling many-to-many channel semantics without host mediation.

#### Scenario: Standard coordination layout
- **WHEN** a shared region is allocated for messaging
- **THEN** page 0 SHALL contain a generation counter at offset 0, `next_tail` at offset 8, `writer_count` at offset 16, `reader_slots` (128 × u64) starting at offset 24, `next_writer_id` at offset 1048, and `reader_slot_counter` at offset 1056, with ring buffer data starting at offset 4096

#### Scenario: Host proxy coordinates via shared fields
- **WHEN** a kernel proxy thread writes data to a ring buffer
- **THEN** it SHALL reserve space via CAS on the shared `next_tail`, perform single-phase writes with release fencing, and bump the generation counter, using the same protocol as guest writers

#### Scenario: Kernel notifies guest via generation counter
- **WHEN** a kernel proxy thread bumps the generation counter after writing data
- **THEN** the guest blocked on `memory.atomic.wait32` at the generation counter offset SHALL be woken
