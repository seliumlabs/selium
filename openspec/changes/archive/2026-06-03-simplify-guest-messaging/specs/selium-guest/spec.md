## MODIFIED Requirements

### Requirement: Safe Guest Handles
`selium-guest` SHALL provide safe, ergonomic handle types over the shared memory ABI primitives (`alloc_region`, `free_region`, `attach_region`) so guest code does not manipulate raw hostcall payloads directly for common operations.

#### Scenario: Guest opens primitive through SDK handle
- **WHEN** guest code acquires a shared memory, channel, or pub/sub resource through the SDK
- **THEN** the SDK SHALL expose a typed handle rather than requiring direct ABI framing code

### Requirement: Messaging-Pattern Layer
`selium-guest` SHALL provide a messaging-pattern layer built above the shared memory substrate, using native WASM atomics for synchronization without signal hostcalls.

#### Scenario: Guest selects messaging pattern
- **WHEN** guest code needs pub/sub, fanout, request/reply, stream, or live-table semantics
- **THEN** the SDK SHALL provide those semantics through the pattern layer rather than through guest-specific boilerplate

#### Scenario: Prototype-local pattern composition
- **WHEN** the current arch3 prototype uses the messaging-pattern layer in native tests or single-process guest logic
- **THEN** the SDK MAY satisfy those semantics through local in-memory composition while the host-backed inter-guest fabric remains future work

### Requirement: Pattern Parity
`selium-guest` SHALL treat request/reply as one messaging pattern among peers and SHALL NOT require RPC-style APIs as the privileged default for inter-guest communication.

#### Scenario: Pub/sub without RPC wrapper
- **WHEN** a guest uses pub/sub semantics for coordination
- **THEN** the SDK SHALL support that pattern directly without requiring the guest to model the interaction as request/reply first

### Requirement: Typed Codec Support
`selium-guest` SHALL provide typed codecs that map guest data types onto canonical framing rules for shared memory ring buffers.

#### Scenario: Typed payload round trip
- **WHEN** guest code sends and receives a typed payload through the SDK
- **THEN** the SDK SHALL encode and decode it using the canonical framing contract

## ADDED Requirements

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
- **WHEN** multiple writers contend for the ring buffer tail cursor
- **THEN** each writer SHALL retry CAS with increasing backoff delays up to a maximum of 64 spin-loop iterations between attempts

### Requirement: Host-Agnostic Shared Region Layout
The shared memory region SHALL contain only ring buffer data and a generation counter for `memory.atomic.wait32`/`notify` synchronization; all other metadata SHALL reside in per-guest private memory.

#### Scenario: Shared region contains no channel metadata
- **WHEN** a guest inspects the shared region layout
- **THEN** it SHALL find only ring buffer data bytes and a u64 generation counter at offset 0

## REMOVED Requirements

### Requirement: Internal Unsafe Send+Sync for Channel State
**Reason**: With shared regions mapped into guest linear memory, there is no separate `Arc`-wrapped channel state requiring `Send + Sync` bounds for Quinn integration. The Quinn integration path will be re-evaluated against the new ABI in a follow-up change.
**Migration**: Quinn integration is temporarily removed; it will be re-implemented against the new shared memory ABI.
