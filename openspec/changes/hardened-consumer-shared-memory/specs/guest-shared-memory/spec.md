## MODIFIED Requirements

### Requirement: Shared Region Coordination Layout
Every shared region used for messaging SHALL include cross-process coordination fields in page 0 at offsets derived from a single `#[repr(C)]` layout struct, `ChannelHeaderLayout`, using `memoffset`. All offset constants SHALL be computed at compile time via `offset_of!(ChannelHeaderLayout, field)` rather than declared as hand-written constants.

The layout version SHALL occupy bytes 0–3 (`layout_version: u32`). The layout size SHALL occupy bytes 4–7 (`layout_size: u32`, equal to `size_of::<ChannelHeaderLayout>()`). The remaining fields follow with natural `#[repr(C)]` alignment.

#### Scenario: Standard coordination layout v1
- **WHEN** a shared region is allocated for messaging with layout version 1
- **THEN** page 0 SHALL contain `layout_version` at offset 0, `layout_size` at offset 4, `generation_counter` at offset 8, `next_tail` at offset 16, `writer_count` at offset 24, `reader_slots` (128 × u64) at offset 32, `next_writer_id` at offset 1056, `reader_slot_counter` at offset 1064, `backpressure` at offset 1072, `capacity` at offset 1080, `writer_slots` (128 × u64) at offset 1088, and `writer_slot_counter` at offset 2112, with ring buffer data starting at offset 4096

#### Scenario: ChannelHeaderLayout is the single source of truth
- **WHEN** a developer changes the field order, type, or count in `ChannelHeaderLayout`
- **THEN** all constants (`READER_SLOTS_OFFSET`, `NEXT_TAIL_OFFSET`, etc.) SHALL shift automatically via `offset_of!` rather than requiring manual recomputation of magic numbers

#### Scenario: size_of validation
- **WHEN** a guest attaches to a region
- **THEN** the runtime SHALL validate that `layout_size` in the region header equals `size_of::<ChannelHeaderLayout>()` — failing the attach if they differ

### Requirement: Native Atomic Access to Shared Regions
Guests SHALL access shared region data and coordination fields using native WASM load, store, and atomic instructions at the page offset returned by attach. Consumer guests SHALL receive read-only mappings; all writes to shared memory by consumers SHALL be mediated through slot hostcalls.

#### Scenario: Guest reads shared data via load
- **WHEN** a guest executes `i32.load` at an address within a mapped shared region
- **THEN** the value SHALL be read directly from the shared memory without host intervention

#### Scenario: Writer writes shared data via store
- **WHEN** a writer guest executes `i32.store` at an address within a writable shared region
- **THEN** the value SHALL be written directly to the shared memory without host intervention

#### Scenario: Consumer write to shared memory traps
- **WHEN** a consumer guest executes `i32.store` at any address within a shared region (mapped `PROT_READ`)
- **THEN** the store SHALL trap with a memory protection fault

#### Scenario: Guest uses atomic wait on shared region
- **WHEN** a guest executes `memory.atomic.wait32` on an address within a mapped shared region
- **THEN** the instruction SHALL block until `memory.atomic.notify` is called on that address by another instance

#### Scenario: Guest uses atomic notify on shared region
- **WHEN** a guest executes `memory.atomic.notify` on an address within a mapped shared region
- **THEN** the instruction SHALL wake waiters blocked on `memory.atomic.wait32` at that address

### Requirement: Shared Region Attach
The host SHALL provide an `attach_region` hostcall that maps an existing shared region into the calling guest's linear memory. The guest SDK SHALL wrap this hostcall via `SharedRegion::attach(region_id, reader_slot, prot)`, which returns a `SharedRegion` instance. Consumer guests SHALL attach with `RegionProt::ReadOnly` and `reader_slot: None`. The host SHALL validate the region exists and is compatible with the guest's expected layout (version and size match).

#### Scenario: Consumer attaches to a region
- **WHEN** a consumer guest calls `attach_region` with `region_id`, `prot: ReadOnly`, `reader_slot: None`
- **THEN** the host SHALL map the entire region as `PROT_READ`, validate the layout version and size, and return the page offset

#### Scenario: Writer attaches to a region
- **WHEN** a writer guest calls `attach_region` with `region_id`, `prot: ReadWrite`, `reader_slot: None`
- **THEN** the host SHALL map the entire region as `PROT_READ | PROT_WRITE` and return the page offset

#### Scenario: Layout version mismatch on attach
- **WHEN** a guest attaches to a region whose `layout_version` differs from the guest's expected `CHANNEL_LAYOUT_VERSION`
- **THEN** the attach SHALL fail with an `IncompatibleLayout` error

#### Scenario: Layout size mismatch on attach
- **WHEN** a guest attaches to a region whose `layout_size` differs from `size_of::<ChannelHeaderLayout>()`
- **THEN** the attach SHALL fail with an `IncompatibleLayout` error

## ADDED Requirements

### Requirement: Layout Versioning and Validation
Every shared region SHALL store a `layout_version: u32` and `layout_size: u32` at the head of page 0. The version SHALL be monotonically incremented on each breaking layout change. On attach, the guest SHALL validate both match its compile-time expectations.

#### Scenario: Creator initialises layout version
- **WHEN** a new shared region is created
- **THEN** `layout_version` SHALL be set to the current `CHANNEL_LAYOUT_VERSION` constant and `layout_size` SHALL be set to `size_of::<ChannelHeaderLayout>()`

#### Scenario: Forward-compatible attach
- **WHEN** a guest compiled with layout version N attaches to a region with layout version N-1
- **THEN** if the guest supports backward compatibility for that version delta, the attach MAY succeed; otherwise it SHALL fail with `IncompatibleLayout`

### Requirement: Hostcall-Mediated Slot Writes for Consumers
The runtime SHALL provide `alloc_slot`, `write_slot`, and `free_slot` hostcalls that allow consumer guests to write to designated table slots in shared memory through validated host-mediated operations. Consumers SHALL never write to shared memory directly.

#### Scenario: Consumer allocates a reader slot
- **WHEN** a consumer guest calls `alloc_slot(region_id, table_offset, elem_size, elem_count)` with the region's `reader_slots` table parameters
- **THEN** the runtime SHALL allocate the next free slot, record the calling process as its owner, write the initial position to shared memory, and return the `slot_index`

#### Scenario: Consumer updates its reader position
- **WHEN** a consumer guest calls `write_slot(region_id, table_offset, elem_size, slot_index, value_bytes)`
- **THEN** the runtime SHALL validate that the calling process owns `slot_index` in table `(region_id, table_offset)`, write `value_bytes` into shared memory, and return success

#### Scenario: Consumer writes to a slot it does not own
- **WHEN** a consumer guest calls `write_slot` with a `slot_index` not owned by the calling process
- **THEN** the runtime SHALL trap the guest with `PermissionDenied`

#### Scenario: Consumer frees its reader slot
- **WHEN** a consumer guest calls `free_slot(region_id, table_offset, elem_size, slot_index)`
- **THEN** the runtime SHALL validate ownership, zero the slot value in shared memory, and return the slot to the free list

#### Scenario: Consumer detaches without freeing slot
- **WHEN** a consumer guest terminates or detaches a region without calling `free_slot`
- **THEN** the runtime SHALL automatically free all slots owned by the terminating process during guest teardown

### Requirement: ChannelHeaderLayout as Single Source of Truth
`selium-guest` SHALL define a `#[repr(C)]` struct `ChannelHeaderLayout` that mirrors the shared memory layout of page 0. All offset constants SHALL be derived via `offset_of!` from the `memoffset` crate. Each field MAY carry a doc-comment describing its purpose.

Explicit padding fields (e.g., `_pad1: [u8; 7]`) SHALL be used where the next field requires alignment beyond what the preceding field provides. This keeps the layout byte-exact and auditable without relying on implicit compiler padding.

The initial layout version SHALL be `1`. The version SHALL be incremented monotonically on every breaking layout change (field reordering, addition, removal, or type change). `CHANNEL_LAYOUT_VERSION` SHALL be a compile-time `u32` constant.

#### Scenario: Backfill pad after single-byte field
- **WHEN** `backpressure: u8` at offset 1072 is followed by `capacity: u64` requiring 8-byte alignment
- **THEN** `_pad1: [u8; 7]` SHALL occupy bytes 1073–1079, and `capacity` SHALL sit at offset 1080

#### Scenario: Adding a new field to the layout
- **WHEN** a developer adds a field to `ChannelHeaderLayout`
- **THEN** the struct SHALL be reordered as appropriate for alignment, explicit padding fields SHALL be adjusted, `offset_of!` SHALL automatically shift dependent constants, `size_of` SHALL produce the new header size, and `CHANNEL_LAYOUT_VERSION` SHALL be bumped

#### Scenario: Hostcall slot operations reference layout-derived offsets
- **WHEN** guest code calls `alloc_slot(region_id, READER_SLOTS_OFFSET, 8, 128)`
- **THEN** `READER_SLOTS_OFFSET` SHALL be `offset_of!(ChannelHeaderLayout, reader_slots)` rather than a hand-written integer

#### Scenario: Old regions (no version field) are rejected
- **WHEN** a guest compiled with layout version 1 attaches to a region created before versioning was introduced
- **THEN** the region's bytes at offset 0 (the old `generation_counter` low 32 bits) SHALL not match version 1, causing `IncompatibleLayout`; this is intentional — there is no backward compatibility with the pre-versioning layout
