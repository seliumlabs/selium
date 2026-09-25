use crate::async_runtime::wake_task;

static mut MAILBOX: [u32; MAILBOX_WORDS] = [0; MAILBOX_WORDS];
const MAILBOX_WORDS: usize = selium_abi::mailbox::BYTE_LEN / 4;
/// Set once the mailbox area has been zeroed for this process (see
/// [`register_mailbox`]). The zeroing is a one-time initialisation: a repeat
/// of it on every reactor entry would wipe host-enqueued wakes (the ring,
/// flag, head, and tail) that arrived between polls.
static MAILBOX_ZEROED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Liveness bound for a worker parked on the shared wake word, in
/// nanoseconds (250 ms): a worker that misses a notify — e.g. a stop or
/// exit signal delivered while it was between its exit-check and its waiter
/// registration — re-checks and re-parks at most this often, so no signal
/// can be slept through indefinitely.
const PARK_TIMEOUT_NANOS: u64 = 250_000_000;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "selium")]
unsafe extern "C" {
    #[link_name = "process_id"]
    fn selium_process_id() -> u64;
    #[link_name = "mark_ready"]
    fn selium_mark_ready();
    #[link_name = "hostcall_create"]
    pub(crate) fn selium_hostcall_create(request_ptr: *const u8, request_len: usize) -> u64;
    #[link_name = "hostcall_poll"]
    pub(crate) fn selium_hostcall_poll(
        operation_id: u64,
        out_ptr: *mut u8,
        out_capacity: usize,
    ) -> u64;
    #[link_name = "hostcall_drop"]
    pub(crate) fn selium_hostcall_drop(operation_id: u64) -> u32;
    #[link_name = "mailbox_register"]
    fn selium_mailbox_register(mailbox_ptr: *mut u8, mailbox_len: usize);
}

/// Marks the current guest as ready for runtime readiness checks.
pub fn mark_ready() {
    // SAFETY: `selium_mark_ready` is a host import with no safety invariants.
    unsafe { selium_mark_ready() }
}

/// Returns the current guest process id assigned by the host.
pub fn process_id() -> u64 {
    // SAFETY: `selium_process_id` is a host import with no safety invariants.
    unsafe { selium_process_id() }
}

/// Bumps the host-visible parking word of `task_id` — the mirror of the
/// task's internal wake counter in the shared ABI layout. Returns the
/// previous value, or `None` when the task id is beyond the parking-word
/// table (those tasks fall back to the ring mailbox wake path).
pub(crate) fn bump_task_parking_word(task_id: selium_abi::TaskId) -> Option<u32> {
    let offset = selium_abi::mailbox::parking_word_offset(task_id)?;
    // SAFETY: `offset` is a bounds-checked mailbox constant within the
    // parking-word table, so `mailbox_cell` returns a valid cell.
    let cell = unsafe { mailbox_cell(offset) };
    // SAFETY: `cell` points to a valid AtomicU32 within the mailbox.
    Some(unsafe { (*cell).fetch_add(1, std::sync::atomic::Ordering::Release) })
}

/// Bumps the shared wake word, signalling parked workers to re-check the run
/// queue. Returns the previous value.
pub(crate) fn bump_wake_word() -> u32 {
    // SAFETY: the wake-word offset is a valid mailbox constant, so
    // `mailbox_cell` returns a valid cell.
    let cell = unsafe { mailbox_cell(selium_abi::mailbox::WAKE_WORD_OFFSET) };
    // SAFETY: `cell` points to a valid AtomicU32 within the mailbox.
    unsafe { (*cell).fetch_add(1, std::sync::atomic::Ordering::Release) }
}

pub(crate) fn drain_mailbox() {
    // SAFETY: The mailbox is a static mutable array. Ring access is
    // serialised against the host by the head/tail handshake (the host is the
    // single producer) and against the guest's workers by the executor's
    // `mailbox_lock` (single consumer: at most one worker drains at a time).
    //
    // Correctness rests on the ring *indices*, never on the advisory flag: a
    // host enqueue that lands after this consumer's `tail` read advances
    // `tail` past `head`, but its flag set can be lost to the consumer's flag
    // clear. Gating the drain (or the pending check) on the flag would strand
    // that entry until the next unrelated wake; `head != tail` cannot.
    let head_ptr = unsafe { mailbox_cell(selium_abi::mailbox::HEAD_OFFSET) };
    // SAFETY: `head_ptr` points to a valid AtomicU32 within the mailbox.
    let mut head = unsafe { (*head_ptr).load(std::sync::atomic::Ordering::Acquire) };

    // SAFETY: Same mailbox serialisation as above.
    let tail_ptr = unsafe { mailbox_cell(selium_abi::mailbox::TAIL_OFFSET) };
    // SAFETY: Same mailbox serialisation as above.
    let tail = unsafe { (*tail_ptr).load(std::sync::atomic::Ordering::Acquire) };

    if head == tail {
        return;
    }

    while head != tail {
        let slot = selium_abi::mailbox::RING_OFFSET
            + (head as usize % selium_abi::mailbox::CAPACITY) * selium_abi::mailbox::SLOT_SIZE;
        // SAFETY: `slot` has been bounds-checked against the mailbox capacity.
        let slot_ptr = unsafe { mailbox_cell(slot) };
        // SAFETY: `slot_ptr` points to a valid AtomicU32 within the mailbox.
        let task_id = unsafe { (*slot_ptr).load(std::sync::atomic::Ordering::Relaxed) };
        wake_task(task_id);
        head = head.wrapping_add(1);
    }
    // SAFETY: Same mailbox serialisation as above.
    let head_ptr = unsafe { mailbox_cell(selium_abi::mailbox::HEAD_OFFSET) };
    // SAFETY: Same mailbox serialisation as above.
    unsafe { (*head_ptr).store(head, std::sync::atomic::Ordering::Release) };

    // Advisory only: the producer sets the flag on enqueue; clearing it here
    // keeps it a rough "ring drained" hint. Nothing gates on it (see above).
    // SAFETY: Same mailbox serialisation as above.
    let flag_ptr = unsafe { mailbox_cell(selium_abi::mailbox::FLAG_OFFSET) };
    // SAFETY: Same mailbox serialisation as above.
    unsafe { (*flag_ptr).store(0, std::sync::atomic::Ordering::Release) };
}

/// Returns whether the host has queued mailbox wakes (the ring holds unread
/// entries: `head != tail`).
///
/// Used as a futex-style re-check before a worker parks: a wake that arrived
/// after the last drain must keep the worker running rather than park. Derived
/// from the ring indices, not the advisory flag, so a host enqueue whose flag
/// set raced a drain's flag clear can never be missed (see [`drain_mailbox`]).
pub(crate) fn mailbox_has_pending() -> bool {
    // SAFETY: the head offset is a valid mailbox constant, so `mailbox_cell`
    // returns a valid cell.
    let head_cell = unsafe { mailbox_cell(selium_abi::mailbox::HEAD_OFFSET) };
    // SAFETY: the tail offset is a valid mailbox constant, so `mailbox_cell`
    // returns a valid cell.
    let tail_cell = unsafe { mailbox_cell(selium_abi::mailbox::TAIL_OFFSET) };
    // SAFETY: `head_cell` points to a valid AtomicU32 within the mailbox.
    let head = unsafe { (*head_cell).load(std::sync::atomic::Ordering::Acquire) };
    // SAFETY: `tail_cell` points to a valid AtomicU32 within the mailbox.
    let tail = unsafe { (*tail_cell).load(std::sync::atomic::Ordering::Acquire) };
    head != tail
}

/// Notifies one worker parked on the shared wake word.
///
/// Native: wakes a thread parked via [`park_on_wake_word`]. WASM with the
/// `nightly-wasm-atomics` feature: emits a genuine `memory.atomic.notify`.
/// Stable WASM (cooperative mode): no worker ever parks, so the notify is a
/// no-op — the word bump is the guest-observable wake signal.
pub(crate) fn notify_wake_word() {
    #[cfg(all(target_arch = "wasm32", feature = "nightly-wasm-atomics"))]
    {
        let addr = wake_word_address() as *mut i32;
        // SAFETY: the wake word is a valid i32 slot in the guest's linear
        // memory; the intrinsic only wakes waiters, it does not write.
        unsafe {
            core::arch::wasm32::memory_atomic_notify(addr, 1);
        }
    }
    #[cfg(all(target_arch = "wasm32", not(feature = "nightly-wasm-atomics")))]
    {
        // Cooperative mode: no worker parks, so there is nothing to notify.
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        selium_memory::host_notify(wake_word_address(), 1);
    }
}

/// Parks the current worker on the shared wake word until a notify fires, the
/// word's value diverges from `expected`, or a liveness timeout elapses.
///
/// The caller MUST have re-checked work availability and observed `expected`
/// immediately before parking (futex discipline); spurious wakes are safe —
/// the worker loop re-checks the run queue and re-parks.
///
/// The park is bounded by [`PARK_TIMEOUT_NANOS`] rather than being
/// indefinite: a notify delivered while every worker is between its
/// exit-check and its waiter registration cannot reach a waiter (a notify
/// without a waiter is lost, per wasm threads semantics), so the timeout
/// turns any missed notify into a delayed re-check instead of a worker
/// sleeping through a process exit forever.
pub(crate) fn park_on_wake_word(expected: u32) {
    #[cfg(all(target_arch = "wasm32", feature = "nightly-wasm-atomics"))]
    {
        let addr = wake_word_address() as *mut i32;
        // SAFETY: the wake word is a valid i32 slot in the guest's linear
        // memory; wait32 blocks the current wasm thread without writing.
        unsafe {
            core::arch::wasm32::memory_atomic_wait32(
                addr,
                expected as i32,
                PARK_TIMEOUT_NANOS as i64,
            );
        }
    }
    #[cfg(all(target_arch = "wasm32", not(feature = "nightly-wasm-atomics")))]
    {
        // Only reachable if multithreaded mode is forced without the atomics
        // target; the runtime gates MT mode on atomics (module_probe), so this
        // is a defensive fallback. Bounded spin for the same liveness reason.
        let deadline = PARK_TIMEOUT_NANOS;
        let mut waited = 0u64;
        while wake_word_value() == expected && waited < deadline {
            core::hint::spin_loop();
            waited += 1;
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        // The native waiter registry handles the wake race itself; the
        // expected value is only meaningful to the wasm wait instruction.
        // `host_wait` takes milliseconds. A returning wait is a notify, the
        // bounded-park timeout, or a poisoned-lock failure; all are
        // best-effort here because the caller re-checks the wake word and run
        // queue after the park. Surface the error rather than swallow it.
        let _ = expected;
        if let Err(error) =
            selium_memory::host_wait(wake_word_address(), PARK_TIMEOUT_NANOS / 1_000_000)
        {
            crate::debug!(error = %error, "guest wake-word park ended without a notify");
        }
    }
}

pub(crate) fn register_mailbox() {
    // Zero the whole shared area (ring + wake word + parking-word table) once,
    // at first executor entry, so a fresh process — or a fresh native test —
    // starts from a clean slate. Wasm linear memory is zero-initialised, so
    // this only matters for native test binaries, but it is cheap and exact.
    //
    // The zeroing is guarded by the one-time `MAILBOX_ZEROED` swap, so
    // concurrent first entries cannot race it — exactly one caller zeroes and
    // the others observe the swap's happens-before edge.
    if !MAILBOX_ZEROED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        let base = mailbox_base().cast::<u32>();
        for index in 0..MAILBOX_WORDS {
            // SAFETY: `index < MAILBOX_WORDS`, so the offset stays within the
            // mailbox allocation.
            let slot = unsafe { base.add(index) };
            // SAFETY: `slot` is a valid, 4-byte-aligned `*mut u32` within the
            // mailbox; writing zero initialises the cell.
            unsafe { slot.write(0) };
        }
    }
    // SAFETY: The mailbox is a static mutable array; the capacity store and
    // the host registration below are mediated by the host import and are
    // idempotent, so repeat entries (cooperative polls re-register) are
    // safe.
    let capacity_ptr = unsafe { mailbox_cell(selium_abi::mailbox::CAPACITY_OFFSET) };
    // SAFETY: `capacity_ptr` points to a valid AtomicU32 within the mailbox.
    unsafe {
        (*capacity_ptr).store(
            selium_abi::mailbox::CAPACITY as u32,
            std::sync::atomic::Ordering::Release,
        );
    }
    // SAFETY: `selium_mailbox_register` is a host import that registers the
    // mailbox with the runtime. It is safe to call once during initialisation.
    unsafe {
        selium_mailbox_register(mailbox_base(), selium_abi::mailbox::BYTE_LEN);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) unsafe fn selium_hostcall_create(_: *const u8, _: usize) -> u64 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) unsafe fn selium_hostcall_drop(_: u64) -> u32 {
    0
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) unsafe fn selium_hostcall_poll(_: u64, _: *mut u8, _: usize) -> u64 {
    0
}

/// Returns whether the host has requested the guest process to stop (the ABI
/// stop word is set). Multithreaded workers check this when woken and return
/// from the worker entry, so the host can tear a process down without
/// driving the reactor.
pub(crate) fn stop_requested() -> bool {
    // SAFETY: the stop offset is a valid mailbox constant, so `mailbox_cell`
    // returns a valid cell.
    let cell = unsafe { mailbox_cell(selium_abi::mailbox::STOP_OFFSET) };
    // SAFETY: `cell` points to a valid AtomicU32 within the mailbox.
    unsafe { (*cell).load(std::sync::atomic::Ordering::Acquire) != 0 }
}

/// Address of the shared wake word: the native futex key and the wasm wait
/// address for parked workers.
///
/// Stable WASM (cooperative mode) never parks, so the address is only needed
/// on native and `nightly-wasm-atomics` builds.
#[cfg(any(not(target_arch = "wasm32"), feature = "nightly-wasm-atomics"))]
pub(crate) fn wake_word_address() -> usize {
    mailbox_base().wrapping_add(selium_abi::mailbox::WAKE_WORD_OFFSET) as usize
}

/// Returns the current value of the shared wake word (the word parked workers
/// wait on). The worker compares this against the value observed before
/// parking; a divergence means a wake arrived and the park must be skipped.
pub(crate) fn wake_word_value() -> u32 {
    // SAFETY: the wake-word offset is a valid mailbox constant, so
    // `mailbox_cell` returns a valid cell.
    let cell = unsafe { mailbox_cell(selium_abi::mailbox::WAKE_WORD_OFFSET) };
    // SAFETY: `cell` points to a valid AtomicU32 within the mailbox.
    unsafe { (*cell).load(std::sync::atomic::Ordering::Acquire) }
}

fn mailbox_base() -> *mut u8 {
    core::ptr::addr_of_mut!(MAILBOX).cast::<u8>()
}

unsafe fn mailbox_cell(offset: usize) -> *mut core::sync::atomic::AtomicU32 {
    // SAFETY: `offset` is a known mailbox constant within the MAILBOX bounds.
    unsafe {
        mailbox_base()
            .add(offset)
            .cast::<core::sync::atomic::AtomicU32>()
    }
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn selium_mailbox_register(_: *mut u8, _: usize) {}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn selium_mark_ready() {}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn selium_process_id() -> u64 {
    0
}
