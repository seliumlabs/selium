## MODIFIED Requirements

### Requirement: Shared Region Allocation via RegionProvider
The `RegionProvider` trait SHALL define `allocate(&self, pages: u32, prot: RegionProt, purpose: ResourceKind) -> Result<Region>` for allocating shared memory regions. The hostcall-backed implementation SHALL delegate to the existing `alloc_region` hostcall. The host SHALL continue to provide `alloc_region`, `free_region`, and `attach_region` hostcalls; `selium-guest` SHALL wrap them in a `HostcallRegionProvider` that implements `RegionProvider`.

#### Scenario: Guest allocates a shared region via provider
- **WHEN** a WASM guest calls `provider.allocate(pages, ReadWrite, SharedMemory)` through the installed `HostcallRegionProvider`
- **THEN** the host SHALL allocate a shared memory region, map it into the guest's linear memory, and return a `Region` with valid `region_id` and `page_offset`

#### Scenario: Guest allocates with read-only protection
- **WHEN** a guest calls `provider.allocate(pages, ReadOnly, SharedMemory)`
- **THEN** the host SHALL map the region with `PROT_READ` only

### Requirement: Shared Region Free
The `RegionProvider` trait SHALL define `free(&self, region_id: u64) -> Result<()>`. The hostcall-backed implementation SHALL delegate to the existing `free_region` hostcall.

#### Scenario: Guest frees a region via provider
- **WHEN** a guest calls `provider.free(region_id)`
- **THEN** the host SHALL unmap the region and decrement the attachment count

### Requirement: Shared Region Attach
The `RegionProvider` trait SHALL define `attach(&self, region_id: u64, reader_slot: Option<u32>, prot: RegionProt) -> Result<Region>`. The hostcall-backed implementation SHALL delegate to the existing `attach_region` hostcall.

#### Scenario: Guest attaches to an existing region via provider
- **WHEN** a guest calls `provider.attach(region_id, None, ReadWrite)`
- **THEN** the host SHALL validate the region exists, map it into guest memory, and return a `Region` with the page offset

#### Scenario: Guest attaches to a non-existent region
- **WHEN** a guest calls `provider.attach(invalid_id, ...)`
- **THEN** the provider SHALL return an error

### Requirement: Native Atomic Access to Shared Regions
Guests SHALL access shared region data and coordination fields using native WASM load, store, and atomic instructions at the page offset returned by the `RegionProvider`. `RegionMapping` SHALL continue to provide safe wrappers for these operations.

#### Scenario: Guest reads shared data via RegionMapping::read_u64
- **WHEN** a guest calls `mapping.read_u64(offset)` within a mapped shared region
- **THEN** the value SHALL be read directly from the shared memory

#### Scenario: Guest uses atomic CAS on coordination field
- **WHEN** a guest calls `mapping.compare_exchange_u64(offset, current, new)`
- **THEN** the CAS SHALL be visible to all processes attached to the same region

### Requirement: Shared Region Coordination Layout
Every shared region used for messaging SHALL include cross-process coordination fields in page 0 at fixed offsets as specified by `selium-shm::region` constants. This layout SHALL remain unchanged.

#### Scenario: Standard coordination layout
- **WHEN** a shared region is allocated for messaging via `RingBuf::create`
- **THEN** page 0 SHALL contain the standard coordination fields (generation counter, next_tail, writer_count, reader_slots, next_writer_id, reader_slot_counter) with ring buffer data starting at PAGE_SIZE
