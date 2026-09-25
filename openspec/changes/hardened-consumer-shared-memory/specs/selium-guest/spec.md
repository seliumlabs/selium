## MODIFIED Requirements

### Requirement: Safe Guest Handles
`selium-guest` SHALL provide safe, ergonomic handle types over the shared memory ABI primitives (`alloc_region`, `free_region`, `attach_region`, `alloc_slot`, `write_slot`, `free_slot`) so guest code does not manipulate raw hostcall payloads directly for common operations.

The architecture SHALL layer region management as follows:
- `SharedRegion` (in `memory.rs`) SHALL be the canonical region lifecycle manager, wrapping `alloc_region`, `attach_region`, and `free_region` hostcalls. It SHALL provide `allocate()`, `attach()`, `free()`, and `mapping()` methods.
- `RegionMapping` (in `memory.rs`) SHALL be the low-level memory access layer, providing typed read/write/atomic operations on a raw pointer derived from a `SharedRegion`.
- `ChannelRegion` (in `io/region.rs`) SHALL wrap a `SharedRegion` and `RegionMapping` to provide ring-buffer coordination fields (generation counter, `next_tail`, reader/writer slots). It SHALL retain the underlying `SharedRegion` via `shared_region()` accessor so that hostcall-mediated slot operations can be delegated.

Reader handles (`Reader`, `BlockingReader`, `FramedRead<R>`) SHALL update their reader position through `ChannelRegion::write_table_slot` which delegates to `SharedRegion::write_table_slot` and ultimately to hostcalls. The handle types SHALL be unaware of whether slot writes are direct or hostcall-mediated.

#### Scenario: Guest opens primitive through SDK handle
- **WHEN** guest code acquires a shared memory, channel, or pub/sub resource through the SDK
- **THEN** the SDK SHALL expose a typed handle rather than requiring direct ABI framing code

#### Scenario: Reader advances position
- **WHEN** a `Reader` or `BlockingReader` advances after consuming a frame
- **THEN** the position update SHALL go through `SharedRegion::write_table_slot` — which under a read-only consumer mapping SHALL invoke the `write_slot` hostcall transparently

### Requirement: Reader/Writer Upgrade and Downgrade
`Reader` SHALL provide `downgrade(self) -> WeakReader` that calls `free_slot` to release the reader slot and returns a weak reader at the same position. `WeakReader` SHALL provide `upgrade(self) -> Result<Reader>` that calls `alloc_slot` to register a reader slot at the current position and returns a strong reader.

`Writer` SHALL provide `downgrade(self) -> WeakWriter` that decrements writer count and releases the writer slot via `free_slot`, returning a weak writer with the same writer ID. `WeakWriter` SHALL provide `upgrade(self) -> Result<Writer>` that increments writer count and calls `alloc_slot` for a writer slot, returning a strong writer.

#### Scenario: Consumer reader downgrades to weak
- **WHEN** a consumer calls `reader.downgrade()`
- **THEN** the reader slot SHALL be freed via `free_slot` hostcall (zeroing the slot in shared memory and returning it to the free list), and a `WeakReader` at the same position SHALL be returned

#### Scenario: Consumer weak reader upgrades to strong
- **WHEN** a consumer calls `WeakReader::upgrade()`
- **THEN** a new reader slot SHALL be allocated via `alloc_slot` hostcall with the current tail position, and a `Reader` with the allocated slot index SHALL be returned

## ADDED Requirements

### Requirement: ChannelHeaderLayout Offsets via memoffset
`selium-guest` SHALL define a `#[repr(C)]` struct `ChannelHeaderLayout` in `io::region` whose fields mirror the shared memory page 0 layout. The `memoffset` crate SHALL be used to derive all offset constants at compile time. Hand-written offset constants SHALL be removed.

```rust
#[repr(C)]
struct ChannelHeaderLayout {
    layout_version:     u32,
    layout_size:        u32,
    generation_counter: u64,
    next_tail:          u64,
    writer_count:       u64,
    reader_slots:       [u64; 128],
    next_writer_id:     u64,
    reader_slot_counter: u64,
    backpressure:       u8,
    _pad1:              [u8; 7],
    capacity:           u64,
    writer_slots:       [u64; 128],
    writer_slot_counter: u64,
}
```

#### Scenario: Offsets derived from ChannelHeaderLayout
- **WHEN** guest code references `READER_SLOTS_OFFSET`
- **THEN** the value SHALL be `offset_of!(ChannelHeaderLayout, reader_slots) as u64` rather than a declared constant

#### Scenario: size_of provides total header size
- **WHEN** guest code needs the header end offset (ring data start)
- **THEN** it SHALL use `size_of::<ChannelHeaderLayout>()` rather than a declared `DATA_OFFSET` constant; the ring data start SHALL be `PAGE_SIZE` (4096) which is enforced at region creation time

### Requirement: Layout Version Validation on Attach
When `Channel` or `RingBuf` attaches to an existing shared region, the attach flow SHALL validate that `layout_version` and `layout_size` in the region header match the compile-time constants `CHANNEL_LAYOUT_VERSION` and `size_of::<ChannelHeaderLayout>()`. Mismatches SHALL produce `Error::IncompatibleLayout`.

#### Scenario: Channel attaches to a compatible region
- **WHEN** `Channel::attach(region_id)` is called and the region header contains `layout_version == CHANNEL_LAYOUT_VERSION` and `layout_size == size_of::<ChannelHeaderLayout>()`
- **THEN** the attach SHALL succeed and return a `Channel`

#### Scenario: Channel attaches to an incompatible region
- **WHEN** `Channel::attach(region_id)` is called and the region header contains a different `layout_version` or `layout_size`
- **THEN** the attach SHALL fail with `Error::IncompatibleLayout { expected_version, actual_version, expected_size, actual_size }`

### Requirement: SharedRegion Slot Methods
`SharedRegion` SHALL expose `alloc_table_slot(offset, elem_size, elem_count)`, `write_table_slot(offset, elem_size, slot_index, value)`, and `free_table_slot(offset, elem_size, slot_index)` methods. These methods SHALL always delegate to the corresponding hostcalls — no direct memory write fallback is provided. The hostcall invocations SHALL pass the `region_id` and table parameters.

`ChannelRegion` SHALL expose matching `alloc_table_slot`, `write_table_slot`, and `free_table_slot` methods that delegate to its underlying `SharedRegion` (obtained via `shared_region()`). For sub-mappings created via `ChannelRegion::from_mapping()` (where no `SharedRegion` is available), these methods SHALL return an error indicating that hostcall-mediated slot operations are not supported.

#### Scenario: SharedRegion allocates a table slot
- **WHEN** `SharedRegion::alloc_table_slot(READER_SLOTS_OFFSET, 8, 128)` is called
- **THEN** the method SHALL invoke the `alloc_slot` hostcall with `(region_id, READER_SLOTS_OFFSET, 8, 128)` and return the allocated slot index

#### Scenario: SharedRegion writes a table slot
- **WHEN** `SharedRegion::write_table_slot(offset, 8, slot_index, &pos.to_le_bytes())` is called
- **THEN** the method SHALL invoke the `write_slot` hostcall with the value bytes; the caller does not know or care whether the write was direct or hostcall-mediated

#### Scenario: SharedRegion frees a table slot
- **WHEN** `SharedRegion::free_table_slot(offset, 8, slot_index)` is called
- **THEN** the method SHALL invoke the `free_slot` hostcall and return success

#### Scenario: ChannelRegion delegates slot operations to SharedRegion
- **WHEN** `ChannelRegion::write_table_slot(offset, 8, slot_index, &pos.to_le_bytes())` is called
- **THEN** the method SHALL obtain the underlying `SharedRegion` via `self.shared_region()` and delegate to `SharedRegion::write_table_slot`

#### Scenario: ChannelRegion sub-mapping rejects slot operations
- **WHEN** `ChannelRegion::write_table_slot(...)` is called on a sub-mapping created via `from_mapping()` (where `shared_region()` returns `None`)
- **THEN** the method SHALL return `Error::InvalidRegion` indicating that hostcall-mediated slot operations are not available for sub-mappings
