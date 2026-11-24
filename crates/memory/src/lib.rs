//! Selium shared-memory primitives and region-provider abstraction.
//!
//! This crate is intentionally free of hostcalls, async runtimes, and
//! transport-specific code. It provides:
//!
//! - [`RegionMapping`]: an abstract view of a shared memory region, backed by
//!   either a raw pointer (WASM guests / native tests) or a delegated accessor
//!   (the runtime).
//! - [`RegionProvider`]: a trait abstracting allocation/attachment of shared
//!   memory regions.
//! - [`HeapRegionProvider`]: a process-local, heap-backed provider for native
//!   tests and in-process use.
//! - Global provider installation so that higher-level crates can allocate
//!   regions without threading a provider through every type.

// The `nightly-wasm-atomics` feature enables the genuine
// `memory.atomic.wait32` / `memory.atomic.notify` WASM intrinsics in
// `PointerBackend`. Those intrinsics live behind the
// `stdarch_wasm_atomic_wait` feature gate on nightly; gate the attribute so
// stable builds (without the cargo feature) stay on the stable compiler.
#![cfg_attr(
    all(target_arch = "wasm32", feature = "nightly-wasm-atomics"),
    feature(stdarch_wasm_atomic_wait)
)]

use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    any::Any,
    collections::HashMap,
    fmt::Debug,
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
    sync::{Arc, OnceLock},
};

use selium_abi::{RegionAllocation, RegionProt, ResourceKind};
use thiserror::Error;

pub use frame::FrameHeader;
pub use multi_memory::{
    HEADER_CAPACITY_OFFSET, HEADER_COUNT_OFFSET, HEADER_ENTRY_OFFSET, HEADER_ENTRY_SIZE,
    HEADER_SIZE_TWO_ENTRIES, MultiMemoryEntry, MultiMemoryHeader,
};

pub mod frame;
pub mod multi_memory;
#[cfg(not(target_arch = "wasm32"))]
mod waiters {
    use std::collections::HashMap;
    use std::sync::{Arc, Condvar, Mutex, OnceLock};
    use std::time::Duration;

    use super::MemoryError;

    pub(crate) struct Waiter {
        pub(crate) condvar: Condvar,
        pub(crate) notified: Mutex<bool>,
    }

    static REGISTRY: OnceLock<Mutex<HashMap<usize, Arc<Waiter>>>> = OnceLock::new();

    fn registry() -> &'static Mutex<HashMap<usize, Arc<Waiter>>> {
        REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Returns (or creates) the waiter entry for `key`.
    pub(crate) fn get_waiter(key: usize) -> Arc<Waiter> {
        let mut map = registry()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry(key)
            .or_insert_with(|| {
                Arc::new(Waiter {
                    condvar: Condvar::new(),
                    notified: Mutex::new(false),
                })
            })
            .clone()
    }

    /// Notify up to `count` waiters at `key`. Returns number notified.
    /// Since the native implementation uses a single condvar per key,
    /// we broadcast at most once.
    pub fn notify(key: usize, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let waiter = get_waiter(key);
        let mut flag = waiter
            .notified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !*flag {
            *flag = true;
            waiter.condvar.notify_one();
            1
        } else {
            0
        }
    }

    /// Park the current thread at `key` until notified or `timeout_ms` elapses.
    /// The caller MUST check the waited-on value before and after this call;
    /// this function only handles the blocking/unblocking.
    pub fn wait(key: usize, timeout_ms: u64) -> super::Result<()> {
        let waiter = get_waiter(key);
        let mut notified = waiter
            .notified
            .lock()
            .map_err(|_e| MemoryError::Other("waiter notified lock poisoned".to_string()))?;

        // Honour spurious / already-fired wakes.
        if *notified {
            *notified = false;
            return Ok(());
        }

        if timeout_ms == u64::MAX {
            let guard = waiter
                .condvar
                .wait(notified)
                .map_err(|_e| MemoryError::Other("condvar wait poisoned".to_string()))?;
            notified = guard;
        } else {
            let timeout = Duration::from_millis(timeout_ms);
            let (guard, wait_result) = waiter
                .condvar
                .wait_timeout(notified, timeout)
                .map_err(|_e| MemoryError::Other("condvar wait poisoned".to_string()))?;
            notified = guard;
            if wait_result.timed_out() {
                return Err(MemoryError::Other("wait32 timed out".to_string()));
            }
        }

        if *notified {
            *notified = false;
        }
        Ok(())
    }
}

/// Callback: store `waker` so it can be woken when `region_id`'s generation
/// advances past `observed_generation`.
pub type GenerationRegisterFn =
    fn(region_id: u64, observed_generation: u64, waker: &std::task::Waker);
/// Callback: wake all registered waiters for `region_id` whose observed
/// generation is less than `new_generation`.
pub type GenerationWakeFn = fn(region_id: u64, new_generation: u64);
/// Result type for selium-memory operations.
pub type Result<T> = std::result::Result<T, MemoryError>;

static GLOBAL_REGION_PROVIDER: OnceLock<Box<dyn RegionProvider>> = OnceLock::new();
static ON_GENERATION_REGISTER: OnceLock<GenerationRegisterFn> = OnceLock::new();
static ON_GENERATION_WAKE: OnceLock<GenerationWakeFn> = OnceLock::new();
/// Size of the coordination header that precedes ring buffer data inside a
/// shared region (4 KiB). This is a ring layout constant, **not** a page size.
pub const RING_HEADER_SIZE: u64 = 4096;
/// Magic value for multi-memory shared region layout headers.
pub const SHARED_REGION_MAGIC: u64 = 0x53454C49554D454D;
/// WASM linear-memory page size (64 KiB). Region page offsets returned by the
/// host are in units of this size.
pub const WASM_PAGE_SIZE: u64 = 65536;

/// Backend abstraction for [`RegionMapping`].
///
/// A backend implements the actual read/write/atomic operations for a shared
/// memory region. This lets the same [`RegionMapping`] API be used by WASM
/// guests (raw pointer into linear memory), the runtime (delegated kernel
/// accessor), and native tests (heap allocation).
pub trait MappingBackend: Send + Sync + Any {
    /// Returns the size of the mapped region in bytes.
    fn size(&self) -> u64;

    /// Reads `len` bytes starting at `offset`.
    fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>>;

    /// Writes `bytes` starting at `offset`.
    fn write(&self, offset: u64, bytes: &[u8]) -> Result<()>;

    /// Atomically loads a little-endian `u64` at `offset`.
    fn atomic_load_u64(&self, offset: u64, ordering: Ordering) -> Result<u64>;

    /// Atomically stores a little-endian `u64` at `offset`.
    fn atomic_store_u64(&self, offset: u64, value: u64, ordering: Ordering) -> Result<()>;

    /// Atomically adds `value` to the little-endian `u64` at `offset`, returning
    /// the previous value.
    fn fetch_add_u64(&self, offset: u64, value: u64, ordering: Ordering) -> Result<u64>;

    /// Atomically compares and exchanges the little-endian `u64` at `offset`.
    fn compare_exchange_u64(&self, offset: u64, current: u64, new: u64) -> Result<u64>;

    /// Notifies waiters on an address within this mapping.
    fn atomic_notify(&self, offset: u64, count: u32) -> Result<u32>;

    /// Waits on an address within this mapping.
    fn atomic_wait32(&self, offset: u64, expected: u32, timeout_ms: u64) -> Result<()>;

    /// Creates a sub-mapping backend covering `size` bytes starting at
    /// `offset` within this mapping.
    fn sub_region(&self, offset: u64, size: u64) -> Result<Arc<dyn MappingBackend>>;

    /// Returns a debug representation of this backend.
    fn as_debug(&self) -> &dyn Debug;
}

/// Abstraction over shared-memory region lifecycle.
///
/// Implementations may be backed by host hostcalls (WASM guests), a runtime
/// region table (native runtime), or a heap allocation map (native tests).
pub trait RegionProvider: Send + Sync {
    /// Allocates a shared memory region under the allocating process's own
    /// tenant.
    fn allocate(&self, pages: u32, prot: RegionProt, purpose: ResourceKind) -> Result<Region> {
        self.allocate_for_tenant(pages, prot, purpose, None)
    }

    /// Allocates a shared memory region minted under `serving_tenant`
    /// (`None` = the allocating process's own tenant). Providers that cannot
    /// distinguish a serving tenant (heap/runtime tables) delegate to
    /// [`allocate`](Self::allocate).
    fn allocate_for_tenant(
        &self,
        pages: u32,
        prot: RegionProt,
        purpose: ResourceKind,
        _serving_tenant: Option<&str>,
    ) -> Result<Region> {
        self.allocate(pages, prot, purpose)
    }

    /// Attaches an existing shared region.
    fn attach(&self, region_id: u64, reader_slot: Option<u32>, prot: RegionProt) -> Result<Region>;

    /// Frees a previously allocated region.
    fn free(&self, region_id: u64) -> Result<()>;

    /// Returns the number of live regions, when the provider can count them.
    ///
    /// Used by lifecycle/soak tests; production providers may return `None`.
    fn live_region_count(&self) -> Option<usize> {
        None
    }
}

/// Error type for selium-memory operations.
#[derive(Debug, Clone, Error, PartialEq)]
pub enum MemoryError {
    #[error("capacity exceeded")]
    CapacityExceeded,
    #[error("index out of bounds")]
    IndexOutOfBounds,
    #[error("invalid layout")]
    InvalidLayout,
    #[error("header checksum mismatch")]
    CorruptedHeader,
    #[error("region provider is not configured")]
    ProviderNotSet,
    #[error("region not found: {0}")]
    RegionNotFound(u64),
    #[error("ABI error: {0:?}")]
    Abi(selium_abi::AbiError),
    #[error("region operation failed: {0}")]
    Other(String),
}

/// An allocated or attached shared memory region handle.
#[derive(Clone)]
pub struct Region {
    allocation: RegionAllocation,
    backend: Arc<dyn MappingBackend>,
}

/// Alignment guaranteed for owned heap-backed mappings.
///
/// Backing storage must be aligned for the widest atomic the mapping exposes
/// (`AtomicU64`), so that a mapping base plus any 8-byte-aligned offset is a
/// valid atomic address. Using a plain `Vec<u8>` would only guarantee
/// byte alignment and make safe atomic APIs depend on undocumented allocator
/// behaviour.
pub const BACKING_ALIGNMENT: usize = std::mem::align_of::<AtomicU64>();

/// Owned, atomically-aligned backing storage for heap-backed mappings.
///
/// The buffer is zero-initialised, exposes raw byte access for the mapping
/// backend, and keeps the allocation alive for the lifetime of the value.
pub struct AlignedBuffer {
    ptr: NonNull<u8>,
    size: usize,
}

// SAFETY: `AlignedBuffer` owns its allocation exclusively; raw pointer access
// through `MappingBackend` performs its own bounds and alignment checks.
unsafe impl Send for AlignedBuffer {}
// SAFETY: see `Send`. Concurrent atomic access uses the atomics themselves;
// non-atomic access follows the caller's synchronisation contract.
unsafe impl Sync for AlignedBuffer {}

impl AlignedBuffer {
    /// Allocates zeroed storage of `size` bytes with [`BACKING_ALIGNMENT`].
    pub fn zeroed(size: u64) -> Result<Self> {
        let size = usize::try_from(size).map_err(|_error| MemoryError::CapacityExceeded)?;
        let layout = Layout::from_size_align(size.max(1), BACKING_ALIGNMENT)
            .map_err(|_error| MemoryError::InvalidLayout)?;
        // SAFETY: `layout` has a non-zero size.
        let ptr = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(ptr).ok_or_else(|| {
            MemoryError::Other("failed to allocate aligned region backing".to_string())
        })?;
        Ok(Self { ptr, size })
    }

    /// Returns the logical length of the buffer in bytes.
    pub fn len(&self) -> usize {
        self.size
    }

    /// Returns whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// Returns a raw pointer to the start of the allocation.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }
}

impl Debug for AlignedBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlignedBuffer")
            .field("ptr", &self.ptr)
            .field("size", &self.size)
            .finish()
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        // SAFETY: the layout is identical to the one used in `zeroed`.
        let layout =
            unsafe { Layout::from_size_align_unchecked(self.size.max(1), BACKING_ALIGNMENT) };
        // SAFETY: `ptr` was allocated with `layout` and is deallocated once.
        unsafe { dealloc(self.ptr.as_ptr(), layout) };
    }
}

/// Default, heap-backed region provider for native tests and in-process use.
///
/// Regions are backed by [`AlignedBuffer`] and registered in a process-local
/// map so that [`RegionProvider::attach`] can share the same memory.
#[derive(Default, Debug)]
pub struct HeapRegionProvider {
    counter: AtomicU64,
    registry: std::sync::Mutex<HashMap<u64, Arc<AlignedBuffer>>>,
}

/// A raw-pointer backend used by WASM guests and native tests.
///
/// The pointer must remain valid for the lifetime of the backend. For heap
/// allocations, `backing` keeps the allocation alive. For WASM linear memory,
/// the host guarantees validity.
#[derive(Debug)]
pub struct PointerBackend {
    base: *mut u8,
    size: u64,
    /// Heap-allocated backing store (native mode only). `None` for WASM
    /// mappings whose lifetime is managed by the runtime.
    backing: Option<Arc<AlignedBuffer>>,
}

/// A direct-memory mapping of a shared region.
#[derive(Clone)]
pub struct RegionMapping {
    inner: Arc<dyn MappingBackend>,
}

impl From<std::io::Error> for MemoryError {
    fn from(err: std::io::Error) -> Self {
        Self::Other(err.to_string())
    }
}

impl Region {
    /// Creates a `Region` from a raw allocation descriptor.
    ///
    /// In WASM mode `backing` should be `None`; the mapping will be derived
    /// from [`RegionAllocation::page_offset`]. This constructor is a
    /// convenience wrapper around [`Self::with_backend`] that uses a raw
    /// pointer backend.
    pub fn new(
        allocation: RegionAllocation,
        size: u64,
        backing: Option<Arc<AlignedBuffer>>,
    ) -> Self {
        let backend: Arc<dyn MappingBackend> = match backing {
            Some(backing) => Arc::new(PointerBackend::from_backing(backing, size)),
            None => {
                let base = (allocation.page_offset as u64 * WASM_PAGE_SIZE) as *mut u8;
                // SAFETY: The caller (region provider) guarantees that the
                // region is mapped at the returned page offset for the lifetime
                // of the region.
                Arc::new(unsafe { PointerBackend::from_raw(base, size) })
            }
        };
        Self::with_backend(allocation, backend)
    }

    /// Creates a `Region` with an arbitrary [`MappingBackend`].
    ///
    /// This is used by the runtime to install a kernel-delegated backend while
    /// still exposing the same [`RegionMapping`] API to higher-level crates.
    pub fn with_backend(allocation: RegionAllocation, backend: Arc<dyn MappingBackend>) -> Self {
        Self {
            allocation,
            backend,
        }
    }

    /// Returns the region allocation descriptor.
    pub const fn allocation(&self) -> RegionAllocation {
        self.allocation
    }

    /// Returns the region id.
    pub fn region_id(&self) -> u64 {
        self.allocation.region_id
    }

    /// Returns the page offset within guest linear memory.
    pub fn page_offset(&self) -> u32 {
        self.allocation.page_offset
    }

    /// Returns the region size in bytes.
    pub fn size(&self) -> u64 {
        self.backend.size()
    }

    /// Creates a [`RegionMapping`] for this region.
    pub fn mapping(&self) -> RegionMapping {
        RegionMapping::new(self.backend.clone())
    }
}

impl HeapRegionProvider {
    /// Creates a new heap-backed provider.
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(1),
            registry: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl RegionProvider for HeapRegionProvider {
    fn allocate(&self, _pages: u32, _prot: RegionProt, _purpose: ResourceKind) -> Result<Region> {
        let size = u64::from(_pages) * WASM_PAGE_SIZE;
        let backing = Arc::new(AlignedBuffer::zeroed(size)?);
        let region_id = self.counter.fetch_add(1, Ordering::SeqCst);
        self.registry
            .lock()
            .map_err(|_error| MemoryError::Other("registry poisoned".to_string()))?
            .insert(region_id, backing.clone());
        Ok(Region::new(
            RegionAllocation {
                region_id,
                page_offset: 0,
            },
            size,
            Some(backing),
        ))
    }

    fn attach(
        &self,
        region_id: u64,
        _reader_slot: Option<u32>,
        _prot: RegionProt,
    ) -> Result<Region> {
        let backing = self
            .registry
            .lock()
            .map_err(|_error| MemoryError::Other("registry poisoned".to_string()))?
            .get(&region_id)
            .cloned()
            .ok_or(MemoryError::RegionNotFound(region_id))?;
        let size = backing.len() as u64;
        Ok(Region::new(
            RegionAllocation {
                region_id,
                page_offset: 0,
            },
            size,
            Some(backing),
        ))
    }

    fn free(&self, region_id: u64) -> Result<()> {
        self.registry
            .lock()
            .map_err(|_error| MemoryError::Other("registry poisoned".to_string()))?
            .remove(&region_id);
        Ok(())
    }

    fn live_region_count(&self) -> Option<usize> {
        self.registry.lock().ok().map(|registry| registry.len())
    }
}

impl PointerBackend {
    /// Creates a backend backed by an aligned heap allocation.
    pub fn allocate(size: u64) -> Result<Self> {
        let backing = Arc::new(AlignedBuffer::zeroed(size)?);
        Ok(Self::from_backing(backing, size))
    }

    /// Creates a backend that shares an existing aligned heap allocation.
    pub fn from_backing(backing: Arc<AlignedBuffer>, size: u64) -> Self {
        let base = backing.as_ptr();
        Self {
            base,
            size,
            backing: Some(backing),
        }
    }

    /// Creates a backend from a raw pointer.
    ///
    /// # Safety
    ///
    /// The pointer must be valid for reads and writes of `size` bytes and must
    /// remain valid for the lifetime of the returned backend.
    pub unsafe fn from_raw(base: *mut u8, size: u64) -> Self {
        Self {
            base,
            size,
            backing: None,
        }
    }

    /// Bounds-check `offset` + `len` and return a pointer to the start.
    fn checked_offset(&self, offset: u64, len: u64) -> Result<*mut u8> {
        let end = offset
            .checked_add(len)
            .ok_or(MemoryError::CapacityExceeded)?;
        if end > self.size {
            return Err(MemoryError::IndexOutOfBounds);
        }
        // SAFETY: bounds checked; offset is within the mapping.
        Ok(unsafe { self.base.add(offset as usize) })
    }

    /// Bounds-check and alignment-check a 64-bit atomic address.
    ///
    /// The effective address (`base + offset`) must satisfy `AtomicU64`
    /// alignment. This is the contract of the safe atomic APIs; a mapping whose
    /// base is unaligned (raw constructor) still rejects misaligned effective
    /// addresses rather than invoking undefined behaviour.
    fn checked_atomic_u64(&self, offset: u64) -> Result<*const AtomicU64> {
        let ptr = self.checked_offset(offset, 8)?;
        if !(ptr as usize).is_multiple_of(std::mem::align_of::<AtomicU64>()) {
            return Err(MemoryError::InvalidLayout);
        }
        Ok(ptr as *const AtomicU64)
    }

    /// Bounds-check and alignment-check a 32-bit atomic address.
    fn checked_atomic_u32(&self, offset: u64) -> Result<*const AtomicU32> {
        let ptr = self.checked_offset(offset, 4)?;
        if !(ptr as usize).is_multiple_of(std::mem::align_of::<AtomicU32>()) {
            return Err(MemoryError::InvalidLayout);
        }
        Ok(ptr as *const AtomicU32)
    }
}

impl MappingBackend for PointerBackend {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        let end = offset
            .checked_add(len)
            .ok_or(MemoryError::CapacityExceeded)?;
        if end > self.size {
            return Err(MemoryError::IndexOutOfBounds);
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: bounds checked above; offset is within the mapping.
        let src = unsafe { self.base.add(offset as usize) };
        // SAFETY: `src` is valid for `len` bytes and `buf` is a fresh
        // allocation of the same length; the ranges do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), len as usize) };
        Ok(buf)
    }

    fn write(&self, offset: u64, bytes: &[u8]) -> Result<()> {
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or(MemoryError::CapacityExceeded)?;
        if end > self.size {
            return Err(MemoryError::IndexOutOfBounds);
        }
        // SAFETY: bounds checked above; offset is within the mapping.
        let dst = unsafe { self.base.add(offset as usize) };
        // SAFETY: `dst` is valid for `bytes.len()` bytes and `bytes` is a
        // disjoint slice; the ranges do not overlap.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len()) };
        Ok(())
    }

    fn atomic_load_u64(&self, offset: u64, ordering: Ordering) -> Result<u64> {
        let ptr = self.checked_atomic_u64(offset)?;
        // SAFETY: bounds and alignment checked above; `ptr` points to a valid
        // `AtomicU64` within the mapping.
        Ok(unsafe { (*ptr).load(ordering) })
    }

    fn atomic_store_u64(&self, offset: u64, value: u64, ordering: Ordering) -> Result<()> {
        let ptr = self.checked_atomic_u64(offset)?;
        // SAFETY: bounds and alignment checked above; `ptr` points to a valid
        // `AtomicU64` within the mapping.
        unsafe {
            (*ptr).store(value, ordering);
        }
        Ok(())
    }

    fn fetch_add_u64(&self, offset: u64, value: u64, ordering: Ordering) -> Result<u64> {
        let ptr = self.checked_atomic_u64(offset)?;
        // SAFETY: bounds and alignment checked above; `ptr` points to a valid
        // `AtomicU64` within the mapping.
        Ok(unsafe { (*ptr).fetch_add(value, ordering) })
    }

    fn compare_exchange_u64(&self, offset: u64, current: u64, new: u64) -> Result<u64> {
        let ptr = self.checked_atomic_u64(offset)?;
        // SAFETY: bounds and alignment checked above; `ptr` points to a valid
        // `AtomicU64` within the mapping.
        let result = unsafe {
            (*ptr).compare_exchange_weak(current, new, Ordering::SeqCst, Ordering::SeqCst)
        };
        match result {
            Ok(prev) => Ok(prev),
            Err(prev) => Ok(prev),
        }
    }

    fn atomic_notify(&self, offset: u64, count: u32) -> Result<u32> {
        let ptr = self.checked_atomic_u32(offset)?;
        let addr = ptr as usize;

        // Nightly-only: genuine `memory.atomic.notify` WASM intrinsic. Requires
        // a nightly toolchain with `#![feature(stdarch_wasm_atomic_wait)]` and
        // a `+atomics`/shared-everything-threads target. See `Cargo.toml`
        // feature `nightly-wasm-atomics`.
        #[cfg(all(target_arch = "wasm32", feature = "nightly-wasm-atomics"))]
        {
            let notified =
                unsafe { core::arch::wasm32::memory_atomic_notify(addr as *mut i32, count) };
            Ok(notified)
        }

        // Stable WASM: the guest is a single-threaded reactor; nothing parks
        // via `memory.atomic.wait32` in the guest's linear memory. The
        // load-bearing wake path is `wake_generation_waiters` (called by
        // `bump_generation`) plus the host mailbox. Returning an honest
        // `Ok(0)` ("0 waiters notified") is truthful — not a fake success
        // — and satisfies the No-Spin-Stubs requirement of
        // channel-wake-wait/spec.md.
        #[cfg(all(target_arch = "wasm32", not(feature = "nightly-wasm-atomics")))]
        {
            let _ = (addr, count);
            Ok(0)
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            Ok(waiters::notify(addr, count))
        }
    }

    fn atomic_wait32(&self, offset: u64, expected: u32, timeout_ms: u64) -> Result<()> {
        let ptr = self.checked_atomic_u32(offset)?;
        let addr = ptr as usize;

        // Nightly-only: genuine `memory.atomic.wait32` WASM intrinsic. Requires
        // a nightly toolchain with `#![feature(stdarch_wasm_atomic_wait)]` and
        // a `+atomics`/shared-everything-threads target. See `Cargo.toml`
        // feature `nightly-wasm-atomics`.
        #[cfg(all(target_arch = "wasm32", feature = "nightly-wasm-atomics"))]
        {
            let timeout_ns: i64 = if timeout_ms == u64::MAX {
                -1
            } else {
                (timeout_ms as i64).saturating_mul(1_000_000)
            };

            let result = unsafe {
                core::arch::wasm32::memory_atomic_wait32(
                    addr as *mut i32,
                    expected as i32,
                    timeout_ns,
                )
            };
            match result {
                0 => Ok(()),
                1 => Ok(()),
                2 => Err(MemoryError::Other("wait32 timed out".to_string())),
                _ => Err(MemoryError::Other("wait32 unknown result".to_string())),
            }
        }

        // Stable WASM: the guest never calls `atomic_wait32` — it parks via
        // `register_generation_wait` on the reactor, which is woken through
        // `wake_generation_waiters` and the host mailbox. Return an explicit
        // unsupported error rather than a fake success, satisfying the
        // No-Spin-Stubs requirement of channel-wake-wait/spec.md.
        #[cfg(all(target_arch = "wasm32", not(feature = "nightly-wasm-atomics")))]
        {
            let _ = (addr, expected, timeout_ms);
            Err(MemoryError::Other(
                "atomic_wait32 is not supported on stable wasm; enable the \
                 `nightly-wasm-atomics` feature"
                    .to_string(),
            ))
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            // SAFETY: `ptr` was computed from a bounds- and alignment-checked
            // atomic address and points to a valid `AtomicU32`.
            let actual = unsafe { (*ptr).load(Ordering::SeqCst) };
            if actual != expected {
                return Ok(());
            }
            waiters::wait(addr, timeout_ms)
        }
    }

    fn sub_region(&self, offset: u64, size: u64) -> Result<Arc<dyn MappingBackend>> {
        if offset
            .checked_add(size)
            .ok_or(MemoryError::CapacityExceeded)?
            > self.size
        {
            return Err(MemoryError::IndexOutOfBounds);
        }
        // SAFETY: bounds checked above.
        let base = unsafe { self.base.add(offset as usize) };
        let backing = self.backing.clone();
        Ok(Arc::new(PointerBackend {
            base,
            size,
            backing,
        }))
    }

    fn as_debug(&self) -> &dyn Debug {
        self
    }
}

// SAFETY: The fd and ptr are process-wide resources. The fd is a plain integer
// and the ptr points to a shared mapping that the kernel serialises. All
// mutation of attachment_count is atomic.
unsafe impl Send for PointerBackend {}

// SAFETY: `PointerBackend` accesses shared memory through a raw pointer, and
// all mutation of the underlying data is serialised by the kernel or by atomic
// operations. Immutable references to `PointerBackend` can safely share access
// to the mapped memory because all read/write paths either use atomic operations
// or go through the `MappingBackend` trait methods which perform internal
// synchronisation.
unsafe impl Sync for PointerBackend {}

impl RegionMapping {
    /// Creates a mapping wrapping the given backend.
    pub fn new(backend: Arc<dyn MappingBackend>) -> Self {
        Self { inner: backend }
    }

    /// Returns a reference to the underlying backend.
    pub fn backend(&self) -> &dyn MappingBackend {
        self.inner.as_ref()
    }

    /// Creates a mapping backed by a heap allocation (for native testing).
    pub fn allocate(size: u64) -> Result<Self> {
        Ok(Self::new(Arc::new(PointerBackend::allocate(size)?)))
    }

    /// Creates a mapping that shares an existing backing store.
    pub fn from_shared_backing(backing: Arc<AlignedBuffer>, size: u64) -> Self {
        Self::new(Arc::new(PointerBackend::from_backing(backing, size)))
    }

    /// Creates a mapping from a raw pointer (for WASM linear memory).
    ///
    /// # Safety
    ///
    /// The pointer must be valid for reads and writes of `size` bytes and must
    /// remain valid for the lifetime of the returned mapping.
    pub unsafe fn from_raw(base: *mut u8, size: u64) -> Self {
        // SAFETY: delegated to the caller of this unsafe function.
        Self::new(Arc::new(unsafe { PointerBackend::from_raw(base, size) }))
    }

    /// Returns the mapping size in bytes.
    pub fn size(&self) -> u64 {
        self.inner.size()
    }

    /// Creates a sub-mapping that points to a sub-region of this mapping.
    pub fn sub_region(&self, offset: u64, size: u64) -> Result<Self> {
        Ok(Self::new(self.inner.sub_region(offset, size)?))
    }

    /// Reads bytes at the given offset.
    pub fn read(&self, offset: u64, len: u64) -> Result<Vec<u8>> {
        self.inner.read(offset, len)
    }

    /// Writes bytes at the given offset.
    pub fn write(&self, offset: u64, bytes: &[u8]) -> Result<()> {
        self.inner.write(offset, bytes)
    }

    /// Reads a little-endian `u64` at the given offset.
    pub fn read_u64(&self, offset: u64) -> Result<u64> {
        let bytes = self.read(offset, 8)?;
        Ok(u64::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_error| MemoryError::InvalidLayout)?,
        ))
    }

    /// Writes a little-endian `u64` at the given offset.
    pub fn write_u64(&self, offset: u64, value: u64) -> Result<()> {
        self.write(offset, &value.to_le_bytes())
    }

    /// Reads a single byte at the given offset.
    pub fn read_u8(&self, offset: u64) -> Result<u8> {
        let bytes = self.read(offset, 1)?;
        bytes.first().copied().ok_or(MemoryError::InvalidLayout)
    }

    /// Writes a single byte at the given offset.
    pub fn write_u8(&self, offset: u64, value: u8) -> Result<()> {
        self.write(offset, &[value])
    }

    /// Atomically loads a `u64` at the given offset.
    pub fn atomic_load_u64(&self, offset: u64, ordering: Ordering) -> Result<u64> {
        self.inner.atomic_load_u64(offset, ordering)
    }

    /// Atomically stores a `u64` at the given offset.
    pub fn atomic_store_u64(&self, offset: u64, value: u64, ordering: Ordering) -> Result<()> {
        self.inner.atomic_store_u64(offset, value, ordering)
    }

    /// Atomically adds to a `u64` at the given offset, returning the previous value.
    pub fn fetch_add_u64(&self, offset: u64, value: u64, ordering: Ordering) -> Result<u64> {
        self.inner.fetch_add_u64(offset, value, ordering)
    }

    /// Atomically compares and exchanges a `u64` at the given offset.
    /// Returns the previous value (equals `current` on success).
    pub fn compare_exchange_u64(&self, offset: u64, current: u64, new: u64) -> Result<u64> {
        self.inner.compare_exchange_u64(offset, current, new)
    }

    /// Notifies waiters on an address within this mapping.
    pub fn atomic_notify(&self, offset: u64, count: u32) -> Result<u32> {
        self.inner.atomic_notify(offset, count)
    }

    /// Waits on an address within this mapping.
    pub fn atomic_wait32(&self, offset: u64, expected: u32, timeout_ms: u64) -> Result<()> {
        self.inner.atomic_wait32(offset, expected, timeout_ms)
    }
}

impl std::fmt::Debug for RegionMapping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegionMapping")
            .field("size", &self.inner.size())
            .field("backend", &self.inner.as_debug())
            .finish()
    }
}

/// Notify up to `count` waiters at a host-side wait key.
///
/// On native targets this wakes threads parked via [`host_wait`]. On WASM
/// targets this is a no-op — guests use `core::arch::wasm32` instructions.
#[cfg(not(target_arch = "wasm32"))]
pub fn host_notify(key: usize, count: u32) -> u32 {
    waiters::notify(key, count)
}

/// Park the current thread at a host-side wait key.
///
/// On native targets this blocks until [`host_notify`] is called or
/// `timeout_ms` elapses. On WASM targets this is unavailable — guests use
/// `core::arch::wasm32` instructions.
///
/// The caller MUST re-check the waited-on value after this returns, as
/// spurious wakes may occur.
#[cfg(not(target_arch = "wasm32"))]
pub fn host_wait(key: usize, timeout_ms: u64) -> Result<()> {
    waiters::wait(key, timeout_ms)
}

/// Install the generation-wait callbacks. Called once during reactor
/// initialisation.
#[expect(
    dropping_copy_types,
    reason = "Result<(), fn> is Copy; ignoring error is intentional"
)]
pub fn install_generation_callbacks(register: GenerationRegisterFn, wake: GenerationWakeFn) {
    drop(ON_GENERATION_REGISTER.set(register));
    drop(ON_GENERATION_WAKE.set(wake));
}

/// Returns the currently installed global region provider.
///
/// Returns an error if no provider has been installed.
pub fn region_provider() -> Result<&'static dyn RegionProvider> {
    GLOBAL_REGION_PROVIDER
        .get()
        .map(|p| p.as_ref())
        .ok_or(MemoryError::ProviderNotSet)
}

/// Register interest in a generation bump on `region_id`.
/// The caller's task will be woken when the generation exceeds
/// `observed_generation`. Returns `true` if a callback was installed
/// (and the waker was registered), or `false` if no callback is
/// installed — in which case the caller MUST self-wake as a fallback.
pub fn register_generation_wait(
    region_id: u64,
    observed_generation: u64,
    waker: &std::task::Waker,
) -> bool {
    if let Some(cb) = ON_GENERATION_REGISTER.get() {
        cb(region_id, observed_generation, waker);
        true
    } else {
        false
    }
}

/// Installs a global region provider.
///
/// This should be called once per process, typically during guest or runtime
/// initialisation. Subsequent calls return an error.
pub fn set_region_provider(provider: Box<dyn RegionProvider>) -> Result<()> {
    GLOBAL_REGION_PROVIDER
        .set(provider)
        .map_err(|_error| MemoryError::Other("region provider already installed".to_string()))
}

/// Wake all tasks waiting on `region_id` for a generation ≤ `new_generation`.
/// No-op if no callback is installed.
pub fn wake_generation_waiters(region_id: u64, new_generation: u64) {
    if let Some(cb) = ON_GENERATION_WAKE.get() {
        cb(region_id, new_generation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // scenario: region-provider/regionprovider-trait/provider-allocates-a-region
    // scenario: region-provider/regionprovider-trait/provider-frees-a-region
    #[test]
    fn heap_provider_allocate_and_free() {
        set_provider_for_test();
        let region = region_provider()
            .unwrap()
            .allocate(2, RegionProt::ReadWrite, ResourceKind::SharedMemory)
            .expect("allocate");
        assert_ne!(region.region_id(), 0);
        assert_eq!(region.size(), 2 * WASM_PAGE_SIZE);
        let region_id = region.region_id();
        region_provider().unwrap().free(region_id).expect("free");
        let result = region_provider()
            .unwrap()
            .attach(region_id, None, RegionProt::ReadWrite);
        assert!(result.is_err());
    }

    // scenario: region-provider/regionprovider-trait/provider-attaches-to-existing-region
    // scenario: region-provider/heapregionprovider-for-testing/two-heapregions-share-memory
    #[test]
    fn heap_provider_attach_shares_memory() {
        set_provider_for_test();
        let provider = region_provider().unwrap();
        let original = provider
            .allocate(2, RegionProt::ReadWrite, ResourceKind::SharedMemory)
            .expect("allocate");
        let region_id = original.region_id();

        let mapping = original.mapping();
        mapping.write(0, b"shared!").expect("write");

        let attached = provider
            .attach(region_id, None, RegionProt::ReadWrite)
            .expect("attach");
        let attached_mapping = attached.mapping();
        let data = attached_mapping.read(0, 7).expect("read");
        assert_eq!(data, b"shared!");

        attached_mapping
            .write(8, b"hello")
            .expect("write via attach");
        let data = mapping.read(8, 5).expect("read original");
        assert_eq!(data, b"hello");

        provider.free(region_id).expect("free");
    }

    // scenario: guest-shared-memory/shared-region-attach/guest-attaches-to-a-non-existent-region
    /// Installing a second provider must fail loudly rather than silently
    /// keep or replace the wrong context: a guest that cannot install its
    /// hostcall-backed provider would otherwise allocate through another
    /// runtime's kernel.
    // scenario: region-provider/global-regionprovider-installation/conflicting-guest-provider-installation
    #[test]
    fn a_second_provider_installation_fails_loudly() {
        // Whichever provider is already installed (test-injected or the first
        // call here) stays authoritative; the second installation is refused.
        drop(set_region_provider(Box::new(HeapRegionProvider::new())));
        let error = set_region_provider(Box::new(HeapRegionProvider::new()))
            .expect_err("a conflicting installation must be refused");
        assert!(
            error.to_string().contains("already installed"),
            "the refusal names the conflict: {error}"
        );
        assert!(
            region_provider().is_ok(),
            "the originally installed provider remains in use"
        );
    }

    #[test]
    fn heap_provider_attach_unknown_fails() {
        set_provider_for_test();
        let result = region_provider()
            .unwrap()
            .attach(999_999_999, None, RegionProt::ReadWrite);
        assert!(result.is_err());
    }

    // scenario: region-provider/regionmapping-remains-transport-agnostic/regionmapping-used-without-a-provider
    #[test]
    fn region_mapping_read_write_round_trip() {
        let mapping = RegionMapping::allocate(256).expect("allocate");
        mapping.write(0, &[1, 2, 3, 4]).expect("write");
        let data = mapping.read(0, 4).expect("read");
        assert_eq!(data, vec![1, 2, 3, 4]);
    }

    // scenario: guest-shared-memory/native-atomic-access-to-shared-regions/guest-reads-shared-data-via-regionmapping-read-u64
    #[test]
    fn region_mapping_u64_round_trip() {
        let mapping = RegionMapping::allocate(64).expect("allocate");
        mapping.write_u64(0, 0xDEAD_BEEF_CAFE_BABE).expect("write");
        assert_eq!(mapping.read_u64(0).expect("read"), 0xDEAD_BEEF_CAFE_BABE);
    }

    #[test]
    fn region_mapping_atomic_operations() {
        let mapping = RegionMapping::allocate(64).expect("allocate");
        mapping
            .atomic_store_u64(0, 10, Ordering::Release)
            .expect("store");
        assert_eq!(
            mapping.atomic_load_u64(0, Ordering::Acquire).expect("load"),
            10
        );
        let prev = mapping
            .fetch_add_u64(0, 5, Ordering::SeqCst)
            .expect("fetch_add");
        assert_eq!(prev, 10);
        assert_eq!(
            mapping.atomic_load_u64(0, Ordering::Acquire).expect("load"),
            15
        );
    }

    // scenario: guest-shared-memory/safe-atomic-access-requires-valid-alignment/misaligned-public-atomic-call
    #[test]
    fn misaligned_atomic_load_is_rejected() {
        let mapping = RegionMapping::allocate(64).expect("allocate");
        mapping.write_u64(0, 0xAABB_CCDD_EEFF_0011).expect("write");
        assert_eq!(
            mapping.atomic_load_u64(4, Ordering::Acquire),
            Err(MemoryError::InvalidLayout)
        );
        // The rejected access must not have consumed or changed the bytes.
        assert_eq!(mapping.read_u64(0).expect("read"), 0xAABB_CCDD_EEFF_0011);
    }

    #[test]
    fn misaligned_atomic_store_is_rejected() {
        let mapping = RegionMapping::allocate(64).expect("allocate");
        assert_eq!(
            mapping.atomic_store_u64(4, 1, Ordering::Release),
            Err(MemoryError::InvalidLayout)
        );
        assert_eq!(mapping.read_u64(0).expect("read"), 0);
    }

    #[test]
    fn misaligned_fetch_add_and_compare_exchange_are_rejected() {
        let mapping = RegionMapping::allocate(64).expect("allocate");
        assert_eq!(
            mapping.fetch_add_u64(4, 1, Ordering::SeqCst),
            Err(MemoryError::InvalidLayout)
        );
        assert_eq!(
            mapping.compare_exchange_u64(4, 0, 1),
            Err(MemoryError::InvalidLayout)
        );
        assert_eq!(mapping.read_u64(0).expect("read"), 0);
    }

    #[test]
    fn misaligned_wait_and_notify_are_rejected() {
        let mapping = RegionMapping::allocate(64).expect("allocate");
        assert_eq!(mapping.atomic_notify(2, 1), Err(MemoryError::InvalidLayout));
        assert_eq!(
            mapping.atomic_wait32(2, 0, 0),
            Err(MemoryError::InvalidLayout)
        );
    }

    #[test]
    fn misaligned_sub_region_atomics_are_rejected() {
        let mapping = RegionMapping::allocate(128).expect("allocate");
        let sub = mapping.sub_region(4, 64).expect("sub region");
        assert_eq!(
            sub.atomic_load_u64(0, Ordering::Acquire),
            Err(MemoryError::InvalidLayout)
        );
        assert_eq!(
            sub.atomic_load_u64(4, Ordering::Acquire),
            Ok(0),
            "an aligned effective address within an offset sub-region must work"
        );
    }

    // scenario: guest-shared-memory/safe-atomic-access-requires-valid-alignment/heap-backed-atomic-operations
    #[test]
    fn heap_backing_supports_aligned_atomics_at_every_aligned_offset() {
        let mapping = RegionMapping::allocate(64).expect("allocate");
        for offset in (0..64).step_by(8) {
            mapping
                .atomic_store_u64(offset, offset + 1, Ordering::SeqCst)
                .expect("aligned atomic store");
            assert_eq!(
                mapping
                    .atomic_load_u64(offset, Ordering::SeqCst)
                    .expect("aligned atomic load"),
                offset + 1
            );
        }
        for offset in 1..8 {
            assert_eq!(
                mapping.atomic_load_u64(offset, Ordering::SeqCst),
                Err(MemoryError::InvalidLayout),
                "offset {offset} must be rejected"
            );
        }
    }

    #[test]
    fn heap_provider_backing_is_aligned_for_atomics() {
        set_provider_for_test();
        let provider = region_provider().expect("provider");
        let region = provider
            .allocate(1, RegionProt::ReadWrite, ResourceKind::SharedMemory)
            .expect("allocate");
        let mapping = region.mapping();
        mapping
            .atomic_store_u64(0, 7, Ordering::SeqCst)
            .expect("atomic store on provider backing");
        let attached = provider
            .attach(region.region_id(), None, RegionProt::ReadWrite)
            .expect("attach");
        assert_eq!(
            attached
                .mapping()
                .atomic_load_u64(0, Ordering::SeqCst)
                .expect("atomic load through attach"),
            7
        );
        provider.free(region.region_id()).expect("free");
    }

    fn set_provider_for_test() {
        // Ignore errors from repeated test installs.
        drop(set_region_provider(Box::new(HeapRegionProvider::new())));
    }
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `region-provider` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `shared-region-builder` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.
