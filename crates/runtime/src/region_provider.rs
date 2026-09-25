//! Runtime-backed [`RegionProvider`].
//!
//! The runtime manages shared memory regions through `wasmtiny` and the
//! `selium-kernel` crate. This module lets the runtime itself participate in
//! shared-memory I/O (e.g. discovery pub/sub) using the same `selium-shm` and
//! `selium-memory` abstractions as guests.
//!
//! [`KernelBackend`] (the [`MappingBackend`] implementation) now lives in
//! `selium-kernel` so that both the kernel and runtime can share it without
//! creating a circular dependency.

use std::{
    collections::HashMap,
    fmt::Debug,
    sync::{Arc, Mutex},
};

use selium_abi::{RegionAllocation, RegionProt, ResourceKind};
use selium_kernel::{Kernel, KernelBackend};
use selium_memory::{MappingBackend, MemoryError, Region, RegionProvider, Result};

/// Region provider backed by the runtime kernel's shared region table.
#[derive(Clone)]
pub struct RuntimeRegionProvider {
    kernel: Kernel,
    /// Local mapping ids created by this provider, keyed by shared region id.
    /// Detached in `free` so the kernel can destroy the region.
    local_ids: Arc<Mutex<HashMap<u64, u64>>>,
}

impl RuntimeRegionProvider {
    /// Creates a new provider backed by the given kernel.
    pub fn new(kernel: Kernel) -> Self {
        Self {
            kernel,
            local_ids: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl RegionProvider for RuntimeRegionProvider {
    fn allocate(&self, pages: u32, _prot: RegionProt, _purpose: ResourceKind) -> Result<Region> {
        let size_bytes = pages as u64 * selium_memory::WASM_PAGE_SIZE;
        let size_u32 = u32::try_from(size_bytes)
            .map_err(|_error| MemoryError::Other("region size exceeds u32".to_string()))?;
        let memory = self.kernel.memory();
        let (shared_id, len) = memory
            .allocate_shared_region(size_u32)
            .map_err(|error| MemoryError::Other(error.to_string()))?;
        let local_id = memory
            .attach_shared_region(shared_id)
            .map_err(|error| MemoryError::Other(error.to_string()))?;
        self.local_ids
            .lock()
            .map_err(|error| MemoryError::Other(error.to_string()))?
            .insert(shared_id, local_id);
        let backend: Arc<dyn MappingBackend> =
            Arc::new(KernelBackend::new(memory, local_id, shared_id, len as u64));
        Ok(Region::with_backend(
            RegionAllocation {
                region_id: shared_id,
                page_offset: 0,
            },
            backend,
        ))
    }

    fn attach(
        &self,
        region_id: u64,
        _reader_slot: Option<u32>,
        _prot: RegionProt,
    ) -> Result<Region> {
        let memory = self.kernel.memory();
        let local_id = memory
            .attach_shared_region(region_id)
            .map_err(|error| MemoryError::Other(error.to_string()))?;
        let len = memory
            .shared_region_len(region_id)
            .map_err(|error| MemoryError::Other(error.to_string()))?;
        let backend: Arc<dyn MappingBackend> =
            Arc::new(KernelBackend::new(memory, local_id, region_id, len as u64));
        Ok(Region::with_backend(
            RegionAllocation {
                region_id,
                page_offset: 0,
            },
            backend,
        ))
    }

    fn free(&self, region_id: u64) -> Result<()> {
        let memory = self.kernel.memory();
        if let Some(local_id) = self
            .local_ids
            .lock()
            .map_err(|error| MemoryError::Other(error.to_string()))?
            .remove(&region_id)
        {
            drop(memory.detach_shared_region(local_id));
        }
        memory
            .destroy_shared_region(region_id)
            .map_err(|error| MemoryError::Other(error.to_string()))
    }
}

impl Debug for RuntimeRegionProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeRegionProvider").finish()
    }
}
