## ADDED Requirements

### Requirement: Builder pattern for multi-memory regions
`SharedRegionBuilder` SHALL construct a `SharedRegion` containing one or more sub-memories with automatic 8-byte alignment padding. The builder SHALL support `add_memory(len)` calls followed by `seal()` which finalises the layout and prevents further modifications.

#### Scenario: Building an RPC session region
- **WHEN** a client constructs a region with `SharedRegionBuilder::new(65536).add_memory(32768).add_memory(32768).seal()`
- **THEN** the resulting `SharedRegion` SHALL contain a header with `memory_count = 2` and two sub-memory layout entries with aligned offsets and lengths

#### Scenario: Adding memory after seal
- **WHEN** `add_memory` is called on a sealed builder
- **THEN** the call SHALL return an error indicating the builder is sealed

#### Scenario: Total memory exceeds capacity
- **WHEN** the combined size of the header and all sub-memories exceeds the region capacity
- **THEN** `seal()` SHALL return an error

### Requirement: Positional sub-memory discovery
After attaching to a `SharedRegion`, a guest SHALL be able to discover sub-memories by positional index. Memory 0 SHALL be the request ring and memory 1 SHALL be the reply ring in RPC sessions.

#### Scenario: Server discovers request and reply rings
- **WHEN** a server attaches to a session region and calls `region.memory(0)` and `region.memory(1)`
- **THEN** the first call SHALL return the request ring sub-memory and the second SHALL return the reply ring sub-memory

#### Scenario: Accessing out-of-bounds memory index
- **WHEN** a caller accesses `region.memory(n)` where `n >= memory_count`
- **THEN** the call SHALL return an error

### Requirement: Layout immutability after seal
After `seal()` is called, the region header (magic, capacity, memory count, offsets, lengths) SHALL be written to shared memory and SHALL NOT be modifiable by subsequent operations. Any party attaching via `shared_id` SHALL be able to read the header and trust its contents.

#### Scenario: Attaching party reads layout
- **WHEN** a second party calls `SharedRegion::attach(shared_id)` on a sealed region
- **THEN** the attach SHALL succeed and the party SHALL be able to enumerate all sub-memories with their correct offsets and lengths