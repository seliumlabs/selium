//! Cross-guest wake delivery integration tests (`cross-guest-wake-delivery`).
//!
//! A guest task parked on a shared channel must be woken by a write from
//! *another* guest process. The wake travels host-side: the writer's
//! generation advance reaches the runtime, which delivers a mailbox wake to
//! every process that registered interest in the region via `WaitRegister`.
//!
//! The consumer here is never polled by the test. Draining the guest's log
//! channel reads shared memory directly and does not re-enter the guest's
//! WASM, so the only thing that can produce the `woke` log line is a wake
//! delivered by the runtime.
//!
//! `#[ignore]`d by default because they require the wake-probe guest built
//! for `wasm32-unknown-unknown`:
//!
//! ```sh
//! cargo build --target wasm32-unknown-unknown -p selium-wake-probe
//! cargo test -p selium-runtime --test cross_guest_wake -- --ignored
//! ```

use std::time::{Duration, Instant};

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_runtime::{Runtime, SystemGuestDescriptor};
use selium_service::FlatMsg;

mod common;

/// Upper bound on the instructions one poll pass may cost against a guest
/// with nothing to do. The interpreted reactor entry (mailbox drain, run-queue
/// pop, poll-owner check) costs a few thousand; the bound leaves generous
/// headroom while still failing loudly if the host starts re-entering the
/// guest on its own re-arm.
const MAX_INSTRUCTIONS_PER_IDLE_POLL: u64 = 10_000;
/// The parker role's integer entrypoint argument: owns a channel nobody
/// writes to, so an attached watcher parks for the whole test.
const MODE_PARKER: u64 = 2;
/// The publisher role's integer entrypoint argument.
const MODE_PUBLISHER: u64 = 0;
/// The watcher role's integer entrypoint argument.
const MODE_WATCHER: u64 = 1;
/// The yielder role's integer entrypoint argument: a task that only yields.
const MODE_YIELDER: u64 = 3;
/// How many yields the yielder reports per log line.
const YIELD_LOG_INTERVAL: u64 = 100;

/// Counts the publisher's log frames over `window`, proving it is still
/// resident and progressing.
fn count_publishes(runtime: &Runtime, process_id: u64, window: Duration) -> u64 {
    let deadline = Instant::now() + window;
    let mut frames = 0_u64;
    while Instant::now() < deadline {
        frames += drain_logs(runtime, process_id).len() as u64;
        std::thread::sleep(Duration::from_millis(50));
    }
    frames
}

/// Task 1.2: a `wasm32` watcher parked on another guest's channel converges on
/// a frame published by that guest, without the host polling it.
// scenario: channel-wake-wait/wakeable-channel-reads/reader-parks-and-wakes-on-publish
// scenario: channel-wake-wait/cross-thread-wake-delivery/no-embedder-cooperation-required
// scenario: selium-runtime/cross-process-channel-wake-delivery/cross-guest-write-wakes-a-parked-reader
// scenario: selium-guest/reactor-parking-and-wake-sources/channel-wait-resumes-on-generation-advance
#[test]
#[ignore = "requires the wake-probe guest built for wasm32-unknown-unknown"]
fn cross_guest_write_wakes_a_parked_watcher() {
    let runtime = Runtime::default();

    let publisher_id = spawn_probe(&runtime, MODE_PUBLISHER, "");

    let region_id = parse_channel_region(&runtime, publisher_id, Duration::from_secs(10));

    // The consumer's blocking reader starts at the live tail, so it parks on
    // an empty tail and its wake can only come from a publish that happens
    // *after* it attached — never from data already in the ring.
    let watcher_id = spawn_probe(&runtime, MODE_WATCHER, &region_id.to_string());

    // The watcher starts at the live tail and has no other wake source. Its
    // own `woke` record is therefore behavioural evidence of this publish;
    // the generous helper bound only distinguishes a missing wake.
    let _ = wait_for_log(
        &runtime,
        watcher_id,
        "wake-probe: woke",
        Duration::from_secs(10),
    );

    runtime.stop_process(watcher_id).expect("stop watcher");
    runtime.stop_process(publisher_id).expect("stop publisher");
}

/// Drains the guest's log channel and decodes each frame as a `LogRecord`.
fn drain_logs(runtime: &Runtime, process_id: u64) -> Vec<String> {
    runtime
        .kernel()
        .processes()
        .drain_log_channel(process_id)
        .expect("drain log channel")
        .iter()
        .map(|frame| {
            selium_service::log::LogRecord::decode(frame)
                .expect("decode log record")
                .message
        })
        .collect()
}

/// Waits for the publisher to announce its channel, returning the region id.
#[expect(clippy::panic, reason = "test helper")]
fn parse_channel_region(runtime: &Runtime, process_id: u64, timeout: Duration) -> u64 {
    let start = Instant::now();
    let mut seen: Vec<String> = Vec::new();
    while start.elapsed() < timeout {
        for message in drain_logs(runtime, process_id) {
            if let Some(region_id) = message
                .strip_prefix("wake-probe: channel ")
                .and_then(|rest| rest.trim().parse::<u64>().ok())
            {
                return region_id;
            }
            seen.push(message);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for the publisher's channel announcement; got {seen:?}");
}

/// Task 3.1: a yield-only pass is not forward progress, so a host that keeps
/// polling a parked watcher must not spend the watcher's execution budget.
///
/// The watcher parks on its channel; polling it re-enters the guest with
/// nothing to do. That re-entry must be a no-op pass, not a loop that drives
/// the guest hard enough to exhaust the budget and get it reaped on
/// `ExecutionBudgetExceeded`.
// scenario: selium-guest/reactor-parking-and-wake-sources/reactor-stalls-on-channel-waits
#[test]
#[ignore = "requires the wake-probe guest built for wasm32-unknown-unknown"]
fn polling_a_parked_watcher_does_not_spend_its_budget() {
    let runtime = Runtime::default();

    // A parker owns the channel and never publishes, so the watcher parks
    // for the whole test: nothing will legitimately wake it, and every
    // instruction it spends is spent on being polled.
    let parker_id = spawn_probe(&runtime, MODE_PARKER, "");
    let region_id = parse_channel_region(&runtime, parker_id, Duration::from_secs(10));

    let watcher_id = spawn_probe(&runtime, MODE_WATCHER, &region_id.to_string());
    wait_for_log(
        &runtime,
        watcher_id,
        "wake-probe: watching",
        Duration::from_secs(10),
    );

    runtime.metering_tick();
    let before = runtime
        .kernel()
        .processes()
        .metering_observation(watcher_id)
        .expect("metered watcher")
        .cpu_instructions;

    // A polling host: many passes over a guest that has nothing to do.
    const POLLS: usize = 2_000;
    let started = Instant::now();
    for _ in 0..POLLS {
        runtime.poll_guest(watcher_id);
    }
    let elapsed = started.elapsed();

    let activity = runtime.activity_log();
    let reaped = activity.iter().any(|event| {
        event.process_id == Some(watcher_id)
            && (event
                .message
                .contains("exhausted its CPU instruction budget")
                || event.message.contains("poll trapped"))
    });
    assert!(
        !reaped,
        "a parked watcher must not be reaped for budget it never spent; \
         activity: {activity:#?}"
    );

    runtime.metering_tick();
    let Some(observation) = runtime
        .kernel()
        .processes()
        .metering_observation(watcher_id)
    else {
        panic!("watcher no longer metered; activity: {activity:#?}");
    };
    let spent = observation.cpu_instructions.saturating_sub(before);
    // Each poll costs one interpreter pass over an idle guest, and no more.
    // The bound is what pins "a pass that re-arms is not forward progress":
    // if the host kept re-entering, the cost per poll would climb with the
    // poll count instead of staying flat.
    assert!(
        spent <= POLLS as u64 * MAX_INSTRUCTIONS_PER_IDLE_POLL,
        "{POLLS} polls spent {spent} instructions \
         (baseline {before}, {elapsed:?}); an idle poll must cost at most \
         {MAX_INSTRUCTIONS_PER_IDLE_POLL} instructions"
    );

    // Both probes are still resident: the watcher neither exited nor was
    // reaped for the polls it was handed.
    assert_eq!(runtime.loaded_guest_count(), 2, "both probes stay resident");

    runtime.stop_process(watcher_id).expect("stop watcher");
    runtime.stop_process(parker_id).expect("stop parker");
}

/// Task 3.1 (yield side): a guest whose only activity is `yield_now` is not
/// re-entered by the host. One poll pass re-arms the yielded task and returns;
/// the host must not treat that re-arm as forward progress and drive the pass
/// again, so `POLLS` polls yield exactly `POLLS` times.
#[test]
#[ignore = "requires the wake-probe guest built for wasm32-unknown-unknown"]
fn polling_a_yield_only_guest_does_not_re_enter_it() {
    let runtime = Runtime::default();
    let yielder_id = spawn_probe(&runtime, MODE_YIELDER, "");
    wait_for_log(
        &runtime,
        yielder_id,
        "wake-probe: yielding",
        Duration::from_secs(10),
    );

    // The boot pass itself yields once, so start from a drained channel.
    drop(drain_logs(&runtime, yielder_id));

    const POLLS: usize = 1_000;
    for _ in 0..POLLS {
        runtime.poll_guest(yielder_id);
    }

    // The yielder reports every `YIELD_LOG_INTERVAL` yields; each report is
    // therefore that many yields the host drove.
    let reports = drain_logs(&runtime, yielder_id)
        .iter()
        .filter(|message| message.starts_with("wake-probe: yielded "))
        .count() as u64;
    let yields = reports * YIELD_LOG_INTERVAL;

    assert!(
        yields >= POLLS as u64 && yields < 2 * POLLS as u64,
        "{POLLS} polls drove {yields} yields; each poll must re-arm the \
         yielded task exactly once, never loop on it"
    );
    assert!(
        runtime.loaded_guest_count() == 1,
        "the yielder must survive being polled: {:#?}",
        runtime.activity_log()
    );

    runtime.stop_process(yielder_id).expect("stop yielder");
}

fn probe_descriptor(mode: u64, region_id: &str) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: format!("wake-probe-{mode}"),
        module_id: format!("wake-probe-{mode}-module"),
        module_bytes: common::read_guest_wasm_debug("selium-wake-probe", "selium_wake_probe.wasm"),
        entrypoint: "wake_probe".to_string(),
        arguments: vec![
            selium_runtime::SystemGuestArg::Integer(mode),
            selium_runtime::SystemGuestArg::Pointer(region_id.as_bytes().to_vec()),
        ],
        grants: vec![CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        )],
        tenant: None,
        discovery_service: false,
        handlers: Vec::new(),
    }
}

/// Spawns the probe guest in `mode`, failing loudly with the activity log (which
/// carries the guest's own log records) when the entrypoint errors.
#[expect(
    clippy::panic,
    reason = "test helper: a failed entrypoint is a test failure"
)]
fn spawn_probe(runtime: &Runtime, mode: u64, region_id: &str) -> u64 {
    let descriptor = probe_descriptor(mode, region_id);
    match runtime.spawn_system_guest(descriptor) {
        Ok(bootstrapped) => bootstrapped.process_id,
        Err(error) => panic!(
            "spawn probe guest: {error}; activity: {:#?}",
            runtime.activity_log()
        ),
    }
}

/// Polls the guest log channel until a message containing `needle` appears.
#[expect(clippy::panic, reason = "test helper")]
fn wait_for_log(runtime: &Runtime, process_id: u64, needle: &str, timeout: Duration) -> Duration {
    let mut seen: Vec<String> = Vec::new();
    let start = Instant::now();
    while start.elapsed() < timeout {
        seen.extend(drain_logs(runtime, process_id));
        if seen.iter().any(|message| message.contains(needle)) {
            return start.elapsed();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {needle:?} in guest log; got {seen:?}");
}

/// Task 1.3: delivering a wake to a process that has already exited is a
/// no-op — the delivery is idempotent and tolerates a dead process id.
// scenario: selium-runtime/cross-process-channel-wake-delivery/waking-a-stopped-process-is-a-no-op
#[test]
#[ignore = "requires the wake-probe guest built for wasm32-unknown-unknown"]
fn waking_a_stopped_guest_is_a_no_op() {
    let runtime = Runtime::default();

    let publisher_id = spawn_probe(&runtime, MODE_PUBLISHER, "");
    let region_id = parse_channel_region(&runtime, publisher_id, Duration::from_secs(10));

    let watcher_id = spawn_probe(&runtime, MODE_WATCHER, &region_id.to_string());
    wait_for_log(
        &runtime,
        watcher_id,
        "wake-probe: watching",
        Duration::from_secs(10),
    );

    runtime.stop_process(watcher_id).expect("stop watcher");

    // The watcher is registered as a waiter on the channel's region, and the
    // publisher keeps publishing, so every subsequent generation advance
    // still targets the stopped process. Delivery must tolerate the dead
    // process id rather than failing or wedging the runtime: the publisher
    // must keep running, and a fresh watcher on the same region must still
    // converge.
    let published = count_publishes(&runtime, publisher_id, Duration::from_secs(2));
    assert!(
        published > 5,
        "the publisher must keep running while wakes target a stopped process; \
         saw {published} publishes in 2s"
    );

    // Delivery is idempotent: a second watcher converges exactly as the first
    // did, with a dead registration still present for the same region.
    let survivor_id = spawn_probe(&runtime, MODE_WATCHER, &region_id.to_string());
    wait_for_log(
        &runtime,
        survivor_id,
        "wake-probe: woke",
        Duration::from_secs(10),
    );

    runtime.stop_process(survivor_id).expect("stop survivor");
    runtime.stop_process(publisher_id).expect("stop publisher");
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `channel-wake-wait` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.
