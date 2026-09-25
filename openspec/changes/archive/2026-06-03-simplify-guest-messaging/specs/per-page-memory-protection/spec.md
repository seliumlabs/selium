## ADDED Requirements

### Implementation Status (Active)
Per-page memory protection enforcement via wasmtiny's `map_shared_region` is now implemented and active.

**How it works**:
1. `AllocRegion` allocates a shared region in wasmtiny's `SharedMemoryRegistry` and returns a region id. The allocating guest must call `AttachRegion` to map the region into its linear memory.
2. `AttachRegion` calls wasmtiny's `map_shared_region()` which uses `mmap(MAP_FIXED | MAP_SHARED)` with per-page `mprotect` based on the `prot` and `reader_slot` parameters. The real `page_offset` is returned to the caller.
3. `FreeRegion` detaches the region from all loaded guests' wasm memory and cleans up all kernel-level mappings before destroying the region in wasmtiny.
4. The kernel, runtime, and wasmtiny share a single `SharedMemoryRegistry` via `Store::shared_memory_registry()`, ensuring that regions allocated through one path are visible to all others.

**Key components**:
- `wasmtiny`: Added `WasmApplication::with_store()`, `AotRuntime::with_store()` — shares a `Store` (and its `SharedMemoryRegistry`) between kernel and guests. `Memory::map_shared_region()` applies `mprotect` per page.
- `selium-kernel`: Added `shared_store()`, `wasmtiny_region_id()`, `detach_all_shared_mappings()`.
- `selium-runtime`: `load_guest_module()` uses `kernel.shared_store()`. `AllocRegion`/`AttachRegion`/`FreeRegion` hostcalls route through wasmtiny with real protection parameters.
- Integration test `attach_accepts_protection_and_reader_slot` verifies the hostcall succeeds, returns a non-zero `page_offset`, and cleanup completes without error.

### Requirement: Per-Page Memory Protection on Attach
When a guest attaches to a shared region with a `reader_slot` parameter, the host SHALL map the region such that only the designated reader cursor page is writable; all other pages SHALL be mapped read-only.

#### Scenario: Consumer attaches with reader slot
- **WHEN** a guest calls `attach_region` with `reader_slot: Some(3)`
- **THEN** the host SHALL map the region `PROT_READ` on all pages except page 3, which SHALL be mapped `PROT_READ | PROT_WRITE`

#### Scenario: Consumer attempts write to data page
- **WHEN** a consumer guest with a reader-slot-protected mapping attempts to store to a data page
- **THEN** the store SHALL trap with a memory protection fault

#### Scenario: Consumer writes to its own cursor page
- **WHEN** a consumer guest writes to its designated reader cursor page
- **THEN** the store SHALL succeed and update the cursor value

#### Scenario: Consumer attempts write to another reader's cursor page
- **WHEN** a consumer guest attempts to store to a reader cursor page it was not assigned
- **THEN** the store SHALL trap with a memory protection fault

### Requirement: Producer Full Access
A producer attaching without a `reader_slot` SHALL receive full read-write access to all pages in the shared region.

#### Scenario: Producer attaches without reader slot
- **WHEN** a guest calls `attach_region` with `reader_slot: None`
- **THEN** the host SHALL map the entire region `PROT_READ | PROT_WRITE`

### Requirement: Protection Is Kernel-Enforced
All memory protection SHALL be enforced by the operating system kernel via `mprotect`, not by runtime software checks.

#### Scenario: Malicious guest bypass attempt
- **WHEN** a guest attempts to write to a read-only shared page via any WASM store instruction
- **THEN** the kernel SHALL deliver `SIGSEGV` and the runtime SHALL translate it to a WASM trap
