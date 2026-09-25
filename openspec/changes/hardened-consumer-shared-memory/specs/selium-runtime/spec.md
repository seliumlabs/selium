## MODIFIED Requirements

### Requirement: Wasmtiny-Backed Guest Execution
`selium-runtime` SHALL execute Selium guests using Wasmtiny as the WebAssembly runtime substrate, including the mmap-backed shared memory primitives, slot table hostcalls, and hostcall dispatch for networking and RPC.

#### Scenario: Runtime starts a guest module
- **WHEN** the runtime starts a valid guest module
- **THEN** it SHALL instantiate and execute that guest through Wasmtiny with access to `alloc_region`, `free_region`, `attach_region`, `alloc_slot`, `write_slot`, `free_slot`, `TcpConnect`, `TcpBind`, and `UdpBind` host functions

### Requirement: Region Lifetime Tied to Guest Lifecycle
When a guest instance terminates, `selium-runtime` SHALL automatically call `free_region` on all regions allocated by or attached to that guest, and SHALL release all slot table entries owned by that guest via `SlotManager::release_all(process_id)`.

#### Scenario: Guest exits with attached regions and owned slots
- **WHEN** a consumer guest that has attached to shared regions and owns reader slots exits
- **THEN** the runtime SHALL release all owned slots (zeroing each in shared memory and returning them to their free lists) before unmapping all shared regions from that guest's memory

## ADDED Requirements

### Requirement: Hostcall Slot Manager
`selium-runtime` SHALL maintain a `SlotManager` that tracks ownership of elements within tables in shared memory regions. The manager SHALL operate on `(region_id, table_offset)` as a compound key, supporting multiple independent tables per region (e.g., reader slots and writer slots).

The `SlotManager` SHALL maintain:
- Per-table free list (stack of available slot indices)
- Per-slot ownership mapping (`slot_index → process_id`)
- Bi-directional process-to-slots index for efficient garbage collection on process termination

#### Scenario: SlotManager allocates a slot
- **WHEN** `SlotManager::alloc(region_id, table_offset, elem_count, caller_pid)` is invoked
- **THEN** the manager SHALL pop the next free slot from the free list, record `owners[slot_index] = caller_pid`, register the slot in the process-to-slots index, and return `slot_index`

#### Scenario: SlotManager frees a slot
- **WHEN** `SlotManager::free(region_id, table_offset, slot_index, caller_pid)` is invoked
- **THEN** the manager SHALL validate `owners[slot_index] == caller_pid`, clear the ownership entry, push `slot_index` to the free list, and remove the slot from the process-to-slots index

#### Scenario: SlotManager releases all slots for a process
- **WHEN** `SlotManager::release_all(process_id)` is invoked (on guest termination)
- **THEN** the manager SHALL free every slot owned by `process_id` across all tables in all regions, zeroing each in shared memory

### Requirement: Consumer ID Tracking in HostCaller
Wasmtiny's `HostCaller` SHALL expose a `consumer_id: u64` field identifying the calling WASM instance, set by the runtime at guest instantiation. Slot hostcall handlers SHALL use this field to validate ownership.

#### Scenario: HostCaller provides caller identity
- **WHEN** a host function is invoked by a guest
- **THEN** `HostCaller::consumer_id()` SHALL return the unique identifier of the calling guest instance

#### Scenario: Slot write validates against caller identity
- **WHEN** a guest calls `write_slot`
- **THEN** the hostcall handler SHALL look up the slot owner using the `consumer_id` from `HostCaller` and reject writes to slots not owned by the caller

### Requirement: Consumer Attach Enforces ReadOnly
When a guest attaches to a shared region and the runtime's capability system determines the guest is a consumer (not a writer), the runtime SHALL attach the region with `RegionProt::ReadOnly` and `reader_slot: None`, and the guest SHALL allocate and update slot positions exclusively through hostcalls.

#### Scenario: Consumer guest attaches to a pub/sub ring
- **WHEN** a guest calls `attach_region` for a pub/sub topic and the capability system identifies the guest as a consumer
- **THEN** the runtime SHALL map the region read-only, validate layout compatibility, and the guest SHALL call `alloc_slot` to register its reader position
