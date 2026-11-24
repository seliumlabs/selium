//! Attachability precedes route publication (`deterministic-guest-io`).
//!
//! Drives `Context::serve` against the real `selium-discovery` guest while the
//! host refuses the attachability declaration (armed through the runtime's test
//! fault seam). A resource whose declaration fails must publish neither its
//! canonical typed route nor its named service route, so no consumer can
//! resolve a target it could not attach.
//!
//! `#[ignore]`d by default because it requires the guests built for
//! `wasm32-unknown-unknown`:
//!
//! ```sh
//! cargo build --target wasm32-unknown-unknown -p selium-discovery -p selium-discovery-probe
//! cargo test -p selium-runtime --test serve_attachability_order -- --ignored
//! ```

use std::time::{Duration, Instant};

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceIdentity, ResourceSelector};
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestArg, SystemGuestDescriptor};
use selium_service::FlatMsg;

mod common;

/// Reviewed bound on the probe's log lines (the probe runs in the interpreter).
const PROBE_BOUND: Duration = Duration::from_secs(30);
/// The probe role that serves with a refused declaration.
const ROLE_SERVE_DECLARATION_FAILS: u64 = 12;

/// Collects every log line until `count` lines with `prefix` have been seen.
#[expect(
    clippy::panic,
    reason = "a missing log line within the bound is a test failure"
)]
fn collect_after(
    runtime: &Runtime,
    process_id: u64,
    prefix: &str,
    count: usize,
    timeout: Duration,
) -> Vec<String> {
    let mut all: Vec<String> = Vec::new();
    let started = Instant::now();
    while started.elapsed() < timeout {
        all.extend(drain_logs(runtime, process_id));
        let matched = all.iter().filter(|line| line.starts_with(prefix)).count();
        if matched >= count {
            return all;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "timed out waiting for {count} {prefix:?} lines from process {process_id}; \
         logs: {all:?}"
    );
}

// scenario: guest-context/attachability-precedes-route-publication/attachability-declaration-fails
#[test]
#[ignore = "requires the discovery and discovery-probe guests built for wasm32-unknown-unknown"]
fn declaration_failure_publishes_neither_route() {
    let runtime = Runtime::default();

    // Discovery declares its own route table attachable during boot; arm the
    // fault only after that has completed so the fixture itself stays healthy.
    runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![discovery_descriptor(discovery_wasm())],
            state_dir: None,
        })
        .expect("bootstrap discovery");
    runtime.inject_declare_attachable_failure(true);

    let probe = spawn_probe(&runtime, ROLE_SERVE_DECLARATION_FAILS);

    // Wait for the probe to finish reporting (it stays resident afterwards),
    // accumulating every line so earlier ones are not discarded.
    let lines = collect_after(&runtime, probe, "serve-decl: route", 2, PROBE_BOUND);

    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("serve-decl: refused")),
        "the declaration must be refused: {lines:?}"
    );
    let routes: Vec<&String> = lines
        .iter()
        .filter(|line| line.starts_with("serve-decl: route"))
        .collect();
    assert_eq!(
        routes.len(),
        2,
        "the probe reports the canonical and named routes: {lines:?}"
    );
    for line in routes {
        assert!(
            line.ends_with("absent"),
            "a failed declaration must publish nothing: {line:?}"
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

/// Spawns the probe with the discovery listener handle bootstrap would have
/// supplied, plus the explicit queue grant the forwarded handle needs.
#[expect(clippy::panic, reason = "a failed probe spawn is a test failure")]
fn spawn_probe(runtime: &Runtime, mode: u64) -> u64 {
    let listener = runtime
        .discovery_listener_shared_id()
        .expect("discovery listener shared id");
    let descriptor = SystemGuestDescriptor {
        name: format!("discovery-probe-{mode}"),
        module_id: format!("discovery-probe-{mode}-module"),
        module_bytes: discovery_probe_wasm(),
        entrypoint: "discovery_probe".to_string(),
        arguments: vec![
            SystemGuestArg::Integer(listener),
            // The probe's entrypoint still takes the route pointer argument;
            // this role ignores its contents.
            SystemGuestArg::Pointer(b"sel://probe/decl".to_vec()),
            SystemGuestArg::Integer(mode),
        ],
        grants: vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![
                    ResourceSelector::ResourceClass(ResourceClass::HostQueue),
                    ResourceSelector::ExplicitResource(ResourceIdentity::Shared(listener)),
                ],
            ),
        ],
        tenant: Some("probe".to_string()),
        discovery_service: false,
        handlers: Vec::new(),
    };
    runtime
        .spawn_system_guest(descriptor)
        .unwrap_or_else(|error| {
            panic!(
                "spawn serve-declaration probe: {error}; activity: {:#?}",
                runtime.activity_log()
            )
        })
        .process_id
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
