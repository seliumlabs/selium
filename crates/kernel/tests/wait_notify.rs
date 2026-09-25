//! Conformance evidence for the shared-page wait/wake fast path.
//!
//! These tests gate the stages of the fast path: Stage 1 (the unified
//! per-region waiter registry) must deliver a notify wake on every host
//! platform, and Stage 2 (per-OS wait-words) may only activate where the
//! engine reports its platform wake emission — i.e. where the notify/wait
//! race has been shown to pass on that platform.

use std::time::Duration;

use selium_kernel::Kernel;
use wasmtiny::runtime::{HostWaitSupport, WakeOutcome};
// The Stage 2 conformance test uses the backend wait/notify path.
#[cfg(feature = "stage2-wait-words")]
use selium_memory::MappingBackend;

/// Non-gating latency reports (task 5.4): the three-way comparison of
/// wake latencies — kick path (portable baseline), Stage 1 (unified
/// registry), Stage 2 (OS wait-word). Run explicitly with
/// `cargo test -p selium-kernel --test wait_notify -- --ignored
/// --nocapture`. Reports per-wake figures only; there is deliberately no
/// pass threshold (non-gating).
///
/// All three use the same harness ([`parked_wake_latency`]): a waiter
/// thread parks, the waker (after the waiter signals it is about to
/// block) notifies, and the sample is the notify→resume time. The three
/// paths differ only in where the waiter parks and who delivers the
/// notify:
///
/// | Report | Waiter parks on | Wake delivered by |
/// | --- | --- | --- |
/// | kick path | `atomic_wait32` (Stage 1 registry) | `notify_region` — exactly what `kick_network_waiters` calls per region on a guest→host transition |
/// | Stage 1 | `RegionWaiter::wait` (registry) | `notify_region` |
/// | Stage 2 | `atomic_wait32` (OS wait-word) | `atomic_notify` (registry + platform wake) |
mod latency_reports {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };

    use selium_kernel::Kernel;
    use selium_memory::MappingBackend;
    use wasmtiny::runtime::WakeOutcome;

    const SAMPLES: u64 = 2_000;

    /// One notify→wake sample: `park` runs on the waiter thread (signalling
    /// `parked` first) and returns when woken; `notify` runs on the waker
    /// thread afterwards. Returns the notify→resume duration.
    ///
    /// The waker MUST change the wait word before notifying (the guest-write
    /// pattern): a notify that lands before the waiter registers is
    /// otherwise lost, and the waiter's re-check would park on an unchanged
    /// word until timeout. With the word change first, the register →
    /// re-check → wait idiom is airtight — the waiter either observes the
    /// new word at re-check or is woken while parked.
    fn parked_wake_latency(
        park: impl FnOnce() + Send + 'static,
        notify: impl FnOnce() + Send + 'static,
    ) -> Duration {
        let parked = Arc::new(AtomicBool::new(false));
        let notify_at = Arc::new(std::sync::Mutex::new(None::<Instant>));
        let woke_at = Arc::new(std::sync::Mutex::new(None::<Instant>));

        let waiter_parked = Arc::clone(&parked);
        let waiter_woke_at = Arc::clone(&woke_at);
        let waiter = std::thread::spawn(move || {
            waiter_parked.store(true, Ordering::Release);
            park();
            *waiter_woke_at.lock().expect("woke timestamp mutex") = Some(Instant::now());
        });

        // Let the waiter reach the park point; the notify then lands either
        // on the parked waiter or in the register→wait window, where the
        // word change (done by `notify`) makes the re-check catch it.
        while !parked.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        *notify_at.lock().expect("notify timestamp mutex") = Some(Instant::now());
        notify();

        waiter.join().expect("waiter thread");
        let notify_at = notify_at
            .lock()
            .expect("notify timestamp mutex")
            .expect("notify timestamp recorded");
        let woke_at = woke_at
            .lock()
            .expect("wake timestamp mutex")
            .expect("wake timestamp recorded");
        woke_at.saturating_duration_since(notify_at)
    }

    fn report(label: &str, total: Duration) {
        let per_wake_ns = total.as_nanos() / u128::from(SAMPLES);
        eprintln!("{label}: {per_wake_ns} ns/wake ({SAMPLES} samples)");
    }

    /// Kick-path baseline: the waiter parks exactly like a network
    /// drainer (`atomic_wait32` on the Stage 1 registry) and is woken by
    /// `notify_region` — the precise call `kick_network_waiters` makes per
    /// region on every guest→host transition for non-fast-path regions.
    #[test]
    #[ignore = "non-gating latency report; run explicitly"]
    fn kick_path_wake_latency_report() {
        let kernel = Kernel::default();
        let memory = kernel.memory();
        let (shared_id, _len) = memory.allocate_shared_region(64).expect("allocate region");
        let backend = memory.attach_backend(shared_id).expect("attach backend");
        let stage = if memory.stage2_active() {
            "stage2 OS wait-word"
        } else {
            "stage1 unified registry"
        };

        let mut total = Duration::ZERO;
        for i in 0..SAMPLES {
            // The waiter parks on generation `i`; the waker bumps the word
            // to `i + 1` then notifies — the guest-write pattern that makes
            // the register → re-check → wait idiom airtight.
            let expected = i as u32;
            let bumped = (i + 1) as u32;
            let waiter_backend = backend.clone();
            let writer_backend = backend.clone();
            let notify_memory = memory.clone();
            total += parked_wake_latency(
                move || {
                    waiter_backend
                        .atomic_wait32(0, expected, 1_000)
                        .expect("wait")
                },
                move || {
                    writer_backend
                        .write(0, &(bumped).to_le_bytes())
                        .expect("write word");
                    // Exactly what kick_network_waiters delivers per region.
                    notify_memory
                        .notify_region(shared_id, 0, 1)
                        .expect("notify");
                },
            );
        }
        report(&format!("kick-path ({stage}) wake"), total);
    }

    /// Stage 1: the waiter parks on a registered `RegionWaiter` (the
    /// unified registry) and is woken by `notify_region` — what a fast-path
    /// guest's `memory.atomic.notify` delivers.
    #[test]
    #[ignore = "non-gating latency report; run explicitly"]
    fn stage1_wake_latency_report() {
        let kernel = Kernel::default();
        let memory = kernel.memory();
        let (shared_id, _len) = memory.allocate_shared_region(64).expect("allocate region");

        let mut total = Duration::ZERO;
        for _ in 0..SAMPLES {
            // Registration happens here, strictly before the notify below,
            // so the waiter's latched `notified` flag makes a lost wake
            // impossible without a word bump.
            let waiter = memory
                .register_region_waiter(shared_id, 0)
                .expect("register waiter");
            let notify_memory = memory.clone();
            total += parked_wake_latency(
                move || match waiter.wait(Duration::from_secs(1)).expect("wait") {
                    WakeOutcome::Woken => {}
                    WakeOutcome::TimedOut => panic!("lost wake during sample"),
                },
                move || {
                    notify_memory
                        .notify_region(shared_id, 0, 1)
                        .expect("notify");
                },
            );
        }
        report("stage1 unified-registry wake", total);
    }

    /// Stage 2: the waiter parks on the OS wait-word (Stage 2 active) and
    /// is woken by `atomic_notify` (registry notify + platform wake). On
    /// platforms without Stage 2 this reports unavailability and exits —
    /// the portable paths remain the baseline.
    #[test]
    #[ignore = "non-gating latency report; run explicitly"]
    fn stage2_wake_latency_report() {
        let kernel = Kernel::default();
        let memory = kernel.memory();
        if !memory.stage2_active() {
            eprintln!("stage2: OS wait-words not enabled on this platform");
            return;
        }
        let (shared_id, _len) = memory.allocate_shared_region(64).expect("allocate region");
        let backend = memory.attach_backend(shared_id).expect("attach backend");

        let mut total = Duration::ZERO;
        for i in 0..SAMPLES {
            // Word bump before notify: the guest-write pattern (see
            // [`parked_wake_latency`]). The OS wait-word re-checks the
            // value atomically inside the primitive, so a notify landing
            // before the park resolves as an immediate value mismatch.
            let expected = i as u32;
            let bumped = (i + 1) as u32;
            let waiter_backend = backend.clone();
            let writer_backend = backend.clone();
            let notify_backend = backend.clone();
            total += parked_wake_latency(
                move || {
                    waiter_backend
                        .atomic_wait32(0, expected, 1_000)
                        .expect("wait")
                },
                move || {
                    writer_backend
                        .write(0, &(bumped).to_le_bytes())
                        .expect("write word");
                    notify_backend.atomic_notify(0, 1).expect("notify");
                },
            );
        }
        report("stage2 OS-wait-word wake", total);
    }
}

const ITERATIONS: usize = 5_000;

/// Stage 1 conformance: a host waiter registered on a shared region offset is
/// woken by a notify on that offset, reliably and across many iterations. The
/// register → notify → wait ordering is controlled so no wake can be lost;
/// a lost wake would surface as a `TimedOut` and fail the test.
#[test]
fn notify_wake_registry_conformance() {
    let kernel = Kernel::default();
    let memory = kernel.memory();
    let (shared_id, _len) = memory.allocate_shared_region(64).expect("allocate region");

    for _ in 0..ITERATIONS {
        // Register the waiter first; the notify below is strictly ordered
        // after registration, so the waiter must observe it.
        let waiter = memory
            .register_region_waiter(shared_id, 0)
            .expect("register waiter");

        let notify_memory = memory.clone();
        let notifier = std::thread::spawn(move || {
            notify_memory
                .notify_region(shared_id, 0, 1)
                .expect("notify region");
        });
        notifier.join().expect("notify thread");

        match waiter
            .wait(Duration::from_millis(100))
            .expect("waiter wait")
        {
            WakeOutcome::Woken => {}
            WakeOutcome::TimedOut => panic!("waiter not woken by notify"),
        }
    }
}

/// Stage 2 conformance: the notify/wait race on a real shared region, many
/// iterations, with jittered park/notify interleavings. The waiter parks in
/// the OS wait-word primitive (`atomic_wait32` with Stage 2 active) and the
/// waker bumps the word then notifies — exactly the guest-write pattern. A
/// lost wake surfaces as a `wait32 timed out` error and fails the test.
///
/// This is the gate for enabling `stage2-wait-words` on a platform (task
/// 4.2/4.3 of the shared-page-fastpath change): it is compiled only when the
/// feature is on, and skips (with a message) where Stage 2 is inactive, so
/// CI runs it as real evidence on Linux. Any failure means the platform
/// must stay on Stage 1 permanently.
#[test]
#[cfg(feature = "stage2-wait-words")]
fn stage2_notify_wait_race_conformance() {
    let kernel = Kernel::default();
    let memory = kernel.memory();
    if !memory.stage2_active() {
        eprintln!("stage2: OS wait-words not enabled on this platform; skipping");
        return;
    }

    let (shared_id, _len) = memory.allocate_shared_region(64).expect("allocate region");
    let backend = memory.attach_backend(shared_id).expect("attach backend");

    for i in 0..ITERATIONS {
        // The waiter parks on the *current* generation value; the waker
        // bumps it to i + 1 and notifies. Jitter the waker's start so the
        // notify sometimes lands before the park (value-race path) and
        // sometimes after (wake path) — both must resolve as a wake.
        let expected = 0_u32;
        let new_value = (i as u32 % 100) + 1;
        let park_delay_us = (i % 64) as u64 * 3;

        let waiter_backend = backend.clone();
        let waiter = std::thread::spawn(move || {
            waiter_backend
                .atomic_wait32(0, expected, 1_000)
                .expect("stage2 wait must not time out (lost wake?)")
        });

        std::thread::sleep(Duration::from_micros(park_delay_us));
        backend
            .write(0, &new_value.to_le_bytes())
            .expect("write word");
        backend.atomic_notify(0, 1).expect("notify");

        waiter.join().expect("waiter thread");
    }
}

/// Stage 2 opt-in gate: the host may wait on OS wait-words only when the
/// engine reports `RegistryAndOsWake` (its platform wake emission is compiled
/// in — the conformance-gated opt-in). Everywhere else — including platforms
/// that permanently fall back, like macOS — Stage 1 is used with identical
/// semantics.
#[test]
fn stage2_requires_engine_os_wake_support() {
    let kernel = Kernel::default();
    let memory = kernel.memory();

    if memory.host_wait_support() != HostWaitSupport::RegistryAndOsWake {
        assert!(
            !memory.stage2_active(),
            "Stage 2 must be inactive when the engine does not emit platform wakes"
        );
    }
}
