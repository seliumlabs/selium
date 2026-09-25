//! Stage 2 — per-OS wait-word primitives for host-side waits.
//!
//! Where a platform wait-word primitive is wired and the engine emits its
//! matching platform wake (see wasmtiny's `os_wake` module, gated by its
//! `platform-wake-emission` feature), the host backend parks on the OS
//! primitive at the region's host mapping address instead of the portable
//! per-region condvar registry (Stage 1).
//!
//! Availability mirrors the engine's support set exactly:
//!
//! | Platform | Wait | Wake |
//! | --- | --- | --- |
//! | Linux | `futex(FUTEX_WAIT)` | `futex(FUTEX_WAKE)` |
//! | Windows | `WaitOnAddress` | `WakeByAddressAll` |
//! | FreeBSD | `_umtx_op(UMTX_OP_WAIT_UINT_PRIVATE)` | `_umtx_op(UMTX_OP_WAKE_PRIVATE)` |
//!
//! macOS is deliberately **not** enabled. Empirically (macOS 26.x),
//! Darwin rejects the wait-word syscalls outright for ordinary binaries:
//! `__ulock_wait` returns junk errors (`-EFAULT` on static words,
//! `-EOWNERDEAD` on heap words) even on private memory, and never parks —
//! the kernel only honours the private `os_sync_wait_on_address` /
//! `__ulock_*` family for entitled callers. The kernel also rejects the
//! restricted futex syscall. The host therefore can never park on the
//! word the engine would wake. The `__ulock_*` backend below (constants
//! audited against XNU's `bsd/sys/ulock.h`) is retained for the day that
//! changes; [`available`] returns `false` there.
//!
//! Enabling Stage 2 is a conformance-gated build-time decision on the engine
//! side; on the selium side the host opts in only when the engine reports
//! `HostWaitSupport::RegistryAndOsWake` (see `KernelBackend::stage2_active`),
//! which is the honest "conformance passed here" signal. Any platform without
//! it — including macOS, always — uses Stage 1 with identical semantics.

/// True when a wait-word primitive is compiled in for this target (and thus
/// could be activated). A function so call sites read as "is Stage 2
/// available here", mirroring the engine's `os_wake::active`.
pub fn available() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "windows",
        target_os = "freebsd"
    ))
}

/// Parks the current thread until the little-endian `u32` word at `ptr`
/// differs from `expected` (woken) or `timeout_ms` elapses. `u64::MAX` blocks
/// indefinitely.
///
/// Returns `true` when woken or racing (the word already changed, or a
/// spurious wake — the caller MUST re-check the word), `false` on timeout.
///
/// # Safety
///
/// `ptr` must point to a mapped, writable, 4-byte-aligned word that remains
/// valid for the duration of the call.
pub unsafe fn wait(ptr: *mut u8, expected: u32, timeout_ms: u64) -> bool {
    // SAFETY: delegated to [`wait`]'s caller; `wait_impl` follows the same
    // contract.
    unsafe { wait_impl(ptr, expected, timeout_ms) }
}

/// Wakes up to `count` threads parked on the little-endian `u32` word at
/// `ptr`, returning the number of wake attempts delivered. Mirrors the
/// engine's platform-wake emission so the two sides agree on the wake
/// primitive.
///
/// # Safety
///
/// `ptr` must point to a mapped, readable, 4-byte-aligned word that remains
/// valid for the duration of the call.
pub unsafe fn wake(ptr: *mut u8, count: u32) -> u32 {
    // SAFETY: delegated to [`wake`]'s caller; `wake_impl` follows the same
    // contract.
    unsafe { wake_impl(ptr, count) }
}

#[cfg(target_os = "linux")]
unsafe fn wait_impl(ptr: *mut u8, expected: u32, timeout_ms: u64) -> bool {
    let ts: Option<libc::timespec> = if timeout_ms == u64::MAX {
        None
    } else {
        let duration = core::time::Duration::from_millis(timeout_ms);
        Some(libc::timespec {
            tv_sec: duration.as_secs() as libc::time_t,
            tv_nsec: duration.subsec_nanos() as libc::c_long,
        })
    };
    let ts_ptr = match ts.as_ref() {
        Some(t) => t as *const libc::timespec,
        None => std::ptr::null(),
    };

    // SAFETY: contract of [`wait`]; ptr is a live mapped 4-byte word.
    let r = unsafe {
        libc::syscall(
            libc::SYS_futex,
            ptr.cast::<libc::c_void>(),
            libc::FUTEX_WAIT,
            expected as libc::c_int,
            ts_ptr,
            std::ptr::null::<libc::c_void>(),
            0,
        )
    };
    // `libc::syscall` is the libc wrapper, not the raw instruction: it
    // reports failure as -1 with the error in `errno` (both glibc and
    // musl), never as the raw `-errno` value the kernel uses internally.
    // A successful FUTEX_WAIT returns exactly 0, so -1 is unambiguous.
    if r == 0 {
        return true; // woken by FUTEX_WAKE
    }
    if r == -1 {
        // EAGAIN: value already differed (race); EINTR: spurious wake. Both
        // mean "re-check the word" rather than "timed out".
        return matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EAGAIN) | Some(libc::EINTR)
        );
    }
    false // Unexpected non-libc return: treat as timed out
}

#[cfg(target_os = "windows")]
unsafe fn wait_impl(ptr: *mut u8, expected: u32, timeout_ms: u64) -> bool {
    unsafe extern "system" {
        #[link_name = "WaitOnAddress"]
        fn wait_on_address_(
            address: *mut libc::c_void,
            compare: *mut libc::c_void,
            size: libc::c_ulong,
            timeout_ms: u32,
        ) -> i32;
    }

    const INFINITE: u32 = u32::MAX;
    const ERROR_TIMEOUT: i32 = 0x5B4; // 1460

    let expected_word = expected.to_le_bytes();
    let effective_timeout = if timeout_ms == u64::MAX {
        INFINITE
    } else {
        timeout_ms.clamp(0, INFINITE as u64) as u32
    };

    // SAFETY: `WaitOnAddress` atomically compares the 4-byte word at `ptr`
    // against `expected_word` and parks. `expected_word` lives across the call.
    let ok = unsafe {
        wait_on_address_(
            ptr.cast::<libc::c_void>(),
            expected_word.as_ptr() as *mut libc::c_void,
            4,
            effective_timeout,
        )
    };
    // TRUE: woken by WakeByAddress*. FALSE with ERROR_TIMEOUT: timed out.
    // FALSE with any other error: spurious — re-check the word.
    ok != 0 || std::io::Error::last_os_error().raw_os_error() != Some(ERROR_TIMEOUT)
}

#[cfg(target_os = "freebsd")]
unsafe fn wait_impl(ptr: *mut u8, expected: u32, timeout_ms: u64) -> bool {
    unsafe extern "C" {
        fn _umtx_op(
            obj: *mut libc::c_void,
            op: libc::c_int,
            val: libc::c_ulong,
            uaddr: *mut libc::c_void,
            uaddr2: *mut libc::c_void,
        ) -> libc::c_int;
    }

    // UMTX_OP_WAIT_UINT_PRIVATE atomically compares the 32-bit word at `obj`
    // against `val` and parks, with an absolute timeout in `uaddr2`.
    const UMTX_OP_WAIT_UINT_PRIVATE: libc::c_int = 15;

    let ts: Option<libc::timespec> = if timeout_ms == u64::MAX {
        None
    } else {
        // `_umtx_op` takes an *absolute* CLOCK_MONOTONIC deadline derived from
        // the caller's relative request. Gated by the FreeBSD conformance test
        // before Stage 2 enables here (see CI).
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: valid out pointer; CLOCK_MONOTONIC is a valid clock id.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } != 0 {
            return false; // clock failure: treat as timed out
        }
        let deadline_ns = (now.tv_sec as i128)
            .saturating_mul(1_000_000_000)
            .saturating_add(now.tv_nsec as i128)
            .saturating_add((timeout_ms as i128).saturating_mul(1_000_000));
        Some(libc::timespec {
            tv_sec: (deadline_ns / 1_000_000_000) as libc::time_t,
            tv_nsec: (deadline_ns % 1_000_000_000) as libc::c_long,
        })
    };
    let ts_ptr = match ts.as_ref() {
        Some(t) => t as *const libc::timespec,
        None => std::ptr::null(),
    };

    // SAFETY: contract of [`wait`].
    let r = unsafe {
        _umtx_op(
            ptr.cast::<libc::c_void>(),
            UMTX_OP_WAIT_UINT_PRIVATE,
            expected as libc::c_ulong,
            std::ptr::null_mut(),
            ts_ptr as *mut libc::c_void,
        )
    };
    match r {
        0 => true,                                          // woken by UMTX_OP_WAKE_PRIVATE
        e if e == libc::ETIMEDOUT => false,                 // timed out
        e if e == libc::EINTR || e == libc::EAGAIN => true, // spurious / race
        _ => false,                                         // unexpected error: treat as timed out
    }
}

#[cfg(target_os = "macos")]
unsafe fn wait_impl(ptr: *mut u8, expected: u32, timeout_ms: u64) -> bool {
    // Retained against the day Darwin allows wait-word primitives.
    // Never reached: [`available`] is false here and the engine does not
    // emit `__ulock_wake` either, so the two sides can never be paired.
    //
    // Constants audited against XNU's bsd/sys/ulock.h: the operation word
    // is `opcode | flags` — `UL_COMPARE_AND_WAIT == 0x2` in the low byte,
    // `ULF_NO_ERRNO == 0x0100_0000`. With `ULF_NO_ERRNO` failures return
    // `-errno`: 0 = woken, `-EAGAIN` = value mismatch (race — caller
    // re-checks), `-EINTR` = spurious, `-ETIMEDOUT` = timeout.
    unsafe extern "C" {
        fn __ulock_wait(
            operation: u32,
            addr: *mut libc::c_void,
            value: u64,
            timeout_us: u32,
        ) -> i32;
    }
    const UL_COMPARE_AND_WAIT: u32 = 0x0000_0002;
    const ULF_NO_ERRNO: u32 = 0x0100_0000;
    let timeout_us = if timeout_ms == u64::MAX {
        u32::MAX
    } else {
        timeout_ms.saturating_mul(1_000).min(u32::MAX as u64) as u32
    };
    // SAFETY: contract of [`wait`].
    let r = unsafe {
        __ulock_wait(
            ULF_NO_ERRNO | UL_COMPARE_AND_WAIT,
            ptr.cast::<libc::c_void>(),
            expected as u64,
            timeout_us,
        )
    };
    match r {
        0 => true,
        e if e == -libc::EAGAIN || e == -libc::EINTR => true,
        _ => false,
    }
}

// Platforms with no wired primitive: `available()` is never true there, so
// these bodies are unreachable by construction (retained for completeness so
// the crate still type-checks on exotic targets).
#[cfg(not(any(
    target_os = "linux",
    target_os = "windows",
    target_os = "freebsd",
    target_os = "macos"
)))]
unsafe fn wait_impl(_ptr: *mut u8, _expected: u32, _timeout_ms: u64) -> bool {
    false
}

#[cfg(target_os = "linux")]
unsafe fn wake_impl(ptr: *mut u8, count: u32) -> u32 {
    // SAFETY: contract of [`wake`]. Keyed by inode+offset for shared
    // mappings, so it reaches waiters on other mappings of the same shm pages
    // (matching the engine's emission).
    let r = unsafe {
        libc::syscall(
            libc::SYS_futex,
            ptr.cast::<libc::c_void>(),
            libc::FUTEX_WAKE,
            count as libc::c_int,
            std::ptr::null::<libc::timespec>(),
            std::ptr::null::<libc::c_void>(),
            0,
        )
    };
    // Errors report as -1 (libc wrapper convention, see wait_impl) and fall
    // through to 0 wake attempts delivered.
    u32::try_from(r).unwrap_or(0)
}

#[cfg(target_os = "windows")]
unsafe fn wake_impl(ptr: *mut u8, _count: u32) -> u32 {
    unsafe extern "system" {
        #[link_name = "WakeByAddressAll"]
        fn wake_by_address_all_(address: *mut libc::c_void);
    }
    // SAFETY: contract of [`wake`].
    unsafe {
        wake_by_address_all_(ptr.cast::<libc::c_void>());
    }
    // `WakeByAddressAll` returns nothing; report one wake attempt delivered.
    1
}

#[cfg(target_os = "freebsd")]
unsafe fn wake_impl(ptr: *mut u8, count: u32) -> u32 {
    unsafe extern "C" {
        fn _umtx_op(
            obj: *mut libc::c_void,
            op: libc::c_int,
            val: libc::c_ulong,
            uaddr: *mut libc::c_void,
            uaddr2: *mut libc::c_void,
        ) -> libc::c_int;
    }

    // UMTX_OP_WAKE_PRIVATE wakes private-address waiters.
    const UMTX_OP_WAKE_PRIVATE: libc::c_int = 16;

    // SAFETY: contract of [`wake`].
    let r = unsafe {
        _umtx_op(
            ptr.cast::<libc::c_void>(),
            UMTX_OP_WAKE_PRIVATE,
            count as libc::c_ulong,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    u32::try_from(r).unwrap_or(0)
}

#[cfg(target_os = "macos")]
unsafe fn wake_impl(ptr: *mut u8, _count: u32) -> u32 {
    // Retained for the day Darwin allows wait-words on shared memory;
    // never reached because [`available`] is false there. Wakes use
    // `UL_COMPARE_AND_WAIT | ULF_WAKE_ALL`: the wake opcode must match the
    // waiters' opcode (XNU returns EDOM otherwise), and WAKE_ALL matches
    // the futex FUTEX_WAKE semantics the host backend expects.
    unsafe extern "C" {
        fn __ulock_wake(operation: u32, addr: *mut libc::c_void, wake_value: u64) -> i32;
    }
    const UL_COMPARE_AND_WAIT: u32 = 0x0000_0002;
    const ULF_WAKE_ALL: u32 = 0x0000_0100;
    const ULF_NO_ERRNO: u32 = 0x0100_0000;
    // SAFETY: contract of [`wake`].
    unsafe {
        let _ = __ulock_wake(
            ULF_NO_ERRNO | ULF_WAKE_ALL | UL_COMPARE_AND_WAIT,
            ptr.cast::<libc::c_void>(),
            0,
        );
    }
    1
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "windows",
    target_os = "freebsd",
    target_os = "macos"
)))]
unsafe fn wake_impl(_ptr: *mut u8, _count: u32) -> u32 {
    0
}

#[cfg(all(
    test,
    // Only platforms where the primitive demonstrably functions. macOS is
    // excluded: Darwin rejects `__ulock_wait` with junk errnos even on
    // private memory (see module docs), so a roundtrip there can only
    // assert the rejection — which `available()` already encodes.
    any(target_os = "linux", target_os = "windows", target_os = "freebsd")
))]
mod tests {
    use super::*;

    /// Mechanics check for each wired primitive, on **private** memory
    /// (every wired primitive works there). This validates wait/wake
    /// plumbing, timeouts, and the wake-race return conventions; the
    /// MAP_SHARED conformance — the actual Stage 2 gate — is
    /// `stage2_notify_wait_race_conformance` in `tests/wait_notify.rs`,
    /// which only runs where Stage 2 is active.
    #[test]
    fn wait_word_roundtrip_on_private_memory() {
        let word = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let waiter = {
            let waiter_word = std::sync::Arc::clone(&word);
            std::thread::spawn(move || {
                // SAFETY: `waiter_word` keeps the u32 alive and 4-byte
                // aligned for the duration of the call.
                unsafe { wait(waiter_word.as_ref() as *const _ as *mut u8, 0, 2_000) }
            })
        };

        // Park first: without a wake the wait below would (incorrectly)
        // time out and the test would fail. Small delay so the waiter is
        // very likely parked, though the word-change race makes it
        // unnecessary for correctness.
        std::thread::sleep(std::time::Duration::from_millis(50));
        word.store(1, std::sync::atomic::Ordering::SeqCst);
        // SAFETY: `word` keeps the u32 alive and aligned for the duration
        // of the call.
        let woken = unsafe { wake(word.as_ref() as *const _ as *mut u8, 1) };

        // A wake was attempted; either the waiter was parked (delivered) or
        // it lost the race and observed the word change (still "woken").
        assert!(
            waiter.join().expect("waiter thread"),
            "waiter must report woken (wake delivered or word changed)"
        );
        let _ = woken;
    }

    /// The timeout convention: an unwoken wait returns `false` (treated as
    /// a timed-out wait by the caller, which re-checks the word).
    #[test]
    fn wait_word_times_out_without_wake() {
        let word: u32 = 0;
        // SAFETY: `word` is a live, 4-byte-aligned u32 for the duration of
        // the call.
        let woken = unsafe { wait(&word as *const _ as *mut u8, 0, 25) };
        assert!(!woken, "unwoken wait must report timeout");
    }

    /// The value-race convention: waiting on a word that already differs
    /// from `expected` returns `true` (caller re-checks) rather than
    /// parking until timeout.
    #[test]
    fn wait_word_returns_immediately_on_value_mismatch() {
        let word: u32 = 7;
        let start = std::time::Instant::now();
        // SAFETY: `word` is a live, 4-byte-aligned u32 for the duration of
        // the call.
        let woken = unsafe { wait(&word as *const _ as *mut u8, 0, 2_000) };
        assert!(woken, "mismatched word must report woken, not timeout");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "value mismatch must not park until the timeout"
        );
    }
}
