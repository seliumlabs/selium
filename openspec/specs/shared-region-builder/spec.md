## Purpose

Define construction and discovery of multi-memory shared regions with aligned layout headers, validated geometry, and positional sub-memory access.

## Requirements

### Requirement: Builder pattern for multi-memory regions
Multi-memory region construction SHALL lay out sub-memories with 8-byte alignment and a final header describing every offset and length. Public construction APIs SHALL validate geometry instead of exposing post-final mutation.

#### Scenario: Building an RPC session region
- **WHEN** a client creates an RPC session region with request and reply rings
- **THEN** the resulting region SHALL contain `memory_count = 2` and aligned request/reply layout entries

#### Scenario: Adding memory after seal
- **WHEN** a caller attempts to append another sub-memory through the final high-level RPC/session layout
- **THEN** no such mutation SHALL be available; a different layout SHALL require a newly constructed region

#### Scenario: Total memory exceeds capacity
- **WHEN** header and sub-memory geometry overflows or exceeds the containing region
- **THEN** construction or layout validation SHALL return an error

### Requirement: Positional sub-memory discovery
After attaching to a multi-memory region, a guest SHALL discover sub-memories through `MultiMemoryHeader::entry`. Memory 0 SHALL be the request ring and memory 1 the reply ring in RPC sessions.

#### Scenario: Server discovers request and reply rings
- **WHEN** a server attaches to a session region and reads entries 0 and 1 through the shared multi-memory parser
- **THEN** entry 0 SHALL identify the request ring and entry 1 the reply ring

#### Scenario: Accessing out-of-bounds memory index
- **WHEN** a caller requests `MultiMemoryHeader::entry(n)` where `n >= memory_count`
- **THEN** the call SHALL return `InvalidLayout`

### Requirement: Layout immutability after seal
A constructed region header (magic, capacity, memory count, offsets, lengths) SHALL be treated as immutable for that region's lifetime. Attaching parties SHALL validate the same layout rather than rewrite it.

#### Scenario: Attaching party reads layout
- **WHEN** a second party attaches to a constructed region via `shared_id`
- **THEN** the attach SHALL validate the header and enumerate the same sub-memory offsets and lengths without modifying them
