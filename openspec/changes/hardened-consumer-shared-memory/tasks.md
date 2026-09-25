## 1. wasmtiny: HostCaller + Instance identity

- [ ] 1.1 Add `consumer_id: u64` field to `Instance`
- [ ] 1.2 Add `consumer_id: u64` field to `HostCaller` (`memory.rs`)
- [ ] 1.3 Add `HostCaller::consumer_id(&self) -> u64` accessor
- [ ] 1.4 Pass `self.consumer_id` from `Instance::call_cloned_host_func` to `HostCaller::new`
- [ ] 1.5 Write wasmtiny unit tests for `consumer_id` round-trip

## 2. selium-abi: Slot hostcall types

- [ ] 2.1 Add `AllocSlot { region_id, table_offset, elem_size, elem_count }` to `HostcallRequest`
- [ ] 2.2 Add `WriteSlot { region_id, table_offset, elem_size, slot_index, value }` to `HostcallRequest`
- [ ] 2.3 Add `FreeSlot { region_id, table_offset, elem_size, slot_index }` to `HostcallRequest`
- [ ] 2.4 Add `AllocSlot { slot_index }` to `HostcallOutput`
- [ ] 2.5 Add `WriteSlot` and `FreeSlot` (unit) to `HostcallOutput`
- [ ] 2.6 Implement serialization/deserialization for new variants

## 3. selium-guest: ChannelHeaderLayout + memoffset

- [ ] 3.1 Add `memoffset` to `Cargo.toml` dependencies
- [ ] 3.2 Define `ChannelHeaderLayout` `#[repr(C)]` struct in `io::region`
- [ ] 3.3 Replace all hand-written offset constants with `offset_of!(ChannelHeaderLayout, field)`:
  - `GENERATION_COUNTER_OFFSET`
  - `NEXT_TAIL_OFFSET`
  - `WRITER_COUNT_OFFSET`
  - `READER_SLOTS_OFFSET`
  - `NEXT_WRITER_ID_OFFSET`
  - `READER_SLOT_COUNTER_OFFSET`
  - `BACKPRESSURE_OFFSET`
  - `SHARED_CAPACITY_OFFSET`
  - `WRITER_SLOTS_OFFSET`
  - `WRITER_SLOT_COUNTER_OFFSET`
- [ ] 3.4 Add `CHANNEL_LAYOUT_VERSION: u32 = 1` constant
- [ ] 3.5 Add `Error::IncompatibleLayout { expected_version, actual_version, expected_size, actual_size }` variant
- [ ] 3.6 Update `RegionBuilder::initialise()` to write `layout_version` and `layout_size`
- [ ] 3.7 Add layout validation in `RingBuf::attach` / `Channel::attach` — read version + size, compare
- [ ] 3.8 Add `SharedRegion::alloc_table_slot(offset, elem_size, elem_count) -> Result<u32>`
- [ ] 3.9 Add `SharedRegion::write_table_slot(offset, elem_size, slot_index, value) -> Result<()>`
- [ ] 3.10 Add `SharedRegion::free_table_slot(offset, elem_size, slot_index) -> Result<()>`
- [ ] 3.11 Update existing tests for new offsets (+8 bytes shift)
- [ ] 3.12 Write tests for layout version validation (compatible and incompatible)
- [ ] 3.13 Write tests for `offset_of!` correctness against known byte positions

## 4. selium-runtime: SlotManager + hostcall dispatch

- [ ] 4.1 Implement `SlotManager` struct with:
  - `tables: HashMap<(region_id, table_offset), TableMeta>`
  - `TableMeta { owners: HashMap<slot_index, process_id>, free_list: Vec<slot_index> }`
  - `process_slots: HashMap<process_id, Vec<(region_id, table_offset, slot_index)>>`
- [ ] 4.2 Implement `SlotManager::alloc(region_id, table_offset, elem_count, pid) -> slot_index`
- [ ] 4.3 Implement `SlotManager::write(region_id, table_offset, elem_size, slot, pid, value)` — validate ownership + write to shm
- [ ] 4.4 Implement `SlotManager::free(region_id, table_offset, slot, pid)` — validate + zero shm + return to freelist
- [ ] 4.5 Implement `SlotManager::release_all(pid)` — free all slots for a process, zero shm
- [ ] 4.6 Set `consumer_id` on `Instance` during guest bootstrap in `load_guest_module`
- [ ] 4.7 Register `alloc_slot`, `write_slot`, `free_slot` host functions via `register_optional_host_function`
- [ ] 4.8 Implement `AllocSlot` hostcall handler — validate region attached, delegate to `SlotManager::alloc`
- [ ] 4.9 Implement `WriteSlot` hostcall handler — validate region attached, delegate to `SlotManager::write`
- [ ] 4.10 Implement `FreeSlot` hostcall handler — delegate to `SlotManager::free`
- [ ] 4.11 Call `SlotManager::release_all(process_id)` on guest termination (in region detach loop)
- [ ] 4.12 Update consumer attach path to use `RegionProt::ReadOnly` + `reader_slot: None`
- [ ] 4.13 Write unit tests for `SlotManager` allocation, ownership validation, and GC
- [ ] 4.14 Write integration test: consumer attaches, allocs slot, writes position, frees slot

## 5. selium-guest I/O: Reader + BlockingReader migration

- [ ] 5.1 Update `BlockingReader::new` to call `region.alloc_table_slot(READER_SLOTS_OFFSET, 8, 128)` instead of direct shared-memory allocation
- [ ] 5.2 Update `BlockingReader::advance` to call `region.write_table_slot(READER_SLOTS_OFFSET, 8, self.reader_id, self.pos.to_le_bytes())`
- [ ] 5.3 Update `BlockingReader::close` and `Drop` to call `region.free_table_slot(...)` instead of direct `release_reader_slot`
- [ ] 5.4 Update `Reader::upgrade` to call `region.alloc_table_slot(...)` for slot allocation
- [ ] 5.5 Update `BlockingReader::downgrade` to call `region.free_table_slot(...)` for slot release
- [ ] 5.6 No changes to `Reader` (non-blocking) read path — it doesn't own a slot
- [ ] 5.7 Update `pubsub::reader_from_ring` to use new allocation path
- [ ] 5.8 Write tests: consumer with read-only mapping reads frames and updates position via hostcalls
- [ ] 5.9 Write tests: consumer write to shared memory traps (SIGSEGV → WASM trap)

## 6. Integration validation

- [ ] 6.1 End-to-end test: publisher writes frames, consumer reads them, positions update correctly, writers see backpressure
- [ ] 6.2 Security test: malicious consumer attempts to write to another consumer's slot — permission denied
- [ ] 6.3 Security test: malicious consumer attempts to write to `next_tail` offset — traps
- [ ] 6.4 Lifecycle test: consumer process killed — all slots freed, no stale positions
- [ ] 6.5 Lifecycle test: consumer detaches cleanly — slot returned to freelist, next alloc gets the same slot
- [ ] 6.6 Layout test: old region (no version field) fails attach with `IncompatibleLayout`
- [ ] 6.7 Layout test: region with wrong `layout_size` fails attach
