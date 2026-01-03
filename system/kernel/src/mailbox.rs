//! Guest mailbox integration: exposes host-side wakers to guest tasks.
//!
//! Safety: the mailbox views raw Wasm linear memory as a trio of shared
//! `AtomicU32` slots (`flag`, `tail`, `ring[..]`). The guest owns the memory
//! allocation but **must** treat the region as host-only: only the host may
//! mutate the indices, while guests read them using matching atomic orderings.
//! The memory outlives the mailbox because we leak the structure via
//! [`create_guest_mailbox`]; use one Wasmtime store per guest instance to avoid
//! aliasing. The offsets match the layout emitted by `selium-guest`:
//!
//! ```text
//! struct Mailbox {
//!     head: u32,
//!     flag: AtomicU32,
//!     capacity: u32,
//!     tail: AtomicU32,
//!     ring: [AtomicU32; CAP]
//! }
//! ```
//!
//! `enqueue` uses relaxed ordering for the per-slot write, release when
//! signalling writers, and AcqRel on the tail counter so concurrent wakers are
//! totally ordered.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use futures_util::task::{ArcWake, waker_ref};
use tokio::sync::Notify;
use wasmtime::{Memory, Store};

use selium_abi::{
    GuestAtomicUint, GuestUint,
    mailbox::{CAPACITY, FLAG_OFFSET, RING_OFFSET, TAIL_OFFSET},
};

/// Mailbox exposing guest task IDs to the host async scheduler.
///
/// The mailbox views guest linear memory as a ring-buffer of task identifiers plus a
/// futex-compatible flag. The host-side scheduler pushes ready tasks to the ring, whilst
/// guest-side polling logic reads from the shared ring.
pub struct GuestMailbox {
    base: AtomicUsize,
    closed: AtomicBool,
    notify: Notify,
}

unsafe impl Send for GuestMailbox {}

unsafe impl Sync for GuestMailbox {}

impl GuestMailbox {
    /// # Safety
    /// * `memory` / `store` must reference a mailbox layout produced by the guest helper.
    /// * The pointed-to memory must not be reclaimed while the mailbox lives.
    /// * Only host code may mutate the mailbox slots; guests may read them.
    unsafe fn new<T>(memory: &Memory, store: &mut Store<T>) -> Self {
        let base = memory.data_ptr(store) as usize;
        Self {
            base: AtomicUsize::new(base),
            closed: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }

    /// Refresh the cached guest memory base in case the instance grew its linear memory.
    pub(crate) fn refresh_base(&self, base: usize) {
        self.base.store(base, Ordering::Release);
    }

    /// Mark the mailbox as closed and wake any waiting host tasks.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.notify.notify_one();
    }

    /// Return whether the mailbox has been closed.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn ptrs(
        &self,
    ) -> (
        *const GuestAtomicUint,
        *const GuestAtomicUint,
        *const GuestAtomicUint,
    ) {
        let base = self.base.load(Ordering::Acquire);
        (
            (base + FLAG_OFFSET) as *const _,
            (base + TAIL_OFFSET) as *const _,
            (base + RING_OFFSET) as *const _,
        )
    }

    /// Push a task ID for the guest executor and wake any parked thread.
    fn enqueue(&self, task_id: usize) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        unsafe {
            let (flag, tail_ptr, ring) = self.ptrs();
            let tail = (*tail_ptr).fetch_add(1, Ordering::AcqRel);
            let slot = (tail % CAPACITY) as usize;
            let id = GuestUint::try_from(task_id).expect("task id exceeds guest width");
            (*ring.add(slot)).store(id, Ordering::Relaxed);
            (*flag).store(1, Ordering::Release);
            #[cfg(target_os = "linux")]
            {
                libc::syscall(
                    libc::SYS_futex,
                    flag as *const GuestAtomicUint as libc::c_long,
                    libc::FUTEX_WAKE as libc::c_long,
                    1 as libc::c_long,
                );
            }
        }
        self.notify.notify_one();
    }

    /// Returns whether the guest has pending wake-ups to drain.
    pub(crate) fn is_signalled(&self) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let (flag, _tail, _ring) = self.ptrs();
        unsafe { (*flag).load(Ordering::Acquire) != 0 }
    }

    /// Await the next mailbox wake-up notification from the host.
    pub(crate) async fn wait_for_signal(&self) {
        self.notify.notified().await;
    }

    /// Produce a [`std::task::Waker`] that enqueues the provided task id when triggered.
    pub(crate) fn waker(&'static self, task_id: usize) -> std::task::Waker {
        struct MbWaker {
            mb: &'static GuestMailbox,
            id: usize,
        }
        impl ArcWake for MbWaker {
            fn wake_by_ref(arc_self: &Arc<Self>) {
                arc_self.mb.enqueue(arc_self.id);
            }
        }
        let arc = Arc::new(MbWaker {
            mb: self,
            id: task_id,
        });
        waker_ref(&arc).clone()
    }
}

/// # Safety
/// Leaks a GuestMailbox to 'static; caller is responsible for process lifetime semantics.
pub unsafe fn create_guest_mailbox<T>(
    memory: &Memory,
    store: &mut Store<T>,
) -> &'static GuestMailbox {
    Box::leak(Box::new(unsafe { GuestMailbox::new(memory, store) }))
}

#[cfg(test)]
mod tests {
    use selium_abi::mailbox::SLOT_SIZE;
    use wasmtime::{Engine, MemoryType};

    use super::*;

    #[test]
    fn enqueue_writes_ring_and_sets_flag() {
        let engine = Engine::default();
        let mut store = Store::new(&engine, ());
        let memory = Memory::new(&mut store, MemoryType::new(1, None)).expect("memory");

        // Zero the backing memory region used by the mailbox.
        {
            let data = memory.data_mut(&mut store);
            for slot in data
                .iter_mut()
                .take(RING_OFFSET + (CAPACITY as usize * SLOT_SIZE))
            {
                *slot = 0;
            }
        }

        let mailbox = unsafe { GuestMailbox::new(&memory, &mut store) };
        mailbox.enqueue(7);

        let base = memory.data_ptr(&mut store) as usize;
        let tail_ptr = (base + TAIL_OFFSET) as *const GuestAtomicUint;
        let ring_ptr = (base + RING_OFFSET) as *const GuestAtomicUint;
        let flag_ptr = (base + FLAG_OFFSET) as *const GuestAtomicUint;

        let tail = unsafe { (*tail_ptr).load(Ordering::Relaxed) as usize };
        assert_eq!(tail, 1);
        let slot = unsafe { (*ring_ptr).load(Ordering::Relaxed) };
        assert_eq!(slot, 7);
        let flag = unsafe { (*flag_ptr).load(Ordering::Relaxed) };
        assert_eq!(flag, 1);
    }
}
