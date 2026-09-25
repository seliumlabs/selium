## MODIFIED Requirements

### Requirement: Blocking Reader Position Updates via Hostcall
`BlockingReader` SHALL update its reader slot position through `ChannelRegion::write_table_slot` rather than through direct shared memory writes. `ChannelRegion` SHALL delegate to `SharedRegion::write_table_slot`, which under a consumer mapping (`PROT_READ`) SHALL invoke the `write_slot` hostcall transparently. The `BlockingReader` does not branch on the write mechanism.

#### Scenario: BlockingReader advances after consuming a frame
- **WHEN** a `BlockingReader` advances its position after a successful read
- **THEN** it SHALL call `self.region.write_table_slot(READER_SLOTS_OFFSET, 8, self.reader_id, self.pos.to_le_bytes())` — where `self.region` is a `ChannelRegion` that delegates to its underlying `SharedRegion`, which for a read-only consumer mapping SHALL invoke the `write_slot` hostcall

### Requirement: Blocking Reader Slot Allocation via Hostcall
`BlockingReader::new` SHALL allocate a reader slot via `ChannelRegion::alloc_table_slot`. `ChannelRegion` SHALL delegate to `SharedRegion::alloc_table_slot`. It SHALL NOT directly fetch_add on the shared `reader_slot_counter` or write to `reader_slots[]`.

#### Scenario: BlockingReader allocates a slot on construction
- **WHEN** `BlockingReader::new(region, start_pos)` is called
- **THEN** it SHALL call `region.alloc_table_slot(READER_SLOTS_OFFSET, 8, 128)` to obtain a slot index, then call `region.write_table_slot(...)` with the initial start position

### Requirement: Blocking Reader Slot Release via Hostcall
`BlockingReader::close` and `Drop` SHALL release the reader slot via `ChannelRegion::free_table_slot`, which delegates to `SharedRegion::free_table_slot`. They SHALL NOT directly write zero to `reader_slots[]`.

#### Scenario: BlockingReader releases its slot on drop
- **WHEN** a `BlockingReader` is dropped
- **THEN** it SHALL call `region.free_table_slot(READER_SLOTS_OFFSET, 8, self.reader_id)` to return the slot to the free list

### Requirement: Non-Blocking Reader Overwrite Detection Unchanged
`Reader` (non-blocking) SHALL NOT allocate or update reader slots — it has none. Its overwrite detection logic (comparing `pos.wrapping_add(capacity) < tail`) SHALL remain unchanged. The `Reader` SHALL continue to read directly from read-only shared memory without hostcall overhead on the read path.

#### Scenario: Non-blocking reader reads from shared memory
- **WHEN** a `Reader` calls `poll_read` or `read_raw`
- **THEN** it SHALL read bytes directly from the shared memory mapping, incurring no hostcall overhead
