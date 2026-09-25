## MODIFIED Requirements

### Requirement: Host-Agnostic Shared Region Layout
The shared memory region SHALL contain ring buffer data, a generation counter for `memory.atomic.wait32`/`notify` synchronization, and cross-process coordination fields (`next_tail`, `writer_count`, `reader_slots`) in page 0. Process-local metadata (`tail_cache`, `next_writer_id`, `next_mutation_id`) SHALL reside in per-guest private memory.

#### Scenario: Shared region contains channel coordination metadata
- **WHEN** a guest inspects the shared region layout
- **THEN** it SHALL find a u64 generation counter at offset 0, a u64 `next_tail` at offset 8, a u64 `writer_count` at offset 16, and 128 u64 `reader_slots` starting at offset 24, followed by ring buffer data at page 1 (offset 4096)

#### Scenario: Guest manages process-local state privately
- **WHEN** a guest creates a writer for a channel
- **THEN** the writer's `tail_cache` and `writer_id` allocation state SHALL reside in `ChannelPrivateState` and SHALL NOT be visible to other processes

### Requirement: Cross-Process Writer Coordination
Multiple writers from different guest processes SHALL coordinate through the shared `next_tail` field using atomic compare-and-swap with exponential backoff.

#### Scenario: Two writers from different guests reserve space
- **WHEN** writer A in guest 1 and writer B in guest 2 both call `reserve_tail` on the same ring
- **THEN** both writers SHALL CAS on the shared `next_tail` at offset 8, and each SHALL receive a unique, non-overlapping reservation position

### Requirement: Cross-Process Backpressure
Writers SHALL enforce backpressure by reading the shared `reader_slots` array and refusing to write past the slowest strong reader's position.

#### Scenario: Writer blocks when ring is full
- **WHEN** a writer attempts to reserve space that would overwrite unread data
- **THEN** `reserve_tail` SHALL return `Error::BufferFull` after checking the minimum position across all shared `reader_slots`

### Requirement: Cross-Process EOF Detection
Readers SHALL detect end-of-stream by observing when the shared `writer_count` reaches zero.

#### Scenario: Reader detects all writers disconnected
- **WHEN** the last writer decrements the shared `writer_count` to 0
- **THEN** all readers SHALL observe `writer_count == 0` and treat the stream as terminated

### Requirement: Single Flat Error Type
`selium-guest` SHALL provide a single flat `Error` enum covering all messaging failure modes without nested type hierarchies or `From` chains.

#### Scenario: Error match on specific failure mode
- **WHEN** guest code matches on a channel read error
- **THEN** the error variants SHALL be directly accessible without unwrapping nested error wrappers

### Requirement: Single-Phase Frame Write
The ring buffer SHALL use a single-phase write protocol with release/acquire fencing, writing the payload before the header and using a single header write with a READY flag.

#### Scenario: Successful frame write
- **WHEN** a writer writes a frame to the ring buffer
- **THEN** it SHALL write the payload, issue a release fence, then write the header with the READY flag set

#### Scenario: Reader observes complete frame
- **WHEN** a reader polls for a frame
- **THEN** it SHALL issue an acquire fence before reading the header, and SHALL only read the payload if the READY flag is set

### Requirement: Exponential Backoff in CAS Loops
All atomic compare-and-swap loops SHALL use exponential backoff with no hard iteration limit.

#### Scenario: Contended tail reservation
- **WHEN** multiple writers contend for the shared `next_tail` cursor
- **THEN** each writer SHALL retry CAS with increasing backoff delays up to a maximum of 64 spin-loop iterations between attempts

### Requirement: Guest Context with Discovery RPC
`selium-guest` SHALL provide a `Context` struct that resolves URIs to `ResourceTarget` values via an RPC client connected to the discovery system guest.

#### Scenario: Guest resolves a known URI
- **WHEN** a guest calls `Context::lookup(uri)` with a URI registered in discovery
- **THEN** the context SHALL send a `DiscoveryRequest::Resolve` RPC request and return `Some(ResourceTarget)` if found

#### Scenario: Guest resolves an unknown URI
- **WHEN** a guest calls `Context::lookup(uri)` with an unregistered URI
- **THEN** the context SHALL return `Ok(None)` indicating the URI was not found

#### Scenario: Context created from raw discovery handle
- **WHEN** the runtime passes a discovery `shared_id` to `Context::from_raw(discovery_handle)`
- **THEN** the guest SHALL attach a `ResourceSender` to the host queue and connect an `RpcClient` for discovery requests

## ADDED Requirements

### Requirement: Shared Writer ID Allocation
Writer IDs SHALL be allocated via a shared atomic counter in the shared region, ensuring global uniqueness across all guest processes.

#### Scenario: Two guests allocate writer IDs
- **WHEN** guest A and guest B both allocate writer IDs on the same channel
- **THEN** each SHALL receive a unique ID via atomic `fetch_add` on the shared `next_writer_id` field

### Requirement: Shared Reader Slot Allocation
Reader slot indices SHALL be allocated via a shared atomic counter, ensuring each strong reader owns a unique slot in the `reader_slots` array.

#### Scenario: Two guests register strong readers
- **WHEN** guest A and guest B both register strong readers on the same channel
- **THEN** each SHALL receive a unique slot index via atomic `fetch_add` on the shared reader slot counter
