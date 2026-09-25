//! Control-plane bootstrap integration test.
//!
//! Deploys the real `selium-discovery` and `selium-control-plane` WASM guests
//! on a single host and asserts the control-plane slice end-to-end: bootstrap
//! with discovery wiring, self-registration of the root `control` serving
//! route, and readiness signalling (which fires only after the route is
//! registered).
//!
//! This test is `#[ignore]`d by default because it requires both WASM guests
//! to be built for `wasm32-unknown-unknown` first:
//!
//! ```sh
//! cargo build --target wasm32-unknown-unknown -p selium-discovery -p selium-control-plane
//! cargo test -p selium-runtime --test control_plane -- --ignored
//! ```

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};

mod common;

#[test]
#[ignore = "requires the control-plane and discovery guests built for wasm32-unknown-unknown"]
fn control_plane_bootstrap_slice_end_to_end() {
    let discovery_wasm = discovery_wasm();
    let control_plane_wasm = control_plane_wasm();

    let runtime = Runtime::default();
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            domain_table: Vec::new(),
            system_guests: vec![
                discovery_descriptor(discovery_wasm),
                control_plane_descriptor(control_plane_wasm),
            ],
        })
        .expect("bootstrap system guests");

    // Discovery must be ready before the control plane (declared dependency).
    assert_eq!(report.guests.len(), 2);
    let discovery_guest = report
        .guests
        .iter()
        .find(|guest| guest.name == "discovery")
        .expect("discovery guest in report");
    let control_guest = report
        .guests
        .iter()
        .find(|guest| guest.name == "control-plane")
        .expect("control-plane guest in report");

    // Both serving guests reached readiness.
    let activity = runtime.activity_log();
    assert!(
        activity
            .iter()
            .any(|event| event.process_id == Some(discovery_guest.process_id)
                && event.message.contains("guest ready")),
        "expected discovery GuestReady, got: {activity:?}"
    );
    assert!(
        activity
            .iter()
            .any(|event| event.process_id == Some(control_guest.process_id)
                && event.message.contains("guest ready")),
        "expected control-plane GuestReady, got: {activity:?}"
    );

    // The control route registered: readiness is signalled only after
    // `Context::serve` registers `sel:///control` in discovery, and
    // discovery records the registration with the runtime before replying.
    assert!(
        runtime.has_registration(control_guest.process_id, "sel:///control"),
        "expected the control route to be registered and observable"
    );

    // Cleanup.
    runtime
        .stop_process(control_guest.process_id)
        .expect("stop control-plane");
    runtime
        .stop_process(discovery_guest.process_id)
        .expect("stop discovery");
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
                vec![ResourceSelector::ResourceClass(ResourceClass::DurableLog)],
            ),
            CapabilityGrant::new(
                Capability::Storage,
                vec![ResourceSelector::ResourceClass(ResourceClass::BlobStore)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(Capability::SystemRegistration, Vec::new()),
        ],
        dependencies: vec!["discovery".to_string()],
        readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

fn control_plane_wasm() -> Vec<u8> {
    common::read_guest_wasm_debug("selium-control-plane", "selium_control_plane.wasm")
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
        ],
        dependencies: Vec::new(),
        readiness: ReadinessCondition::ActivityLogContains("guest ready".to_string()),
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

fn discovery_wasm() -> Vec<u8> {
    common::read_guest_wasm_debug("selium-discovery", "selium_discovery.wasm")
}
