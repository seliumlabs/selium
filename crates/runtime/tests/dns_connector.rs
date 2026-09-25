//! DNS connector substrate tests.
//!
//! Verifies the runtime substrate the DNS connector runs on: a raw UDP
//! socket is capability-gated behind `Network + UdpSocket`, and the
//! connector's channel is attach-gated so a guest without a grant for the
//! well-known channel cannot resolve. The connector's correlation,
//! unknown-txid dropping, and typed-outcome mapping are unit-tested in
//! `selium-connector-dns`; the wire codec is tested in `selium-proto-dns`.
//!
//! ```sh
//! cargo test -p selium-runtime --test dns_connector
//! ```

use selium_abi::{
    Capability, CapabilityGrant, CompletionState, HostcallOutput, HostcallRequest, ProcessId,
    RegionProt, ResourceClass, ResourceSelector,
};
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};

/// Task 4.1 substrate: the connector's raw UDP socket is capability-gated by
/// `Network + UdpSocket`; a guest without that grant cannot bind one.
#[test]
fn connector_udp_socket_requires_network_grant() {
    let runtime = Runtime::default();

    // With the grant, binding an ephemeral UDP socket succeeds.
    let granted = spawn_guest(
        &runtime,
        "dns-connector-granted",
        vec![CapabilityGrant::new(
            Capability::Network,
            vec![ResourceSelector::ResourceClass(ResourceClass::UdpSocket)],
        )],
    );
    let (status, op_id) = runtime.begin_hostcall(
        granted,
        HostcallRequest::UdpBind {
            address: "127.0.0.1:0".to_string(),
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    let CompletionState::Ready(HostcallOutput::SharedRegion(descriptor)) =
        runtime.poll_hostcall(granted, op_id)
    else {
        panic!("granted guest should bind a UDP socket");
    };
    assert!(descriptor.shared_id > 0);

    // Without the grant, binding is denied.
    let ungranted = spawn_guest(&runtime, "no-network-guest", Vec::new());
    let (status, deny_op) = runtime.begin_hostcall(
        ungranted,
        HostcallRequest::UdpBind {
            address: "127.0.0.1:0".to_string(),
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "guest without Network + UdpSocket grant must be denied UdpBind"
    );
    assert!(matches!(
        runtime.poll_hostcall(ungranted, deny_op),
        CompletionState::Failed(_)
    ));
}

fn module_with_entrypoint(entrypoint: &str) -> Vec<u8> {
    wat::parse_str(format!(
        "(module (memory 1) (func (export \"{entrypoint}\")))"
    ))
    .expect("compile wat")
}

/// A guest must be able to attach the ring region of a socket it opened:
/// `UdpBind`/`TcpConnect` claim the region under their network class, and the
/// fix also claims it under `SharedRegion`, which is what `AttachRegion`
/// authorisation checks.
#[test]
fn network_socket_regions_are_attachable_by_owner() {
    let runtime = Runtime::default();
    let guest = spawn_guest(
        &runtime,
        "udp-owner",
        vec![
            CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::UdpSocket)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
        ],
    );

    let (_, op_id) = runtime.begin_hostcall(
        guest,
        HostcallRequest::UdpBind {
            address: "127.0.0.1:0".to_string(),
        },
    );
    let CompletionState::Ready(HostcallOutput::SharedRegion(descriptor)) =
        runtime.poll_hostcall(guest, op_id)
    else {
        panic!("expected UDP socket region");
    };

    // The owner must be able to attach its own socket region.
    let (status, attach_op) = runtime.begin_hostcall(
        guest,
        HostcallRequest::AttachRegion {
            region_id: descriptor.shared_id,
            reader_slot: None,
            prot: RegionProt::ReadWrite,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "socket region owner must be able to attach its own region"
    );
    assert!(matches!(
        runtime.poll_hostcall(guest, attach_op),
        CompletionState::Ready(_)
    ));
}

fn spawn_guest(runtime: &Runtime, name: &str, grants: Vec<CapabilityGrant>) -> ProcessId {
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![SystemGuestDescriptor {
                name: name.to_string(),
                module_id: format!("{name}-module"),
                module_bytes: module_with_entrypoint("boot"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants,
                dependencies: Vec::new(),
                readiness: ReadinessCondition::Immediate,
                tenant: None,
                serving_role: None,
                handlers: Vec::new(),
            }],
            domain_table: Vec::new(),
        })
        .expect("bootstrap guest");
    report
        .guests
        .first()
        .expect("bootstrap report contains the requested guest")
        .process_id
}

/// Task 4.3: a guest without a grant for the connector's channel cannot
/// attach to it (and therefore cannot resolve).
#[test]
fn ungranted_guest_cannot_attach_connector_channel() {
    let runtime = Runtime::default();

    // The connector owns a host queue for the well-known resolve channel.
    let connector = spawn_guest(
        &runtime,
        "dns-connector",
        vec![
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
        ],
    );

    let intruder = spawn_guest(&runtime, "resolver-without-grant", Vec::new());

    // The connector creates its listener queue.
    let (_, op_id) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(listener)) =
        runtime.poll_hostcall(connector, op_id)
    else {
        panic!("connector should create a host queue");
    };

    // The intruder has no resolve basis and no grant: attach is denied, so
    // no DNS traffic can ever leave the host on its behalf.
    let (status, deny_op) = runtime.begin_hostcall(
        intruder,
        HostcallRequest::HostQueueAttach {
            shared_id: listener.shared_id,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "guest without a grant for the connector channel must be denied attach"
    );
    assert!(matches!(
        runtime.poll_hostcall(intruder, deny_op),
        CompletionState::Failed(_)
    ));
}
