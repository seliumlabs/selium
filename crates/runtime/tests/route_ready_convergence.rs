//! Bounded, watch-driven readiness convergence (`deterministic-guest-io`).
//!
//! Drives `Context::resolve_ready` against the real `selium-discovery` guest:
//! a consumer that boots before its provider converges on the provider's later
//! registration without any retry interval, a route nobody publishes times out
//! with the typed readiness error, and a request-path `lookup` of an absent URI
//! answers immediately instead of waiting for the readiness bound.
//!
//! The consumer is never polled by the test: draining its log channel reads
//! shared memory and does not re-enter the guest's WASM, so a `ready:` line can
//! only be produced by a wake delivered by the runtime.
//!
//! `#[ignore]`d by default because it requires the guests built for
//! `wasm32-unknown-unknown`:
//!
//! ```sh
//! cargo build --target wasm32-unknown-unknown -p selium-discovery -p selium-discovery-probe
//! cargo test -p selium-runtime --test route_ready_convergence -- --ignored
//! ```

use std::time::{Duration, Instant};

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceIdentity, ResourceSelector};
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestArg, SystemGuestDescriptor};
use selium_service::FlatMsg;

mod common;

/// The route nobody ever publishes.
const ABSENT_ROUTE: &str = "sel://probe/ready/absent";
/// Reviewed bound on a convergence event (the appearance is host-wake driven).
const CONVERGENCE_BOUND: Duration = Duration::from_secs(30);
/// The route that appears late.
const LATE_ROUTE: &str = "sel://probe/ready/late";
/// Provider role.
const ROLE_PROVIDER: u64 = 1;
/// Converging readiness role.
const ROLE_READY: u64 = 9;
/// Immediate request-path miss role.
const ROLE_READY_MISS: u64 = 11;
/// Short-bound readiness role (times out).
const ROLE_READY_TIMEOUT: u64 = 10;

/// Bootstraps discovery plus a ready probe for `LATE_ROUTE`, a short-bound
/// probe for `ABSENT_ROUTE`, and an immediate-miss probe for `ABSENT_ROUTE`.
#[expect(
    clippy::panic,
    reason = "a missing bootstrap report entry is a test failure"
)]
fn bootstrap(runtime: &Runtime) -> (u64, u64, u64) {
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![
                discovery_descriptor(discovery_wasm()),
                probe_descriptor(LATE_ROUTE, ROLE_READY),
                probe_descriptor(ABSENT_ROUTE, ROLE_READY_TIMEOUT),
                probe_descriptor(ABSENT_ROUTE, ROLE_READY_MISS),
            ],
            state_dir: None,
        })
        .expect("bootstrap discovery and readiness probes");

    let process = |mode: u64| {
        report
            .guests
            .iter()
            .find(|guest| guest.name == guest_name(mode))
            .unwrap_or_else(|| panic!("probe role {mode} in bootstrap report"))
            .process_id
    };
    (
        process(ROLE_READY),
        process(ROLE_READY_TIMEOUT),
        process(ROLE_READY_MISS),
    )
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
        // A process that already exited has no log channel left; its
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

#[expect(
    clippy::panic,
    reason = "a missing log line within the bound is a test failure"
)]
fn expect_line(runtime: &Runtime, process_id: u64, prefix: &str, timeout: Duration) -> String {
    let mut seen: Vec<String> = Vec::new();
    let started = Instant::now();
    while started.elapsed() < timeout {
        seen.extend(drain_logs(runtime, process_id));
        if let Some(found) = seen.iter().find(|message| message.starts_with(prefix)) {
            return found.clone();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {prefix:?} from process {process_id}; got {seen:?}");
}

fn guest_name(mode: u64) -> String {
    format!("discovery-probe-{mode}")
}

fn probe_descriptor(route: &str, mode: u64) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: guest_name(mode),
        module_id: format!("discovery-probe-{mode}-module"),
        module_bytes: discovery_probe_wasm(),
        entrypoint: "discovery_probe".to_string(),
        arguments: vec![
            SystemGuestArg::Pointer(route.as_bytes().to_vec()),
            SystemGuestArg::Integer(mode),
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
        ],
        tenant: Some("probe".to_string()),
        discovery_service: false,
        handlers: Vec::new(),
    }
}

// scenario: guest-context/boot-time-service-convergence/consumer-converges-on-a-service-that-publishes-later
// scenario: guest-context/boot-time-service-convergence/provider-never-appears
// scenario: guest-context/boot-time-service-convergence/a-genuine-miss-is-not-retried-on-the-request-path
// scenario: guest-context/context-lookup-convenience-method/guest-resolves-an-unknown-uri-via-lookup
// scenario: guest-context/context-register-convenience-method/guest-registers-a-uri-via-context
/// A consumer started before its provider converges on the later registration.
#[test]
#[ignore = "requires the discovery and discovery-probe guests built for wasm32-unknown-unknown"]
fn resolve_ready_converges_on_a_late_route_and_distinguishes_misses() {
    let runtime = Runtime::default();
    let (ready_id, timeout_id, miss_id) = bootstrap(&runtime);

    // The request path answers immediately: a genuine miss is NotFound, not a
    // wait for the readiness bound.
    let miss = expect_line(&runtime, miss_id, "miss: notfound", CONVERGENCE_BOUND);
    let elapsed = miss
        .split_whitespace()
        .next_back()
        .and_then(|token| token.strip_suffix("ms"))
        .and_then(|ms| ms.parse::<u128>().ok())
        .expect("miss line carries an elapsed millisecond count");
    assert!(
        elapsed < 5_000,
        "an absent route must resolve immediately on the request path, took {elapsed}ms"
    );

    // A route nobody publishes yields the typed readiness error at its bound.
    let timeout_line = expect_line(&runtime, timeout_id, "ready: timeout", CONVERGENCE_BOUND);
    assert!(
        timeout_line.contains(ABSENT_ROUTE),
        "timeout identifies the absent route: {timeout_line:?}"
    );

    // The provider appears after the consumer has already parked.
    let provider = spawn_provider(&runtime, LATE_ROUTE);
    expect_line(
        &runtime,
        provider,
        "provider: registered",
        CONVERGENCE_BOUND,
    );

    let found = expect_line(&runtime, ready_id, "ready: found", CONVERGENCE_BOUND);
    assert!(
        found.contains(LATE_ROUTE),
        "readiness converged on the late route: {found:?}"
    );
}

/// Spawns a provider for `route`, wiring in the discovery listener handle that
/// `bootstrap_system_guests` would otherwise have supplied.
#[expect(clippy::panic, reason = "a failed provider spawn is a test failure")]
fn spawn_provider(runtime: &Runtime, route: &str) -> u64 {
    let listener = runtime
        .discovery_listener_shared_id()
        .expect("discovery listener shared id");
    let mut descriptor = probe_descriptor(route, ROLE_PROVIDER);
    descriptor
        .arguments
        .insert(0, SystemGuestArg::Integer(listener));
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

// --- Spec traceability (scripts/check-traceability.sh) -------------------
