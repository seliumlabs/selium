//! Soak lane: do repeated spawns, timers, and registrations leave host-held
//! state bounded?
//!
//! A quiet platform is not the same as a leak-free one. This test cycles the
//! operations that mint host-side records — process spawn/stop, region
//! allocation, host-queue creation, guest timers, and discovery
//! registrations — many times over one runtime, and after every cycle asserts
//! that the records return to their baseline:
//!
//! - the timer driver's pending sleeps (`Runtime::pending_timer_count`),
//! - live shared regions and their local mappings (`MemoryRegistry`),
//! - live host queues (`HostQueueRegistry`),
//! - the host's per-process claim record (`Runtime::expected_claims`),
//! - the discovery feed's owner-keyed revocation for each stopped provider.
//!
//! It is `#[ignore]`d because it is too slow for every push; the gate runs it
//! in the scheduled/nightly lane (`scripts/gate.sh --soak`). Cycle count
//! defaults to 25 and can be raised with `SELIUM_SOAK_CYCLES` for a longer
//! run.
//!
//! ```sh
//! cargo build --target wasm32-unknown-unknown \
//!   -p selium-discovery -p selium-discovery-probe -p selium-timer-probe
//! cargo test -p selium-runtime --test soak -- --ignored
//! ```

#![allow(
    clippy::panic,
    reason = "soak assertions and failure diagnostics are panics by design"
)]

use std::{
    collections::HashSet,
    time::{Duration, Instant},
};

use selium_abi::{
    ActivityKind, Capability, CapabilityGrant, CompletionState, HostcallOutput, HostcallRequest,
    RegionProt, ResourceClass, ResourceIdentity, ResourceKind, ResourceSelector,
};
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestArg, SystemGuestDescriptor};
use selium_service::{DiscoveryRequest, FlatMsg};
use selium_shm::{Channel, transport::ShmTransport};
use selium_wire::{framed::FramedRead, pubsub::Subscriber};

mod common;

/// Cycles run by default. Enough to expose a per-cycle leak without making the
/// nightly lane hour-long.
const DEFAULT_CYCLES: usize = 25;
/// Reviewed bound on a provider's registration being admitted.
const REGISTRATION_BOUND: Duration = Duration::from_secs(60);
/// Reviewed bound on a revocation reaching the discovery feed after the owner
/// stops.
const REVOCATION_BOUND: Duration = Duration::from_secs(30);
/// The discovery-probe provider role: register a route and stay resident.
const ROLE_PROVIDER: u64 = 1;
/// Reviewed bound on one spawn/stop cycle's guest work.
const SPAWN_BOUND: Duration = Duration::from_secs(60);
/// Reviewed bound on the timer probe's 250 ms sleep firing.
const TIMER_BOUND: Duration = Duration::from_secs(30);

/// The host-held state the soak holds bounded. Timers are handled separately:
/// the discovery guest keeps one sweep sleep queued at all times, so a raw
/// count is not comparable across cycles.
#[derive(Debug, PartialEq, Eq)]
struct HostState {
    regions: usize,
    mappings: usize,
}

/// Mints a shared region attributed to `process_id` and returns its id.
#[expect(clippy::panic, reason = "unexpected hostcall output indicates a bug")]
fn alloc_region(runtime: &Runtime, process_id: u64) -> u64 {
    let (status, op_id) = runtime.begin_hostcall(
        process_id,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: ResourceKind::SharedMemory,
            serving_tenant: None,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    match runtime.poll_hostcall(process_id, op_id) {
        CompletionState::Ready(HostcallOutput::RegionAlloc(alloc)) => alloc.region_id,
        other => panic!("expected RegionAlloc, got {other:?}"),
    }
}

/// The discovery feed as a host-side subscriber, replaying from the ring's
/// start so bootstrap-time events are not missed.
fn attach_feed_subscriber(runtime: &Runtime) -> Subscriber<DiscoveryRequest, ShmTransport> {
    let feed_region_id = runtime
        .discovery_feed_region_id()
        .expect("discovery feed region id");
    let channel = Channel::attach_with_provider(runtime.region_provider().as_ref(), feed_region_id)
        .expect("attach to discovery feed");
    let capacity = channel.ring().capacity();
    let transport = ShmTransport::new_replay(&channel, &channel).expect("feed transport");
    Subscriber::new(FramedRead::new(transport), Some(capacity))
}

/// Mints a host queue attributed to `process_id` and returns its shared id.
#[expect(clippy::panic, reason = "unexpected hostcall output indicates a bug")]
fn create_host_queue(runtime: &Runtime, process_id: u64) -> u64 {
    let (status, op_id) = runtime.begin_hostcall(
        process_id,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    match runtime.poll_hostcall(process_id, op_id) {
        CompletionState::Ready(HostcallOutput::HostQueue(descriptor)) => descriptor.shared_id,
        other => panic!("expected HostQueue, got {other:?}"),
    }
}

/// The discovery service: RPC listener + registration feed.
fn discovery_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "discovery".to_string(),
        module_id: "discovery-module".to_string(),
        module_bytes,
        entrypoint: "discovery_main".to_string(),
        arguments: Vec::new(),
        grants: vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(Capability::ProcessProbe, Vec::new()),
            CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///discovery-routes".to_string())],
            ),
            CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///discovery".to_string())],
            ),
        ],
        tenant: None,
        discovery_service: true,
        handlers: Vec::new(),
    }
}

fn discovery_wasm() -> Vec<u8> {
    common::read_guest_wasm_debug("selium-discovery", "selium_discovery.wasm")
}

fn drain_logs(runtime: &Runtime, process_id: u64) -> Vec<String> {
    let Ok(frames) = runtime.kernel().processes().drain_log_channel(process_id) else {
        return Vec::new();
    };
    frames
        .iter()
        .map(|frame| {
            selium_service::log::LogRecord::decode(frame)
                .expect("decode log record")
                .message
        })
        .collect()
}

/// A minimal host-side guest: it exists to own a region and a queue minted on
/// its behalf, then to be stopped.
fn host_guest_descriptor(cycle: usize) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: format!("soak-host-{cycle}"),
        module_id: format!("soak-host-{cycle}-module"),
        module_bytes: wat::parse_str("(module (func (export \"boot\")))").expect("compile wat"),
        entrypoint: "boot".to_string(),
        arguments: Vec::new(),
        grants: vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
        ],
        tenant: None,
        discovery_service: false,
        handlers: Vec::new(),
    }
}

fn host_state(runtime: &Runtime) -> HostState {
    let memory = runtime.kernel().memory();
    HostState {
        regions: memory.live_shared_region_count(),
        mappings: memory.live_shared_mapping_count(),
    }
}

/// A `discovery-probe` provider for this cycle, wired to the running discovery
/// listener exactly as `route_watch_convergence` drives it.
fn provider_descriptor(runtime: &Runtime, cycle: usize) -> SystemGuestDescriptor {
    let listener = runtime
        .discovery_listener_shared_id()
        .expect("discovery listener shared id");
    SystemGuestDescriptor {
        name: format!("soak-provider-{cycle}"),
        module_id: format!("soak-provider-{cycle}-module"),
        module_bytes: common::read_guest_wasm_debug(
            "selium-discovery-probe",
            "selium_discovery_probe.wasm",
        ),
        entrypoint: "discovery_probe".to_string(),
        arguments: vec![
            SystemGuestArg::Integer(listener),
            SystemGuestArg::Pointer(provider_route(cycle).into_bytes()),
            SystemGuestArg::Integer(ROLE_PROVIDER),
        ],
        grants: vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ExplicitResource(
                    ResourceIdentity::Shared(listener),
                )],
            ),
        ],
        tenant: Some("probe".to_string()),
        discovery_service: false,
        handlers: Vec::new(),
    }
}

/// The route a cycle's provider registers. Unique per cycle so a retained
/// registration cannot masquerade as a fresh one.
fn provider_route(cycle: usize) -> String {
    format!("sel://probe/app/soak-{cycle}")
}

/// One soak cycle: mint and release a region and a host queue, fire a guest
/// timer, and register then revoke a discovery route.
fn run_cycle(
    runtime: &Runtime,
    feed: &mut Subscriber<DiscoveryRequest, ShmTransport>,
    cycle: usize,
    discovery_id: u64,
) {
    // ── Region and queue ownership ─────────────────────────────────────────
    let host_guest = runtime
        .spawn_system_guest(host_guest_descriptor(cycle))
        .unwrap_or_else(|error| panic!("cycle {cycle}: spawn host guest: {error}"))
        .process_id;

    let region_id = alloc_region(runtime, host_guest);
    let queue_id = create_host_queue(runtime, host_guest);
    assert!(
        region_id != 0 && queue_id != 0,
        "cycle {cycle}: host resources minted"
    );

    runtime
        .stop_process(host_guest)
        .unwrap_or_else(|error| panic!("cycle {cycle}: stop host guest: {error}"));
    assert!(
        runtime.expected_claims(host_guest).is_empty(),
        "cycle {cycle}: the stopped process's claim record must be gone"
    );
    settle(runtime);

    // ── Guest timers ───────────────────────────────────────────────────────
    // The discovery guest keeps one sleep queued for its lease sweep, so the
    // check is that the probe's sleep does not *add* a lingering one: the
    // count must return to at most the ambient count it had before the spawn.
    let timers_before = runtime.pending_timer_count();
    let probe = runtime
        .spawn_system_guest(timer_probe_descriptor())
        .unwrap_or_else(|error| panic!("cycle {cycle}: spawn timer probe: {error}"))
        .process_id;
    wait_for_log(runtime, probe, "timer-probe: timer fired", TIMER_BOUND);
    wait_for_exit(runtime, probe, TIMER_BOUND);
    wait_until(
        &format!("cycle {cycle}: the timer driver to drain the probe's sleep"),
        TIMER_BOUND,
        || runtime.pending_timer_count() <= timers_before,
    );

    settle(runtime);

    // ── Discovery registration ─────────────────────────────────────────────
    // `start_discovery: false` still wires the running discovery handle into
    // the new guest, which is how a platform adds a guest after bootstrap.
    let provider_report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![provider_descriptor(runtime, cycle)],
            state_dir: None,
        })
        .unwrap_or_else(|error| panic!("cycle {cycle}: spawn provider: {error}"));
    let provider = provider_report
        .guests
        .first()
        .expect("provider in report")
        .process_id;
    let route = provider_route(cycle);

    wait_for_log(
        runtime,
        provider,
        &format!("provider: registered {route}"),
        REGISTRATION_BOUND,
    );
    settle(runtime);

    runtime
        .stop_process(provider)
        .unwrap_or_else(|error| panic!("cycle {cycle}: stop provider: {error}"));
    assert!(
        runtime.expected_claims(provider).is_empty(),
        "cycle {cycle}: a stopped provider's claim must be released"
    );
    settle(runtime);

    // The owner-keyed revocation must reach the feed: discovery is a
    // projection of authority, and a registration it never learns to drop is
    // exactly the unbounded table this lane exists to catch.
    let deadline = Instant::now() + REVOCATION_BOUND;
    let mut seen = HashSet::new();
    while Instant::now() < deadline {
        match feed.read_with_tag() {
            Ok((DiscoveryRequest::ProcessGone { process_id }, _tag)) => {
                if process_id == provider {
                    return;
                }
                seen.insert(process_id);
            }
            Ok((DiscoveryRequest::Revoke { uri }, _tag)) => {
                if uri == route {
                    return;
                }
            }
            Ok(_) => {}
            Err(selium_wire::error::Error::BufferEmpty) => {}
            Err(error) => panic!("cycle {cycle}: discovery feed read failed: {error}"),
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "cycle {cycle}: no revocation for provider {provider} ({route}) reached the feed; \
         other process-gone ids seen: {seen:?}; discovery resident: {}",
        runtime.is_guest_resident(discovery_id)
    );
}

/// Waits for host-held state to stop changing.
///
/// Teardown of a guest whose entrypoint completed runs on the runtime's own
/// reactor thread, so `loaded_guest_count` can drop before the reclaim pass
/// has finished. Comparing counts immediately would race that pass; this waits
/// for a quiet window instead.
fn settle(runtime: &Runtime) {
    let timeout = Duration::from_secs(10);
    let quiet = Duration::from_millis(150);
    let start = Instant::now();
    let mut previous = host_state(runtime);
    let mut stable_since = Instant::now();
    while start.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(25));
        let current = host_state(runtime);
        if current == previous {
            if stable_since.elapsed() >= quiet {
                return;
            }
        } else {
            previous = current;
            stable_since = Instant::now();
        }
    }
    // A state that never settles is itself a finding; the caller's assertion
    // reports the values.
}

// scenario: resource-handshake/host-mediated-connection-queue-resourcesender-resourcelistener/last-handle-releases-the-queue
// scenario: resource-handshake/host-mediated-connection-queue-resourcesender-resourcelistener/discovery-listener-queue-is-released-when-discovery-stops
// scenario: sleep-hostcall/teardown-releases-pending-sleeps/stopped-guest-leaves-no-pending-timer
// scenario: sleep-hostcall/teardown-releases-pending-sleeps/a-stopped-discovery-guest-drains-its-sweep-sleep
// scenario: verification-gate/soak-and-crash-consistency-coverage/resource-growth-is-detected
#[test]
#[ignore = "soak lane: repeated spawn/stop cycles, run nightly via scripts/gate.sh --soak"]
fn soak_repeated_spawns_timers_and_registrations_leave_bounded_host_state() {
    let cycles = std::env::var("SELIUM_SOAK_CYCLES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|cycles| *cycles > 0)
        .unwrap_or(DEFAULT_CYCLES);

    let runtime = Runtime::default();

    // One discovery guest hosts every registration the soak makes; it is
    // bootstrapped once, like a real platform.
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![discovery_descriptor(discovery_wasm())],
            state_dir: None,
        })
        .expect("bootstrap discovery");
    let discovery_id = report
        .guests
        .iter()
        .find(|guest| guest.name == "discovery")
        .expect("discovery in report")
        .process_id;

    let mut feed = attach_feed_subscriber(&runtime);

    // Warm-up cycle: the first cycle can allocate lazily (route table, log
    // channel, caches), so the baseline is taken after it rather than before.
    run_cycle(&runtime, &mut feed, 0, discovery_id);
    // Let the warm-up's transient connection regions be reaped before the
    // steady-state baseline is recorded (the handler's idle check is 2 s).
    wait_for_stable_state(&runtime, Duration::from_secs(15));
    let baseline = host_state(&runtime);
    let baseline_queues = runtime.kernel().queues().live_queue_count();

    for cycle in 1..=cycles {
        run_cycle(&runtime, &mut feed, cycle, discovery_id);

        // A departed client's connection region is released by its handler on
        // the next idle check (<= 2 s), so wait for the baseline rather than
        // sampling immediately.
        let deadline = Instant::now() + Duration::from_secs(15);
        while host_state(&runtime) != baseline && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let state = host_state(&runtime);
        assert_eq!(
            state,
            baseline,
            "cycle {cycle}: host-held regions/mappings grew: {state:?} vs {baseline:?}; \
             region owners: {:?}; activity: {:#?}",
            runtime.region_owner_report(),
            runtime.activity_log()
        );
        assert_eq!(
            runtime.kernel().queues().live_queue_count(),
            baseline_queues,
            "cycle {cycle}: host queues leaked"
        );
    }

    // Tear discovery down: everything it minted must go with it.
    runtime.stop_process(discovery_id).expect("stop discovery");
    let start = Instant::now();
    while runtime.loaded_guest_count() != 0 && start.elapsed() < SPAWN_BOUND {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        runtime.loaded_guest_count(),
        0,
        "guests still loaded after discovery stopped: {:?}",
        runtime.resident_guest_ids()
    );
    // The lease-sweep sleep is still queued when the guest stops; it must be
    // discarded at its deadline rather than linger (or be renewed).
    wait_until(
        "the stopped discovery's sweep sleep to drain",
        SPAWN_BOUND,
        || runtime.pending_timer_count() == 0,
    );
    assert_eq!(runtime.pending_timer_count(), 0, "no stranded sleeps");
    assert_eq!(
        runtime.kernel().queues().live_queue_count(),
        0,
        "a stopped discovery must release its listener queue"
    );
}

/// The timer probe guest: sleeps 250 ms and exits.
fn timer_probe_descriptor() -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "timer-probe".to_string(),
        module_id: "timer-probe-module".to_string(),
        module_bytes: common::read_guest_wasm_debug(
            "selium-timer-probe",
            "selium_timer_probe.wasm",
        ),
        entrypoint: "timer_probe".to_string(),
        arguments: Vec::new(),
        grants: vec![CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        )],
        tenant: None,
        discovery_service: false,
        handlers: Vec::new(),
    }
}

/// Waits for the durable `ProcessExited` activity record for `process_id`.
///
/// The loaded-guest table is transiently empty while a guest is polled, so
/// residency is not a reliable completion signal.
#[expect(clippy::panic, reason = "test helper")]
fn wait_for_exit(runtime: &Runtime, process_id: u64, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if runtime.activity_log().iter().any(|event| {
            event.process_id == Some(process_id) && event.kind == ActivityKind::ProcessExited
        }) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("process {process_id} did not exit within {timeout:?}");
}

#[expect(clippy::panic, reason = "test helper")]
fn wait_for_log(runtime: &Runtime, process_id: u64, needle: &str, timeout: Duration) {
    let mut seen = Vec::new();
    let start = Instant::now();
    while start.elapsed() < timeout {
        seen.extend(drain_logs(runtime, process_id));
        if seen.iter().any(|message| message.contains(needle)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {needle:?}; got {seen:?}");
}

/// Waits until host-held state has been unchanged for a quiet window.
///
/// A departed client's connection region is released by its handler on the
/// next idle check, so state can still be falling when a cycle returns. The
/// baseline and the per-cycle comparison must be taken from a steady state.
fn wait_for_stable_state(runtime: &Runtime, timeout: Duration) {
    let quiet = Duration::from_millis(2_500);
    let start = Instant::now();
    let mut previous = host_state(runtime);
    let mut stable_since = Instant::now();
    while start.elapsed() < timeout {
        std::thread::sleep(Duration::from_millis(100));
        let current = host_state(runtime);
        if current == previous {
            if stable_since.elapsed() >= quiet {
                return;
            }
        } else {
            previous = current;
            stable_since = Instant::now();
        }
    }
}

#[expect(clippy::panic, reason = "test helper")]
fn wait_until(what: &str, timeout: Duration, condition: impl Fn() -> bool) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what} after {timeout:?}");
}
