## ADDED Requirements

### Requirement: Slot Table Hostcall Variants
`selium-abi` SHALL define `AllocSlot`, `WriteSlot`, and `FreeSlot` variants on `HostcallRequest` with the following payloads:

- `AllocSlot { region_id: u64, table_offset: u64, elem_size: u32, elem_count: u32 }` returning `{ slot_index: u32 }`
- `WriteSlot { region_id: u64, table_offset: u64, elem_size: u32, slot_index: u32, value: Vec<u8> }` returning unit
- `FreeSlot { region_id: u64, table_offset: u64, elem_size: u32, slot_index: u32 }` returning unit

These variants SHALL be generic over table purpose — the runtime does not interpret the semantics of the table (reader position, writer position, or other) and enforces only ownership.

#### Scenario: AllocSlot hostcall
- **WHEN** a guest encodes `AllocSlot { region_id: 7, table_offset: 32, elem_size: 8, elem_count: 128 }` and the runtime processes it
- **THEN** the hostcall SHALL complete with `HostcallOutput::AllocSlot { slot_index }` where `slot_index` identifies a free element in the table

#### Scenario: WriteSlot hostcall with valid ownership
- **WHEN** a guest encodes `WriteSlot { region_id: 7, table_offset: 32, elem_size: 8, slot_index: 3, value: <8 bytes> }` and the runtime processes it, and the calling process owns slot 3
- **THEN** the runtime SHALL write the 8-byte value to the shared region at offset `table_offset + slot_index * elem_size` and return success

#### Scenario: WriteSlot hostcall with invalid ownership
- **WHEN** a guest encodes `WriteSlot` for a slot owned by a different process
- **THEN** the runtime SHALL return `AbiErrorCode::PermissionDenied`

#### Scenario: FreeSlot hostcall
- **WHEN** a guest encodes `FreeSlot { region_id: 7, table_offset: 32, elem_size: 8, slot_index: 3 }` and the runtime processes it
- **THEN** the runtime SHALL validate ownership, zero the slot in shared memory, and return the slot to the free list
