use std::{
    collections::HashMap,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
};

use parking_lot::Mutex;
use selium_abi::SharedResourceId;
use wasmtiny::{
    RegionProt,
    memory::Memory,
    runtime::{HostWaitSupport, RegionWaiter, SharedRegionId, Store},
};

use crate::{Error, Result, error::map_wasm_error, kernel::hashed_id};

#[derive(Clone)]
pub struct MemoryRegistry {
    pub(crate) inner: Arc<MemoryRegistryInner>,
}

pub(crate) struct SharedRegionRecord {
    pub(crate) region_id: SharedRegionId,
}

#[derive(Clone, Copy)]
pub(crate) struct SharedMappingState {
    pub(crate) region_id: SharedRegionId,
    pub(crate) shared_id: SharedResourceId,
}

pub(crate) struct MemoryRegistryInner {
    pub(crate) store: Mutex<Store>,
    pub(crate) shared_regions: Mutex<HashMap<SharedResourceId, SharedRegionRecord>>,
    pub(crate) shared_mappings: Mutex<HashMap<u64, SharedMappingState>>,
    pub(crate) next_local_id: AtomicU64,
    pub(crate) next_shared_id: AtomicU64,
    pub(crate) id_seed: u64,
}

impl MemoryRegistry {
    pub(crate) fn new(id_seed: u64) -> Self {
        Self {
            inner: Arc::new(MemoryRegistryInner {
                store: Mutex::new(Store::new()),
                shared_regions: Mutex::new(HashMap::new()),
                shared_mappings: Mutex::new(HashMap::new()),
                next_local_id: AtomicU64::new(0),
                next_shared_id: AtomicU64::new(0),
                id_seed,
            }),
        }
    }

    pub(crate) fn next_local_id(&self) -> u64 {
        loop {
            let counter = self.inner.next_local_id.fetch_add(1, Ordering::SeqCst);
            let id = hashed_id(self.inner.id_seed, counter);
            if id != 0 {
                return id;
            }
        }
    }

    pub(crate) fn next_shared_id(&self) -> u64 {
        loop {
            let counter = self.inner.next_shared_id.fetch_add(1, Ordering::SeqCst);
            let id = hashed_id(self.inner.id_seed, counter);
            if id != 0 {
                return id;
            }
        }
    }

    /// Allocates a shared memory region.
    pub fn allocate_shared_region(&self, size: u32) -> Result<(SharedResourceId, u32)> {
        let region_id = self
            .inner
            .store
            .lock()
            .allocate_shared_region(size)
            .map_err(map_wasm_error)?;
        let shared_id = self.next_shared_id();
        let len = self
            .inner
            .store
            .lock()
            .shared_region_len(region_id)
            .map_err(map_wasm_error)?;
        self.inner
            .shared_regions
            .lock()
            .insert(shared_id, SharedRegionRecord { region_id });

        Ok((shared_id, len))
    }

    /// Attaches a shared region, returning the local mapping id.
    pub fn attach_shared_region(&self, shared_id: SharedResourceId) -> Result<u64> {
        let region_id = self
            .inner
            .shared_regions
            .lock()
            .get(&shared_id)
            .map(|record| record.region_id)
            .ok_or_else(|| Error::NotFound(format!("shared region {shared_id}")))?;

        let local_id = self.next_local_id();
        let state = SharedMappingState {
            region_id,
            shared_id,
        };
        self.inner.shared_mappings.lock().insert(local_id, state);

        Ok(local_id)
    }

    /// Destroys a shared memory region when no mappings remain.
    pub fn destroy_shared_region(&self, shared_id: SharedResourceId) -> Result<()> {
        if self.shared_region_mapping_count(shared_id) > 0 {
            return Err(Error::Wasm(
                "shared region still has attached mappings".to_string(),
            ));
        }
        let region_id = self
            .inner
            .shared_regions
            .lock()
            .get(&shared_id)
            .map(|region| region.region_id)
            .ok_or_else(|| Error::NotFound(format!("shared region {shared_id}")))?;
        self.inner
            .store
            .lock()
            .destroy_shared_region(region_id)
            .map_err(map_wasm_error)?;
        self.inner.shared_regions.lock().remove(&shared_id);
        Ok(())
    }

    /// Detaches a local shared memory mapping.
    pub fn detach_shared_region(&self, local_id: u64) -> Result<()> {
        self.inner
            .shared_mappings
            .lock()
            .remove(&local_id)
            .ok_or_else(|| Error::NotFound(format!("shared mapping {local_id}")))?;
        Ok(())
    }

    /// Detaches all local mappings for a given shared region.
    pub fn detach_all_shared_mappings(&self, shared_id: SharedResourceId) {
        self.inner
            .shared_mappings
            .lock()
            .retain(|_, mapping| mapping.shared_id != shared_id);
    }

    /// Reads bytes from a shared memory region.
    pub fn read_shared_memory(&self, local_id: u64, offset: u64, len: usize) -> Result<Vec<u8>> {
        let state = self.shared_mapping(local_id)?;
        let mut bytes = vec![0_u8; len];
        self.inner
            .store
            .lock()
            .read_shared_region(state.region_id, offset as usize, &mut bytes)
            .map_err(map_wasm_error)?;
        Ok(bytes)
    }

    /// Writes bytes to a shared memory region.
    pub fn write_shared_memory(&self, local_id: u64, offset: u64, bytes: &[u8]) -> Result<()> {
        let state = self.shared_mapping(self.local_id_for(local_id)?)?;
        self.inner
            .store
            .lock()
            .write_shared_region(state.region_id, offset as usize, bytes)
            .map_err(map_wasm_error)
    }

    /// Atomically adds to a little-endian `u64` in a shared memory region.
    pub fn fetch_add_shared_memory_u64(
        &self,
        local_id: u64,
        offset: u64,
        value: u64,
    ) -> Result<u64> {
        let state = self.shared_mapping(local_id)?;
        let store = self.inner.store.lock();
        let mut bytes = [0_u8; 8];
        store
            .read_shared_region(state.region_id, offset as usize, &mut bytes)
            .map_err(map_wasm_error)?;
        let previous = u64::from_le_bytes(bytes);
        let next = previous.wrapping_add(value);
        store
            .write_shared_region(state.region_id, offset as usize, &next.to_le_bytes())
            .map_err(map_wasm_error)?;
        Ok(previous)
    }

    /// Atomically compares and exchanges a little-endian `u64` in a shared memory region.
    pub fn compare_exchange_shared_memory_u64(
        &self,
        local_id: u64,
        offset: u64,
        current: u64,
        new: u64,
    ) -> Result<u64> {
        let state = self.shared_mapping(local_id)?;
        let store = self.inner.store.lock();
        let mut bytes = [0_u8; 8];
        store
            .read_shared_region(state.region_id, offset as usize, &mut bytes)
            .map_err(map_wasm_error)?;
        let previous = u64::from_le_bytes(bytes);
        if previous == current {
            store
                .write_shared_region(state.region_id, offset as usize, &new.to_le_bytes())
                .map_err(map_wasm_error)?;
        }
        Ok(previous)
    }

    /// Returns the length of a shared region in bytes.
    pub fn shared_region_len(&self, shared_id: SharedResourceId) -> Result<u32> {
        let region_id = self
            .inner
            .shared_regions
            .lock()
            .get(&shared_id)
            .map(|record| record.region_id)
            .ok_or_else(|| Error::NotFound(format!("shared region {shared_id}")))?;
        self.inner
            .store
            .lock()
            .shared_region_len(region_id)
            .map_err(map_wasm_error)
    }

    /// Returns the shared region id backing a local mapping.
    pub fn shared_mapping_shared_id(&self, local_id: u64) -> Result<SharedResourceId> {
        Ok(self.shared_mapping(local_id)?.shared_id)
    }

    /// Returns the number of local mappings attached to a shared region.
    pub fn shared_region_mapping_count(&self, shared_id: SharedResourceId) -> usize {
        self.inner
            .shared_mappings
            .lock()
            .values()
            .filter(|mapping| mapping.shared_id == shared_id)
            .count()
    }

    /// Returns the wasmtiny `SharedRegionId` backing a selium `SharedResourceId`.
    pub fn wasmtiny_region_id(&self, shared_id: SharedResourceId) -> Result<SharedRegionId> {
        self.inner
            .shared_regions
            .lock()
            .get(&shared_id)
            .map(|record| record.region_id)
            .ok_or_else(|| Error::NotFound(format!("shared region {shared_id}")))
    }

    /// Registers a host waiter on `(shared_id, offset)` in the engine's
    /// per-region waiter registry.
    ///
    /// This is the same registry guest `memory.atomic.wait32`/`notify` use on
    /// shared ranges, so a guest notify on the address mapping `offset` wakes
    /// the returned waiter directly — no runtime transition kick involved.
    /// The caller owns the returned handle; dropping it deregisters the entry.
    pub fn register_region_waiter(
        &self,
        shared_id: SharedResourceId,
        offset: u64,
    ) -> Result<Arc<RegionWaiter>> {
        let region_id = self.wasmtiny_region_id(shared_id)?;
        let registry = self.inner.store.lock().shared_memory_registry();
        registry
            .lock()
            .register_region_waiter(region_id, offset as usize)
            .map_err(map_wasm_error)
    }

    /// Notifies up to `count` waiters registered on `(shared_id, offset)` in
    /// the engine's per-region waiter registry, and — when Stage 2 is active
    /// — additionally emits the platform wake on the region's host mapping
    /// address so OS-wait-word parkers wake too.
    ///
    /// Wakes both host waiters created via [`Self::register_region_waiter`]
    /// and guest threads parked in `memory.atomic.wait32` on the address
    /// mapping `offset`. This is the unified wait registry: guest notifies
    /// and host kicks both flow through it for engine-backed regions, and the
    /// stage-2 side effect keeps the platform primitive consistent with the
    /// registry.
    pub fn notify_region(
        &self,
        shared_id: SharedResourceId,
        offset: u64,
        count: u32,
    ) -> Result<u32> {
        let region_id = self.wasmtiny_region_id(shared_id)?;
        let registry = self.inner.store.lock().shared_memory_registry();
        let guard = registry.lock();
        let notified = guard
            .notify_region(region_id, offset as usize, count)
            .map_err(map_wasm_error)?;

        // Stage 2 side effect: parkers sleeping in the OS wait-word primitive
        // (fast-path proxies) are not reachable through the registry, so wake
        // them on the region's host mapping address. Emitted only when the
        // engine's platform-wake support is compiled in and this host has a
        // wired wait-word; otherwise a no-op.
        if crate::os_wait_word::available()
            && guard.host_wait_support() == HostWaitSupport::RegistryAndOsWake
        {
            let region = guard.get_region(region_id).map_err(map_wasm_error)?;
            // The engine's `notify_region` above checks only `offset < len`;
            // the wait-word primitive touches the full 4-byte word, so check
            // `offset + 4 <= len` here before forming the pointer.
            if offset.checked_add(4).map(|end| end as usize) > Some(region.len()) {
                return Err(Error::NotFound(format!(
                    "shared region {shared_id} wait-word offset {offset} out of bounds"
                )));
            }
            // SAFETY: `offset + 4 <= len` was just verified, and `region` is
            // the engine's live host mapping of that length, so
            // `region.ptr() + offset` is a valid 4-byte slot for the duration
            // of the wake call. 4-byte alignment holds because wait-word
            // offsets are always 4-aligned (ring generation words).
            let word = unsafe { region.ptr().add(offset as usize) };
            // SAFETY: as above; `word` is a valid, aligned 4-byte slot in
            // live shared memory for the duration of the call.
            unsafe {
                crate::os_wait_word::wake(word, count);
            }
        }
        drop(guard);
        Ok(notified)
    }

    /// Reports the engine's host-wait support level for shared regions.
    ///
    /// [`HostWaitSupport::RegistryOnly`] means Stage 1 (unified registry) is
    /// available; [`HostWaitSupport::RegistryAndOsWake`] additionally means
    /// the engine emits the platform wake primitive on guest notifies
    /// (Stage 2). This is a process-wide build-time capability — there is no
    /// runtime toggle — so it is detected rather than configured.
    pub fn host_wait_support(&self) -> HostWaitSupport {
        self.inner
            .store
            .lock()
            .shared_memory_registry()
            .lock()
            .host_wait_support()
    }

    /// True when Stage 2 wait-words are active for this kernel: the engine
    /// reports [`HostWaitSupport::RegistryAndOsWake`] (its platform wake
    /// emission is compiled in — the conformance-gated per-platform opt-in)
    /// AND this host has a wired wait-word primitive. Otherwise Stage 1 is
    /// used with identical semantics.
    pub fn stage2_active(&self) -> bool {
        crate::os_wait_word::available()
            && self.host_wait_support() == HostWaitSupport::RegistryAndOsWake
    }

    /// Returns the host mapping address of a shared region (Stage 2 wait-word
    /// target). The returned pointer is the engine's own mapping of the
    /// region; a byte at `ptr + offset` is the same physical word a guest
    /// notify's platform wake targets.
    pub fn region_host_ptr(&self, shared_id: SharedResourceId) -> Result<*mut u8> {
        let region_id = self.wasmtiny_region_id(shared_id)?;
        let registry = self.inner.store.lock().shared_memory_registry();
        let region = registry
            .lock()
            .get_region(region_id)
            .map_err(map_wasm_error)?;
        Ok(region.ptr())
    }

    pub(crate) fn shared_mapping(&self, local_id: u64) -> Result<SharedMappingState> {
        self.inner
            .shared_mappings
            .lock()
            .get(&local_id)
            .copied()
            .ok_or_else(|| Error::NotFound(format!("shared mapping {local_id}")))
    }

    fn local_id_for(&self, local_id: u64) -> Result<u64> {
        let _ = self.shared_mapping(local_id)?;
        Ok(local_id)
    }

    /// Maps a shared region into a guest's linear memory, returning the page
    /// offset where it was mapped.
    ///
    /// This operates directly on the calling guest's memory, so it works while
    /// the guest is mid-execution (e.g. inside its entrypoint), when the
    /// guest's `WasmApplication` is not available through the runtime's
    /// loaded-guest table.
    pub fn attach_shared_region_to_memory(
        &self,
        memory: &mut Memory,
        shared_id: SharedResourceId,
        prot: RegionProt,
        reader_slot: Option<u32>,
    ) -> Result<u32> {
        let region_id = self.wasmtiny_region_id(shared_id)?;
        let registry = self.inner.store.lock().shared_memory_registry();
        registry
            .lock()
            .attach_region(memory, region_id, prot, reader_slot)
            .map_err(map_wasm_error)
    }

    /// Registers a shared region that was already allocated (e.g. by WasmApplication)
    /// in the kernel's metadata, returning a new selium `SharedResourceId`.
    pub fn register_guest_allocated_region(
        &self,
        region_id: SharedRegionId,
    ) -> Result<(SharedResourceId, u32)> {
        let shared_id = self.next_shared_id();
        let len = self
            .inner
            .store
            .lock()
            .shared_region_len(region_id)
            .map_err(map_wasm_error)?;
        self.inner
            .shared_regions
            .lock()
            .insert(shared_id, SharedRegionRecord { region_id });
        Ok((shared_id, len))
    }

    /// Creates a `Store` that shares the kernel's `SharedMemoryRegistry`.
    ///
    /// This allows `WasmApplication` instances to access the same shared memory
    /// regions as the kernel, enabling direct mapping into guest linear memory.
    pub fn shared_store(&self) -> std::sync::Arc<std::sync::Mutex<Store>> {
        let registry = self.inner.store.lock().shared_memory_registry();
        std::sync::Arc::new(std::sync::Mutex::new(Store::with_shared_registry(registry)))
    }
}

#[cfg(test)]
mod tests {
    use crate::kernel::Kernel;

    #[test]
    fn shared_memory_round_trips_between_attachments() {
        let kernel = Kernel::default();
        let memory = kernel.memory();
        let (shared_id, _len) = memory.allocate_shared_region(64).expect("allocate region");
        let left = memory.attach_shared_region(shared_id).expect("attach left");
        let right = memory
            .attach_shared_region(shared_id)
            .expect("attach right");

        memory
            .write_shared_memory(left, 0, b"hello")
            .expect("write left");
        let bytes = memory.read_shared_memory(right, 0, 5).expect("read right");
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn ids_are_non_sequential_and_deterministic() {
        let kernel_a = Kernel::with_seed(42);
        let kernel_b = Kernel::with_seed(42);

        let ids_a: Vec<u64> = (0..5).map(|_| kernel_a.memory().next_shared_id()).collect();
        let ids_b: Vec<u64> = (0..5).map(|_| kernel_b.memory().next_shared_id()).collect();

        assert_eq!(ids_a, ids_b, "same seed must produce same ids");
        assert_ne!(ids_a, vec![1, 2, 3, 4, 5], "ids must not be sequential");
        assert!(ids_a.iter().all(|&id| id != 0), "ids must never be 0");
    }
}
