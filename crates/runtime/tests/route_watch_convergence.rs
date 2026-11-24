//! End-to-end route-watch convergence (`cross-guest-wake-delivery`).
//!
//! Drives the `route-watch` promise against the real `selium-discovery`
//! guest: a consumer that starts *before* its provider converges on the
//! provider's registration, observes its revocation when the provider exits,
//! and re-attaches when a restarted provider re-registers with a new target.
//!
//! The consumer is never polled by the test. Draining its log channel reads
//! shared memory and does not re-enter the guest's WASM, so the only thing
//! that can produce a `consumer:` line is a wake delivered by the runtime —
//! here, the discovery guest publishing a route change into the table the
//! consumer is parked on, crossing the process boundary through
//! `WaitRegister` → `GenerationAdvance` → mailbox.
//!
//! Also pins task 4.2's sibling observation: the consumer is re-entered by a
//! wake alone, so it stays resident with its budget intact. Task 4.3
//! (discovery's lease sweep ticking) lives in `discovery_lease_sweep` — one
//! `Runtime` per process, so each scenario needs its own binary.
//!
//! `#[ignore]`d by default because it requires the guests built for
//! `wasm32-unknown-unknown`:
//!
//! ```sh
//! cargo build --target wasm32-unknown-unknown -p selium-discovery -p selium-discovery-probe
//! cargo test -p selium-runtime --test route_watch_convergence -- --ignored
//! ```

use std::time::{Duration, Instant};

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceIdentity, ResourceSelector};
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestArg, SystemGuestDescriptor};
use selium_service::FlatMsg;

mod common;

/// Reviewed bound on a route-watch convergence event. Both sides run in the
/// interpreted guest; the appearance is driven by a host wake, so a loaded
/// runner only delays it.
const CONVERGENCE_BOUND: Duration = Duration::from_secs(30);
/// The consumer role: watch a route and log every event.
const ROLE_CONSUMER: u64 = 2;
/// The provider role: register a route and stay resident.
const ROLE_PROVIDER: u64 = 1;
/// The route under watch. Multi-segment so it is a plain named-service
/// registration rather than a leaf alias (an alias additionally requires the
/// typed target to be registered under its own URI first).
const ROUTE: &str = "sel://probe/app/service";

/// Waits for the consumer to report an appearance and returns the resource id
/// it converged on.
#[expect(clippy::panic, reason = "test helper: a malformed log line is a bug")]
fn appeared_resource(runtime: &Runtime, consumer_id: u64) -> u64 {
    let line = expect_line(
        runtime,
        consumer_id,
        "consumer: appeared",
        CONVERGENCE_BOUND,
    );
    line.split_whitespace()
        .next_back()
        .and_then(|id| id.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("appeared line carries no resource id: {line:?}"))
}

/// Bootstraps the discovery service plus a route-watch consumer.
fn bootstrap_discovery_and_consumer(runtime: &Runtime) -> (u64, u64) {
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![
                discovery_descriptor(discovery_wasm()),
                probe_descriptor(
                    // The leading argument is a pointer, so bootstrap
                    // prepends the discovery handle for this guest.
                    vec![
                        SystemGuestArg::Pointer(ROUTE.as_bytes().to_vec()),
                        SystemGuestArg::Integer(ROLE_CONSUMER),
                    ],
                    ROLE_CONSUMER,
                ),
            ],
            state_dir: None,
        })
        .expect("bootstrap discovery and consumer");

    let consumer = report
        .guests
        .iter()
        .find(|guest| guest.name == guest_name(ROLE_CONSUMER))
        .expect("consumer in report")
        .process_id;
    let discovery = report
        .guests
        .iter()
        .find(|guest| guest.name == "discovery")
        .expect("discovery in report")
        .process_id;
    (consumer, discovery)
}

/// Task 4.2: a consumer started before its provider converges, and re-attaches
/// after the provider restarts.
// scenario: selium-runtime/cross-process-channel-wake-delivery/host-commit-wakes-its-subscriber
// scenario: route-watch/reactive-route-watch/provider-is-revoked
// scenario: route-watch/convergence-without-a-readiness-gate/provider-restarts-mid-life
#[test]
#[ignore = "requires the discovery and discovery-probe guests built for wasm32-unknown-unknown"]
fn consumer_converges_on_a_provider_that_starts_later_and_restarts() {
    let runtime = Runtime::default();
    let (consumer_id, discovery_id) = bootstrap_discovery_and_consumer(&runtime);

    // The consumer is parked on the route table before any provider exists.
    expect_line(
        &runtime,
        consumer_id,
        "consumer: watching",
        CONVERGENCE_BOUND,
    );

    // --- The provider appears. Nothing polls the consumer. ---
    let first_provider = spawn_provider(&runtime);
    expect_line(
        &runtime,
        first_provider,
        "provider: registered",
        CONVERGENCE_BOUND,
    );
    let first = appeared_resource(&runtime, consumer_id);

    // --- The provider exits: the route is revoked. ---
    runtime
        .stop_process(first_provider)
        .expect("stop first provider");
    expect_line(
        &runtime,
        consumer_id,
        "consumer: revoked",
        CONVERGENCE_BOUND,
    );

    // --- The provider restarts with a fresh resource: re-attach. ---
    let second_provider = spawn_provider(&runtime);
    expect_line(
        &runtime,
        second_provider,
        "provider: registered",
        CONVERGENCE_BOUND,
    );
    let second = appeared_resource(&runtime, consumer_id);
    assert_ne!(
        second, first,
        "the restarted provider must mint a new resource the consumer re-attaches to"
    );

    // The consumer never exited: it is still the same resident process. Check
    // residency, not `loaded_guest_count`: a guest is briefly absent from the
    // loaded set while another thread drives its reactor, so the count is a
    // racy proxy for liveness.
    for (name, process_id) in [("consumer", consumer_id), ("discovery", discovery_id)] {
        assert!(
            runtime.is_guest_resident(process_id),
            "{name} (process {process_id}) must still be resident; activity: {:#?}",
            runtime.activity_log()
        );
    }
}

fn discovery_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "discovery".to_string(),
        module_id: "discovery-module".to_string(),
        module_bytes,
        entrypoint: "discovery_main".to_string(),
        arguments: Vec::new(), // populated by bootstrap
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

fn discovery_probe_wasm() -> Vec<u8> {
    common::read_guest_wasm_debug("selium-discovery-probe", "selium_discovery_probe.wasm")
}

fn discovery_wasm() -> Vec<u8> {
    common::read_guest_wasm_debug("selium-discovery", "selium_discovery.wasm")
}

fn drain_logs(runtime: &Runtime, process_id: u64) -> Vec<String> {
    let Ok(frames) = runtime.kernel().processes().drain_log_channel(process_id) else {
        // A process that has already exited has no log channel left; the
        // interesting lines were drained before it went.
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

/// Polls the guest log channel until a line starting with `prefix` appears,
/// returning it.
#[expect(clippy::panic, reason = "test helper")]
fn expect_line(runtime: &Runtime, process_id: u64, prefix: &str, timeout: Duration) -> String {
    let mut seen: Vec<String> = Vec::new();
    let start = Instant::now();
    while start.elapsed() < timeout {
        seen.extend(drain_logs(runtime, process_id));
        if let Some(found) = seen.iter().find(|message| message.starts_with(prefix)) {
            return found.clone();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {prefix:?} in guest log; got {seen:?}");
}

fn guest_name(mode: u64) -> String {
    format!("discovery-probe-{mode}")
}

fn probe_descriptor(arguments: Vec<SystemGuestArg>, mode: u64) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: guest_name(mode),
        module_id: format!("discovery-probe-{mode}-module"),
        module_bytes: discovery_probe_wasm(),
        entrypoint: "discovery_probe".to_string(),
        arguments,
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
        tenant: Some("probe".to_string()),
        discovery_service: false,
        handlers: Vec::new(),
    }
}

/// Spawns a provider for [`ROUTE`], wiring in the discovery listener handle
/// that `bootstrap_system_guests` would otherwise have supplied.
#[expect(
    clippy::panic,
    reason = "test helper: a failed entrypoint is a test failure"
)]
fn spawn_provider(runtime: &Runtime) -> u64 {
    let listener = runtime
        .discovery_listener_shared_id()
        .expect("discovery listener shared id");
    let mut descriptor = probe_descriptor(
        vec![
            SystemGuestArg::Integer(listener),
            SystemGuestArg::Pointer(ROUTE.as_bytes().to_vec()),
            SystemGuestArg::Integer(ROLE_PROVIDER),
        ],
        ROLE_PROVIDER,
    );
    descriptor.grants.push(CapabilityGrant::new(
        Capability::HostQueue,
        vec![ResourceSelector::ExplicitResource(
            ResourceIdentity::Shared(listener),
        )],
    ));
    runtime
        .spawn_system_guest(descriptor)
        .unwrap_or_else(|error| {
            panic!(
                "spawn provider guest: {error}; activity: {:#?}",
                runtime.activity_log()
            )
        })
        .process_id
}
