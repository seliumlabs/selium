## Purpose

TBD — Define the canonical ring buffer protocol (layout, codec, reservation, slots, multi-memory header) exactly once so that guest-side and host-side code consume a single shared implementation rather than reimplementing locally.

## Requirements

### Requirement: Single Ring Protocol Definition

The ring buffer layout (coordination offsets, frame codec, reservation
algorithm, reader/writer slot protocol, multi-memory header) SHALL be
defined exactly once and consumed by both guest-side and host-side code.
No crate SHALL reimplement these primitives locally.

#### Scenario: Guest and host read identical rings

- **WHEN** a guest writes frames to a ring with `PointerBackend` and the
  kernel reads the same ring with its Store-mediated backend
- **THEN** both sides agree on frame boundaries, readiness, capacity, and
  slot state without bespoke adapters

#### Scenario: Multi-memory header parsed identically

- **WHEN** any code parses a multi-memory region header
- **THEN** it uses the single shared definition; invalid magic or entry
  counts fail fast with a shared error

### Requirement: Backend-Generic Primitives

The ring primitives SHALL be generic over `MappingBackend` so that guest
(hardware atomic) and host (mutex-mediated) environments share the same
logic, and the atomicity contract (single writer domain per ring) SHALL
be documented and asserted where a domain tag is available.

#### Scenario: Slot allocation is uniform

- **WHEN** any reader (guest or kernel) opens a tracked read position
- **THEN** its slot index is allocated through the shared
  `reader_slot_counter` with no reserved or hard-coded indices

#### Scenario: Reservation behaves identically across backends

- **WHEN** writers reserve tail space on equivalent rings via guest and
  host backends
- **THEN** capacity, overflow, and backpressure outcomes match (given the
  same slot state), subject to the documented atomicity contract