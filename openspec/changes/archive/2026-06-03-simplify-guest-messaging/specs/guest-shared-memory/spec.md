## ADDED Requirements

### Requirement: Shared Region Allocation via Host ABI
The host SHALL provide an `alloc_region` hostcall that allocates a shared memory region, maps it into the calling guest's linear memory, and returns a region identifier together with the page offset at which the region is accessible.

#### Scenario: Guest allocates a shared region
- **WHEN** a guest calls `alloc_region` with a valid page count and protection flag
- **THEN** the host SHALL allocate a shared memory region of the requested size, extend the guest's linear memory to include those pages, and return `(region_id, page_offset)`

#### Scenario: Guest allocates with read-only protection
- **WHEN** a guest calls `alloc_region` with `RegionProt::ReadOnly`
- **THEN** the host SHALL map the region into guest memory with `PROT_READ` only, and any store to those pages SHALL trap

### Requirement: Shared Region Free
The host SHALL provide a `free_region` hostcall that unmaps a shared region from the calling guest's linear memory and releases the region if no other instances are attached.

#### Scenario: Guest frees a region it allocated
- **WHEN** a guest calls `free_region` with a valid `region_id`
- **THEN** the host SHALL unmap the region's pages from guest memory and decrement the region's attachment count

#### Scenario: Region freed while other instances are attached
- **WHEN** a guest calls `free_region` on a region with multiple attached instances
- **THEN** the host SHALL unmap the pages from the calling guest only; the region SHALL persist until the last attachment is released

### Requirement: Shared Region Attach
The host SHALL provide an `attach_region` hostcall that maps an existing shared region into the calling guest's linear memory at a page offset determined by the runtime.

#### Scenario: Guest attaches to an existing region
- **WHEN** a guest calls `attach_region` with a valid `region_id` and protection flag
- **THEN** the host SHALL validate the region exists, map it into guest memory, and return the `page_offset`

#### Scenario: Guest attaches to a non-existent region
- **WHEN** a guest calls `attach_region` with an invalid `region_id`
- **THEN** the host SHALL return an error indicating the region was not found

### Requirement: Native Atomic Access to Shared Regions
Guests SHALL access shared region data using native WASM load, store, and atomic instructions at the page offset returned by `alloc_region` or `attach_region`.

#### Scenario: Guest reads shared data via load
- **WHEN** a guest executes `i32.load` at an address within a mapped shared region
- **THEN** the value SHALL be read directly from the shared memory without host intervention

#### Scenario: Guest writes shared data via store
- **WHEN** a guest executes `i32.store` at an address within a writable shared region
- **THEN** the value SHALL be written directly to the shared memory without host intervention

#### Scenario: Guest uses atomic wait on shared region
- **WHEN** a guest executes `memory.atomic.wait32` on an address within a mapped shared region
- **THEN** the instruction SHALL block until `memory.atomic.notify` is called on that address by another instance

#### Scenario: Guest uses atomic notify on shared region
- **WHEN** a guest executes `memory.atomic.notify` on an address within a mapped shared region
- **THEN** the instruction SHALL wake waiters blocked on `memory.atomic.wait32` at that address
