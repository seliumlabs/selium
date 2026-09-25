//! `KernelBackend`: a [`MappingBackend`] backed by the kernel's
//! Store-mediated shared memory.
//!
//! This lets the kernel participate in shared-memory ring I/O using the same
//! [`crate`] ring protocol primitives as guests, without depending on the
//! runtime (which would create a cycle: runtime → kernel → runtime). The
//! runtime re-uses this implementation via its dependency on `selium-kernel`.

use std::{
    fmt::Debug,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use selium_memory::{MappingBackend, MemoryError, Result};
use wasmtiny::runtime::{RegionWaiter, WakeOutcome};

use crate::memory::MemoryRegistry;

/// Mapping backend that delegates reads, writes, and atomics to the kernel's
/// shared-memory store. All operations go through the kernel's mutex-protected
/// accessor methods, providing a consistent (if coarser-grained) atomicity
/// domain for host-side ring operations.
#[derive(Clone)]
pub struct KernelBackend {
    memory: MemoryRegistry,
    /// Kernel local mapping id used for read/write/atomic calls.
    pub(crate) local_id: u64,
    /// Selium shared region id.
    pub(crate) shared_id: u64,
    /// Byte offset within the shared region that this mapping starts at.
    base_offset: u64,
    /// Size of this mapping in bytes.
    size: u64,
}

impl KernelBackend {
    /// Creates a new backend wrapping the given kernel mapping.
    pub fn new(memory: MemoryRegistry, local_id: u64, shared_id: u64, size: u64) -> Self {
        Self {
            memory,
            local_id,
            shared_id,
            base_offset: 0,
            size,
        }
    }

    /// Returns the local mapping id.
    pub fn local_id(&self) -> u64 {
        self.local_id
    }

    fn offset(&self, offset: u64) -> Result<u64> {
        offset
            .checked_add(self.base_offset)
            .ok_or(MemoryError::CapacityExceeded)
    }

    /// True when Stage 2 wait-words are active for this backend: the engine
    /// reports `RegistryAndOsWake` (its platform wake emission is compiled in
    /// — the conformance-gated per-platform opt-in) AND a host wait-word
    /// primitive is wired on this platform. Otherwise Stage 1 is used with
    /// identical semantics.
    fn stage2_active(&self) -> bool {
        self.memory.stage2_active()
    }
}

impl MappingBackend for KernelBackend {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let offset = self.offset(offset)?;
        self.memory
            .read_shared_memory(self.local_id, offset, len as usize)
            .map_err(|error| MemoryError::Other(error.to_string()))
    }

    fn write(&self, offset: u64, bytes: &[u8]) -> Result<()> {
        let offset = self.offset(offset)?;
        self.memory
            .write_shared_memory(self.local_id, offset, bytes)
            .map_err(|error| MemoryError::Other(error.to_string()))
    }

    fn atomic_load_u64(&self, offset: u64, _ordering: Ordering) -> Result<u64> {
        let bytes = self.read(offset, 8)?;
        Ok(u64::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_error| MemoryError::InvalidLayout)?,
        ))
    }

    fn atomic_store_u64(&self, offset: u64, value: u64, _ordering: Ordering) -> Result<()> {
        self.write(offset, &value.to_le_bytes())
    }

    fn fetch_add_u64(&self, offset: u64, value: u64, _ordering: Ordering) -> Result<u64> {
        let offset = self.offset(offset)?;
        self.memory
            .fetch_add_shared_memory_u64(self.local_id, offset, value)
            .map_err(|error| MemoryError::Other(error.to_string()))
    }

    fn compare_exchange_u64(&self, offset: u64, current: u64, new: u64) -> Result<u64> {
        let offset = self.offset(offset)?;
        self.memory
            .compare_exchange_shared_memory_u64(self.local_id, offset, current, new)
            .map_err(|error| MemoryError::Other(error.to_string()))
    }

    fn atomic_notify(&self, offset: u64, count: u32) -> Result<u32> {
        let effective_offset = self.offset(offset)?;
        self.memory
            .notify_region(self.shared_id, effective_offset, count)
            .map_err(|error| MemoryError::Other(error.to_string()))
    }

    fn atomic_wait32(&self, offset: u64, expected: u32, timeout_ms: u64) -> Result<()> {
        let effective_offset = self.offset(offset)?;

        // Stage 2: park on the OS wait-word primitive at the region's host
        // mapping address. The primitive atomically re-checks the word, so no
        // separate re-check is needed. Only active when the engine emits the
        // matching platform wake (`RegistryAndOsWake` — the conformance-gated
        // opt-in) AND this host has a wired wait-word.
        if self.stage2_active() {
            // The wait-word primitive touches the full 4-byte word; check
            // `effective_offset + 4` against the region length explicitly
            // (`self.offset` only guards addition overflow).
            let len =
                self.memory
                    .shared_region_len(self.shared_id)
                    .map_err(|error| MemoryError::Other(error.to_string()))? as u64;
            if effective_offset + 4 > len {
                return Err(MemoryError::IndexOutOfBounds);
            }
            let base = self
                .memory
                .region_host_ptr(self.shared_id)
                .map_err(|error| MemoryError::Other(error.to_string()))?;
            // SAFETY: `effective_offset + 4 <= len` was just verified, and
            // `base` is the engine's live host mapping of a region of `len`
            // bytes, so `base + effective_offset` is a valid 4-byte slot for
            // the duration of the wait.
            let word = unsafe { base.add(effective_offset as usize) };
            // SAFETY: as above; additionally the word is 4-byte aligned
            // (generation words sit 4-aligned within the ring header).
            let woken = unsafe { crate::os_wait_word::wait(word, expected, timeout_ms) };
            return if woken {
                Ok(())
            } else {
                Err(MemoryError::Other("wait32 timed out".to_string()))
            };
        }

        // Stage 1: register on the engine's per-region waiter registry BEFORE
        // re-checking the word, so a guest notify landing between the two
        // steps is latched on the waiter rather than lost (the register →
        // re-check → wait idiom; see `wasmtiny::runtime::RegionWaiter`).
        let waiter = self
            .memory
            .register_region_waiter(self.shared_id, effective_offset)
            .map_err(|error| MemoryError::Other(error.to_string()))?;

        let bytes = self
            .memory
            .read_shared_memory(self.local_id, effective_offset, 4)
            .map_err(|error| MemoryError::Other(error.to_string()))?;
        let actual = u32::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_error| MemoryError::InvalidLayout)?,
        );
        if actual != expected {
            return Ok(());
        }

        wait_on_region_waiter(&waiter, timeout_ms)
    }

    fn sub_region(&self, offset: u64, size: u64) -> Result<Arc<dyn MappingBackend>> {
        if offset
            .checked_add(size)
            .ok_or(MemoryError::CapacityExceeded)?
            > self.size
        {
            return Err(MemoryError::IndexOutOfBounds);
        }
        let base_offset = self.offset(offset)?;
        Ok(Arc::new(KernelBackend {
            memory: self.memory.clone(),
            local_id: self.local_id,
            shared_id: self.shared_id,
            base_offset,
            size,
        }))
    }

    fn as_debug(&self) -> &dyn Debug {
        self
    }
}

impl Debug for KernelBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KernelBackend")
            .field("local_id", &self.local_id)
            .field("shared_id", &self.shared_id)
            .field("base_offset", &self.base_offset)
            .field("size", &self.size)
            .finish()
    }
}

/// Helper: allocates a shared region and creates a `KernelBackend` for it.
impl MemoryRegistry {
    /// Allocates a shared region and returns a `KernelBackend` covering the
    /// full region, plus the shared region id.
    pub fn allocate_backend(&self, size: u32) -> crate::Result<(KernelBackend, u64)> {
        let (shared_id, len) = self.allocate_shared_region(size)?;
        let local_id = self.attach_shared_region(shared_id)?;
        Ok((
            KernelBackend::new(self.clone(), local_id, shared_id, len as u64),
            shared_id,
        ))
    }

    /// Attaches to an existing shared region and returns a `KernelBackend`
    /// covering the full region.
    pub fn attach_backend(&self, shared_id: u64) -> crate::Result<KernelBackend> {
        let local_id = self.attach_shared_region(shared_id)?;
        let len = self.shared_region_len(shared_id)?;
        Ok(KernelBackend::new(
            self.clone(),
            local_id,
            shared_id,
            len as u64,
        ))
    }
}

/// Parks on a registered region waiter with the given timeout in
/// milliseconds. `u64::MAX` blocks indefinitely; indefinite waits chunk into
/// bounded steps so `Instant` deadlines never overflow while still blocking
/// until a notify lands.
fn wait_on_region_waiter(waiter: &RegionWaiter, timeout_ms: u64) -> Result<()> {
    loop {
        let chunk = if timeout_ms == u64::MAX {
            Duration::from_secs(600)
        } else {
            Duration::from_millis(timeout_ms)
        };
        match waiter.wait(chunk) {
            Ok(WakeOutcome::Woken) => return Ok(()),
            Ok(WakeOutcome::TimedOut) if timeout_ms == u64::MAX => continue,
            Ok(WakeOutcome::TimedOut) => {
                return Err(MemoryError::Other("wait32 timed out".to_string()));
            }
            Err(error) => return Err(MemoryError::Other(error.to_string())),
        }
    }
}
