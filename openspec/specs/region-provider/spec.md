## Purpose

Define the `RegionProvider` trait that abstracts shared memory region lifecycle (allocate, attach, free) behind a common interface, enabling both hostcall-backed WASM guests and native runtime code to manage shared regions without `cfg`-gated code paths.

## Requirements

### Requirement: RegionProvider Trait
`selium-memory` SHALL provide a `RegionProvider` trait that abstracts shared memory region lifecycle. The trait SHALL define:

- `allocate(&self, pages: u32, prot: RegionProt, purpose: ResourceKind) -> Result<Region>`
- `attach(&self, region_id: u64, reader_slot: Option<u32>, prot: RegionProt) -> Result<Region>`
- `free(&self, region_id: u64) -> Result<()>`

Where `Region` is a handle exposing `region_id() -> u64`, `page_offset() -> u32`, `size() -> u64`, and `mapping() -> RegionMapping`.

#### Scenario: Provider allocates a region
- **WHEN** a caller invokes `provider.allocate(2, ReadWrite, SharedMemory)`
- **THEN** the provider SHALL return a `Region` with `size == 2 * PAGE_SIZE` and a valid `region_id`

#### Scenario: Provider attaches to existing region
- **WHEN** a caller invokes `provider.attach(region_id, None, ReadWrite)` with a valid region_id
- **THEN** the provider SHALL return a `Region` whose `mapping()` shares the same underlying memory

#### Scenario: Provider frees a region
- **WHEN** a caller invokes `provider.free(region_id)`
- **THEN** subsequent `attach` to that `region_id` SHALL fail

### Requirement: Global RegionProvider Installation
`selium-memory` SHALL provide a mechanism to install a global `RegionProvider` instance (e.g., `set_region_provider()` / `region_provider()` analogous to an `OnceLock<Box<dyn RegionProvider>>`). The I/O crate (`selium-shm`) SHALL call through this global rather than directly invoking hostcalls or `cfg`-gated code paths.

#### Scenario: Provider installed before I/O operations
- **WHEN** a WASM guest calls `install_hostcall_region_provider()` during early init
- **THEN** subsequent `RingBuf::create` SHALL allocate through the hostcall-backed provider

#### Scenario: No provider installed
- **WHEN** an I/O operation is attempted without a `RegionProvider` installed
- **THEN** the operation SHALL return an error indicating no provider is configured

### Requirement: HeapRegionProvider for Testing
`selium-memory` SHALL ship a `HeapRegionProvider` that allocates heap-backed `Arc<Vec<u8>>` regions and registers them in a process-local map. This SHALL be the default provider for `cfg(not(target_arch = "wasm32"))` tests.

#### Scenario: Two HeapRegions share memory
- **WHEN** a HeapRegionProvider allocates a region, writes data via its mapping, and another caller attaches to the same region_id
- **THEN** the second mapping SHALL see the written data

### Requirement: RegionMapping Remains Transport-Agnostic
`RegionMapping` SHALL NOT depend on any `RegionProvider` or hostcall code. It SHALL remain a pure pointer-and-atomics wrapper over a `*mut u8` base, supporting `read`/`write`/`atomic_load_u64`/`atomic_store_u64`/`fetch_add_u64`/`compare_exchange_u64`/`atomic_notify`/`atomic_wait32`/`sub_region`.

#### Scenario: RegionMapping used without a provider
- **WHEN** a test constructs a `RegionMapping::allocate(256)`
- **THEN** all read/write/atomic operations SHALL function without any global state or hostcall dependency
