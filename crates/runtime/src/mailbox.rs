use std::sync::atomic::{AtomicU32, Ordering};

use selium_abi::TaskId;
use wasmtiny::{Memory, WasmError, runtime::SharedMemory};

pub(crate) struct GuestMailbox {
    pub(crate) memory: SharedMemory,
    pub(crate) base: u32,
}

impl GuestMailbox {
    pub(crate) fn new(memory: SharedMemory, base: u32) -> Self {
        Self { memory, base }
    }

    pub(crate) fn enqueue(&self, task_id: TaskId) -> wasmtiny::runtime::Result<()> {
        let memory = self.lock()?;
        let head = load(self.cell(&memory, selium_abi::mailbox::HEAD_OFFSET)?);
        let tail = load(self.cell(&memory, selium_abi::mailbox::TAIL_OFFSET)?);
        // Full-check: the ring holds `CAPACITY` slots. In multithreaded mode
        // wakes can accumulate while every worker is busy, so a full ring must
        // not overwrite unread entries — that would silently lose the wakes of
        // the clobbered slots. Skip the ring write instead; the wake is still
        // delivered on the multithreaded path by the parking-word bump and the
        // wake-word notify (a cooperative guest drains inline, so its ring
        // never backs up).
        if tail.wrapping_sub(head) as usize >= selium_abi::mailbox::CAPACITY {
            return Ok(());
        }
        let slot = (tail as usize % selium_abi::mailbox::CAPACITY) * selium_abi::mailbox::SLOT_SIZE;
        let slot_offset = selium_abi::mailbox::RING_OFFSET
            .checked_add(slot)
            .ok_or_else(|| WasmError::Runtime("mailbox slot offset overflow".to_string()))?;
        store(self.cell(&memory, slot_offset)?, task_id);
        store(
            self.cell(&memory, selium_abi::mailbox::TAIL_OFFSET)?,
            tail.wrapping_add(1),
        );
        store(self.cell(&memory, selium_abi::mailbox::FLAG_OFFSET)?, 1);
        Ok(())
    }

    /// Bumps `task_id`'s host-visible parking word in the ABI mailbox, the
    /// per-task wake signal multithreaded guests observe (see the
    /// channel-wake-wait spec: the host notifies the parked task's parking
    /// word). Best-effort: task ids beyond the parking-word table fall back
    /// to the ring wake path alone.
    pub(crate) fn bump_task_parking_word(&self, task_id: TaskId) -> wasmtiny::runtime::Result<()> {
        let Some(offset) = selium_abi::mailbox::parking_word_offset(task_id) else {
            return Ok(());
        };
        let memory = self.lock()?;
        fetch_add(self.cell(&memory, offset)?);
        Ok(())
    }

    /// Bumps the guest's shared wake word, the word parked workers wait on.
    ///
    /// Used before a notify so a worker that has not yet registered its wait
    /// still observes the change on its `wait32` (the futex value-mismatch
    /// discipline): `memory.atomic.notify` wakes only already-parked waiters,
    /// so bumping the word is what makes a wake that races a worker's
    /// check-then-park impossible to lose.
    pub(crate) fn bump_wake_word(&self) -> wasmtiny::runtime::Result<()> {
        let memory = self.lock()?;
        fetch_add(self.cell(&memory, selium_abi::mailbox::WAKE_WORD_OFFSET)?);
        Ok(())
    }

    /// Notifies up to `count` workers parked on the guest's shared wake word, so
    /// they resume in place and re-check the run queue — the multithreaded
    /// wake path replaces "drive the reactor" with atomic notify delivery.
    /// A per-task wake notifies one worker; a process stop or trap notifies
    /// the whole pool.
    pub(crate) fn notify_wake_word(&self, count: u32) -> wasmtiny::runtime::Result<()> {
        let memory = self.lock()?;
        let wake_word = self.offset(selium_abi::mailbox::WAKE_WORD_OFFSET)?;
        drop(memory.notify(wake_word, count));
        Ok(())
    }

    /// Sets the ABI stop word: multithreaded workers check it when woken and
    /// return from the worker entry, letting the host tear a process down
    /// without driving the reactor.
    ///
    /// Also bumps the shared wake word. `memory.atomic.notify` wakes only
    /// waiters *already parked*, so a stop racing a worker that has not yet
    /// reached its wait would be lost forever if the word it waits on did not
    /// change. Bumping the wake word makes any subsequent
    /// `wait32(…, expected)` return immediately on the value mismatch (futex
    /// discipline), so the stop signal reaches the pool regardless of which
    /// side of the park the race lands on.
    pub(crate) fn set_stop(&self) -> wasmtiny::runtime::Result<()> {
        let memory = self.lock()?;
        store(self.cell(&memory, selium_abi::mailbox::STOP_OFFSET)?, 1);
        fetch_add(self.cell(&memory, selium_abi::mailbox::WAKE_WORD_OFFSET)?);
        Ok(())
    }

    fn lock(&self) -> wasmtiny::runtime::Result<std::sync::MutexGuard<'_, Memory>> {
        self.memory
            .lock()
            .map_err(|_lock_err| WasmError::Runtime("guest memory lock poisoned".to_string()))
    }

    /// Returns an atomic cell for the `u32` at `offset` within the mailbox.
    ///
    /// The guest accesses the mailbox with atomic operations and never takes
    /// the host's memory lock, so the host must access the same words
    /// atomically too: a plain load/store racing the guest's atomics is a data
    /// race, and a read-modify-write (read `u32`, add one, write `u32`) can
    /// lose an update. `Memory::as_ptr` is stable across `memory.grow`
    /// (growth mprotects the reservation; it never reallocates), so the
    /// pointer stays valid while the memory lock is held.
    fn cell(&self, memory: &Memory, offset: usize) -> wasmtiny::runtime::Result<*const AtomicU32> {
        let addr = self.offset(offset)? as usize;
        let end = addr
            .checked_add(std::mem::size_of::<u32>())
            .ok_or_else(|| WasmError::Runtime("mailbox offset overflow".to_string()))?;
        if end > memory.len_bytes() {
            return Err(WasmError::Runtime(
                "mailbox cell lies outside guest linear memory".to_string(),
            ));
        }
        // SAFETY: `addr..addr + 4` is within the owned linear memory, and the
        // mailbox layout keeps every u32 cell 4-byte aligned (the guest uses
        // `AtomicU32` on the same words).
        Ok(unsafe { memory.as_ptr().add(addr).cast::<AtomicU32>() })
    }

    fn offset(&self, offset: usize) -> wasmtiny::runtime::Result<u32> {
        self.base
            .checked_add(offset as u32)
            .ok_or_else(|| WasmError::Runtime("mailbox offset overflow".to_string()))
    }
}

/// Atomic fetch-add on a mailbox cell, returning the previous value. This is
/// the atomic read-modify-write the ABI documents for the wake/parking words
/// (the guest bumps the same words with a real `fetch_add`).
fn fetch_add(cell: *const AtomicU32) -> u32 {
    // SAFETY: `cell` points to a valid AtomicU32 within the guest's memory.
    unsafe { (*cell).fetch_add(1, Ordering::AcqRel) }
}

/// Atomic load of a mailbox cell (acquire), synchronising with the guest's
/// own atomic accesses.
fn load(cell: *const AtomicU32) -> u32 {
    // SAFETY: `cell` points to a valid AtomicU32 within the guest's memory.
    unsafe { (*cell).load(Ordering::Acquire) }
}

/// Atomic store to a mailbox cell (release).
fn store(cell: *const AtomicU32, value: u32) {
    // SAFETY: `cell` points to a valid AtomicU32 within the guest's memory.
    unsafe { (*cell).store(value, Ordering::Release) };
}
