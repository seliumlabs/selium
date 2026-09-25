//! Identity bootstrap-order substrate test (task 7.1).
//!
//! Verifies the wiring the single-host spine needs before the golden-path
//! integration test:
//!
//! - the identity guest's grant set (`selium_identity::identity_grants()`,
//!   including `MintCertificate` plus `SystemRegistration` for its
//!   root-namespace routes) is admissible at bootstrap;
//! - the host keyring initialises and is installed before identity runs;
//! - the connector and bridge declare a dependency on identity, and the
//!   runtime honours that dependency order even when the descriptor vector is
//!   not already sorted.
//!
//! These tests use stub modules (the real guests need the wasm32 build); the
//! wasm spine itself is exercised by the golden-path test.

use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_identity::identity_grants;
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};

/// 7.1: the connector and bridge depend on identity, and the runtime boots
/// identity first even though the descriptor vector lists it last.
#[test]
fn connector_and_bridge_boot_after_identity() {
    let runtime = Runtime::default();
    runtime.generate_keyring().expect("generate keyring");

    let identity = stub_descriptor("identity", identity_grants(), Vec::new());
    let connector = stub_descriptor(
        "quic-connector",
        vec![CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        )],
        vec!["identity".to_string()],
    );
    let bridge = stub_descriptor(
        "bridge-server",
        vec![CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        )],
        vec!["identity".to_string()],
    );

    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            domain_table: Vec::new(),
            // Identity is listed last: dependency order must still boot it
            // before the guests that depend on it.
            system_guests: vec![connector, bridge, identity],
        })
        .expect("bootstrap identity spine");

    let names: Vec<&str> = report
        .guests
        .iter()
        .map(|guest| guest.name.as_str())
        .collect();
    let identity_pos = names
        .iter()
        .position(|name| *name == "identity")
        .expect("identity booted");
    let connector_pos = names
        .iter()
        .position(|name| *name == "quic-connector")
        .expect("connector booted");
    let bridge_pos = names
        .iter()
        .position(|name| *name == "bridge-server")
        .expect("bridge booted");
    assert!(
        identity_pos < connector_pos,
        "identity boots before the connector"
    );
    assert!(
        identity_pos < bridge_pos,
        "identity boots before the bridge"
    );
}

/// 7.1: the identity guest's grant set is admissible at bootstrap, carrying
/// the `MintCertificate` capability (bootstrap-provisioned only) plus the
/// system-registration grant for its root-namespace routes.
#[test]
fn identity_descriptor_grants_are_bootstrap_admissible() {
    let runtime = Runtime::default();
    runtime.generate_keyring().expect("generate keyring");

    let guest = runtime
        .spawn_system_guest(SystemGuestDescriptor {
            name: "identity".to_string(),
            module_id: "identity-module".to_string(),
            module_bytes: module_with_entrypoint("boot"),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants: identity_grants(),
            dependencies: Vec::new(),
            readiness: ReadinessCondition::Immediate,
            tenant: None,
            serving_role: None,
            handlers: Vec::new(),
        })
        .expect("identity descriptor must be admissible at bootstrap");

    let authority = runtime
        .restore_process_authority(guest.process_id)
        .expect("identity authority");
    assert!(
        authority
            .grants
            .iter()
            .any(|grant| grant.capability == Capability::MintCertificate)
    );
    assert!(
        authority
            .grants
            .iter()
            .any(|grant| grant.capability == Capability::SystemRegistration)
    );
}

fn module_with_entrypoint(entrypoint: &str) -> Vec<u8> {
    wat::parse_str(format!("(module (func (export \"{entrypoint}\")))")).expect("compile wat")
}

fn stub_descriptor(
    name: &str,
    grants: Vec<CapabilityGrant>,
    dependencies: Vec<String>,
) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: name.to_string(),
        module_id: format!("{name}-module"),
        module_bytes: module_with_entrypoint("boot"),
        entrypoint: "boot".to_string(),
        arguments: Vec::new(),
        grants,
        dependencies,
        readiness: ReadinessCondition::Immediate,
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}
