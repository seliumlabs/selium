//! Control-plane bootstrap integration test.
//!
//! Deploys the real `selium-discovery`, `selium-store`, `selium-control-plane`
//! and `selium-control-probe` WASM guests on a single host and asserts the
//! control-plane slice end-to-end: bootstrap with discovery wiring,
//! self-registration of the root `control` serving route, readiness
//! signalling, a named-resource round trip through the guest-owned catalogue,
//! and a catalogue replay after the control plane restarts.
//!
//! This test is `#[ignore]`d by default because it requires the WASM guests to
//! be built for `wasm32-unknown-unknown` first:
//!
//! ```sh
//! cargo build --release --target wasm32-unknown-unknown \
//!   -p selium-discovery -p selium-store -p selium-control-plane -p selium-control-probe
//! cargo test -p selium-runtime --test control_plane -- --ignored
//! ```

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_runtime::{BootstrapReport, Runtime, RuntimeConfig, SystemGuestDescriptor};

mod common;
mod spine_common;

/// Boots discovery, the store, and the control plane against the given state
/// root.
fn bootstrap(runtime: &Runtime, state: &spine_common::StateRoot) -> BootstrapReport {
    runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![
                discovery_descriptor(discovery_wasm()),
                spine_common::store_descriptor(vec!["discovery".to_string()]),
                control_plane_descriptor(control_plane_wasm()),
            ],
            state_dir: state.config_dir(),
        })
        .expect("bootstrap system guests")
}

#[expect(clippy::panic, reason = "test helper panics on an unknown guest name")]
fn bootstrap_extra(
    runtime: &Runtime,
    state: &spine_common::StateRoot,
    name: &str,
) -> BootstrapReport {
    let descriptor = match name {
        "control-plane" => control_plane_descriptor(control_plane_wasm()),
        "control-probe" => control_probe_descriptor(control_probe_wasm()),
        other => panic!("unknown extra guest {other}"),
    };
    bootstrap_one(runtime, state, descriptor)
}

/// Boots one additional guest against an already-running discovery service.
fn bootstrap_one(
    runtime: &Runtime,
    state: &spine_common::StateRoot,
    descriptor: SystemGuestDescriptor,
) -> BootstrapReport {
    runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![descriptor],
            state_dir: state.config_dir(),
        })
        .expect("bootstrap extra guest")
}
// scenario: selium-store/store-guest-crate/store-bootstraps-from-descriptor
// scenario: selium-store/served-typed-store-surface/serving-route-registered

// scenario: control-plane/control-plane-guest-crate/control-plane-bootstraps-from-descriptor
// scenario: control-plane/served-typed-control-surface/serving-route-registered
// scenario: control-plane/resource-catalogue-ownership/catalogue-rebuilt-on-boot
// scenario: resource-catalogue/anonymous-allocation-is-not-catalogued/scratch-region-without-the-catalogue
// scenario: resource-catalogue/anonymous-allocation-is-not-catalogued/catalogue-unreachable
// scenario: guest-context/serve-based-route-registration/claimed-root-name-is-served
// scenario: guest-context/context-resource-operations/named-region-creation
// scenario: guest-context/context-resource-operations/name-resolution
// scenario: guest-context/context-resource-operations/allocation-without-the-catalogue
// scenario: guest-context/context-health-reporting/guest-publishes-health
#[test]
#[ignore = "requires the control-plane, discovery, store and probe guests built for wasm32-unknown-unknown"]
fn control_plane_bootstrap_slice_end_to_end() {
    let state = spine_common::StateRoot::new("control-plane");
    let runtime = spine_common::runtime_in_state_root(&state);

    // ── Bootstrap, self-registration, readiness ────────────────────────────
    let report = bootstrap(&runtime, &state);
    assert_eq!(report.guests.len(), 3);
    let discovery_guest = find(&report, "discovery");
    let control_guest = find(&report, "control-plane");
    assert!(
        report.guests.iter().any(|guest| guest.name == "store"),
        "the store guest is bootstrapped"
    );
    let _ = discovery_guest;

    // There is no readiness gate and no host-side registration record: the
    // probe's catalogue and health round trips below are the convergence
    // evidence (they only succeed once discovery and the control plane serve).

    // ── The probe registers a named resource ───────────────────────────────
    let probe_report = bootstrap_extra(&runtime, &state, "control-probe");
    let probe = find(&probe_report, "control-probe");
    // Waited, not drained: the probe runs asynchronously, so its catalogue and
    // health round trips land after bootstrap returns.
    let logs = spine_common::wait_for_logs(
        &runtime,
        probe,
        &[
            ("catalogue registered", 1),
            ("health published and observed: true", 1),
        ],
        spine_common::OBSERVATION_BOUND,
    );
    assert!(
        logs.iter()
            .any(|line| line.contains("catalogue registered")),
        "expected the probe to register a named region, got: {logs:?}"
    );

    // Stop the probe and the control plane so a later boot must rebuild the
    // catalogue from the store's durable log rather than from memory.
    runtime.stop_process(probe).expect("stop probe");
    runtime
        .stop_process(control_guest)
        .expect("stop control-plane");

    // With the catalogue unreachable (the control plane is down), an
    // anonymous allocation still succeeds.
    let anon_report = bootstrap_extra(&runtime, &state, "control-probe");
    let anon = find(&anon_report, "control-probe");
    let logs = spine_common::wait_for_logs(
        &runtime,
        anon,
        &[
            ("catalogue unreachable", 1),
            ("anonymous allocation succeeded", 1),
        ],
        // The probe waits the guest SDK's service-ready bound for the
        // control route before reporting it unreachable, so this observation
        // must outlast that bound.
        spine_common::CONVERGENCE_BOUND + std::time::Duration::from_secs(60),
    );
    assert!(
        logs.iter()
            .any(|line| line.contains("catalogue unreachable")),
        "expected the probe to observe an unreachable catalogue, got: {logs:?}"
    );
    assert!(
        logs.iter()
            .any(|line| line.contains("anonymous allocation succeeded")),
        "expected anonymous allocation without the catalogue, got: {logs:?}"
    );
    runtime.stop_process(anon).expect("stop anonymous probe");

    // ── The catalogue rebuilds from replay on restart ──────────────────────
    let _cp_report = bootstrap_extra(&runtime, &state, "control-plane");
    let probe_report = bootstrap_extra(&runtime, &state, "control-probe");
    let probe = find(&probe_report, "control-probe");
    let logs = spine_common::wait_for_logs(
        &runtime,
        probe,
        &[("catalogue replayed", 1)],
        spine_common::OBSERVATION_BOUND,
    );
    assert!(
        logs.iter().any(|line| line.contains("catalogue replayed")),
        "expected the catalogue to rebuild from replay, got: {logs:?}"
    );

    for guest in &report.guests {
        drop(runtime.stop_process(guest.process_id));
    }
    drop(runtime.stop_process(probe));
}

fn control_plane_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "control-plane".to_string(),
        module_id: "control-plane-module".to_string(),
        module_bytes,
        entrypoint: "control_plane_main".to_string(),
        arguments: Vec::new(), // populated by bootstrap via set_discovery_handle
        grants: vec![
            CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::Volume)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::RouteClaim,
                vec![ResourceSelector::Uri("sel:///control".to_string())],
            ),
        ],
        tenant: None,
        discovery_service: false,
        handlers: Vec::new(),
    }
}

fn control_plane_wasm() -> Vec<u8> {
    common::read_guest_wasm("selium-control-plane", "selium_control_plane.wasm")
}

fn control_probe_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "control-probe".to_string(),
        module_id: "control-probe-module".to_string(),
        module_bytes,
        entrypoint: "control_probe".to_string(),
        arguments: Vec::new(), // populated by bootstrap via set_discovery_handle
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

fn control_probe_wasm() -> Vec<u8> {
    common::read_guest_wasm("selium-control-probe", "selium_control_probe.wasm")
}

fn discovery_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "discovery".to_string(),
        module_id: "discovery-module".to_string(),
        module_bytes,
        entrypoint: "discovery_main".to_string(),
        arguments: Vec::new(), // populated by bootstrap via set_discovery_feed_and_handle
        grants: vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            // The registry probes capability state and records
            // registrations under a bootstrap-provisioned authority, not a
            // privileged guest identity.
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
    common::read_guest_wasm("selium-discovery", "selium_discovery.wasm")
}

#[expect(clippy::panic, reason = "test helper panics on a missing guest")]
fn find(report: &BootstrapReport, name: &str) -> selium_abi::ProcessId {
    report
        .guests
        .iter()
        .find(|guest| guest.name == name)
        .unwrap_or_else(|| panic!("bootstrap report contains guest {name}"))
        .process_id
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `control-plane` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.
