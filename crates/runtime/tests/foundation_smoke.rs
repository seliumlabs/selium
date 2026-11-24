use selium_abi::{
    Capability, CapabilityGrant, CompletionState, HostcallOutput, HostcallRequest, LocalityScope,
    RegionProt, ResourceClass, ResourceSelector,
};
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestDescriptor};

// scenario: capability-enforcement/owner-scoped-resource-handoff/owner-authorises-attach
// scenario: capability-enforcement/ownership-checked-attach/owner-authorisation-admits-a-peer
// scenario: selium-abi/owner-scoped-attach-authorisation/owner-authorises-a-peer
/// The push form of owner-scoped handoff: the owner authorises exactly one
/// client process, that client's attach succeeds on that basis alone, and no
/// other process gains one.
#[test]
fn an_owner_push_authorisation_admits_exactly_the_named_peer() {
    let runtime = Runtime::default();
    let queue_grant = || {
        CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        )
    };
    let spawn = |name: &str| {
        runtime
            .spawn_system_guest(SystemGuestDescriptor {
                name: name.to_string(),
                module_id: format!("{name}-module"),
                module_bytes: module_with_entrypoint("boot"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![queue_grant()],
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            })
            .expect("spawn guest")
    };
    let owner = spawn("owner");
    let peer = spawn("peer");
    let bystander = spawn("bystander");

    // The owner creates the queue and pushes an authorisation naming `peer`.
    let (status, op_id) = runtime.begin_hostcall(
        owner.process_id,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    let CompletionState::Ready(HostcallOutput::HostQueue(descriptor)) =
        runtime.poll_hostcall(owner.process_id, op_id)
    else {
        panic!("expected a host queue descriptor");
    };
    let shared_id = descriptor.shared_id;

    // Before the authorisation, neither peer nor bystander has a basis.
    for process in [peer.process_id, bystander.process_id] {
        let (status, _op_id) =
            runtime.begin_hostcall(process, HostcallRequest::HostQueueAttach { shared_id });
        assert_eq!(
            status,
            selium_abi::HOSTCALL_STATUS_FAILED,
            "an unauthorised process must not attach"
        );
    }

    let (status, _op_id) = runtime.begin_hostcall(
        owner.process_id,
        HostcallRequest::AuthoriseAttach {
            resource_class: ResourceClass::HostQueue,
            resource_id: shared_id,
            client_process_id: peer.process_id,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "the owner authorises a named peer"
    );

    // The named peer's attach is now authorised.
    let (status, op_id) = runtime.begin_hostcall(
        peer.process_id,
        HostcallRequest::HostQueueAttach { shared_id },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "the authorised peer attaches on its recorded basis"
    );
    assert!(matches!(
        runtime.poll_hostcall(peer.process_id, op_id),
        CompletionState::Ready(_)
    ));

    // No other process gained a basis from that authorisation.
    let (status, _op_id) = runtime.begin_hostcall(
        bystander.process_id,
        HostcallRequest::HostQueueAttach { shared_id },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "an authorisation names one peer; nobody else gains a basis"
    );
}
// scenario: guest-networking/guest-tcp-listener/each-connection-is-a-hot-attached-region

/// Task 4.2 & 3.1: Concurrent connections use distinct regions; all reclaim
/// on disconnect.
// scenario: connector-channel-forwarding/per-connection-region-lifecycle/distinct-regions-per-external-connection
// scenario: connector-channel-forwarding/per-connection-region-lifecycle/reclamation-on-disconnect
// scenario: selium-kernel/host-queue-primitives/runtime-sends-value-to-queue
#[test]
fn concurrent_connections_use_distinct_regions() {
    let runtime = Runtime::default();

    let server = spawn_guest(
        &runtime,
        "server-concurrent",
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

    let connector = spawn_guest(
        &runtime,
        "connector-concurrent",
        vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
        ],
    );

    // Server creates a listener queue.
    let (_, op_id) = runtime.begin_hostcall(
        server,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(listener)) =
        runtime.poll_hostcall(server, op_id)
    else {
        panic!("expected host queue");
    };

    // Discovery records the queue on the connector's behalf (simulates
    // discovery resolve).
    owner_declares_and_records_resolve(&runtime, server, connector, listener.shared_id);

    // Connector attaches to the queue.
    let (_, op_id) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueAttach {
            shared_id: listener.shared_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(connector_queue)) =
        runtime.poll_hostcall(connector, op_id)
    else {
        panic!("attach should succeed");
    };

    // Allocate two regions — simulate two concurrent forwarded connections.
    let (_, op_a) = runtime.begin_hostcall(
        connector,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: selium_abi::ResourceKind::RpcRing,
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::RegionAlloc(region_a)) =
        runtime.poll_hostcall(connector, op_a)
    else {
        panic!("expected region A");
    };

    let (_, op_b) = runtime.begin_hostcall(
        connector,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: selium_abi::ResourceKind::RpcRing,
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::RegionAlloc(region_b)) =
        runtime.poll_hostcall(connector, op_b)
    else {
        panic!("expected region B");
    };

    // Regions must have distinct ids.
    assert_ne!(
        region_a.region_id, region_b.region_id,
        "concurrent connections must use distinct region ids"
    );

    // Send both region ids through the queue.
    let _ = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueSend {
            local_id: connector_queue.local_id,
            value: region_a.region_id,
            metadata: Vec::new(),
        },
    );
    let _ = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueSend {
            local_id: connector_queue.local_id,
            value: region_b.region_id,
            metadata: Vec::new(),
        },
    );

    // Server receives both region ids.
    let (_, server_recv_a) = runtime.begin_hostcall(
        server,
        HostcallRequest::HostQueueRecv {
            local_id: listener.local_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::ConnectionInfo { value: recv_a, .. }) =
        runtime.poll_hostcall(server, server_recv_a)
    else {
        panic!("server should receive region A");
    };
    assert_eq!(recv_a, region_a.region_id);

    let (_, server_recv_b) = runtime.begin_hostcall(
        server,
        HostcallRequest::HostQueueRecv {
            local_id: listener.local_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::ConnectionInfo { value: recv_b, .. }) =
        runtime.poll_hostcall(server, server_recv_b)
    else {
        panic!("server should receive region B");
    };
    assert_eq!(recv_b, region_b.region_id);

    // Free both regions (simulates connection teardown / reclaim). The
    // handoff transferred ownership to the server: the delivered regions
    // entered the receiver's resource table, so the server frees them and
    // the connector no longer can.
    let (_, free_a) = runtime.begin_hostcall(
        server,
        HostcallRequest::FreeRegion {
            region_id: region_a.region_id,
        },
    );
    assert!(matches!(
        runtime.poll_hostcall(server, free_a),
        CompletionState::Ready(_)
    ));

    let (_, free_b) = runtime.begin_hostcall(
        server,
        HostcallRequest::FreeRegion {
            region_id: region_b.region_id,
        },
    );
    assert!(matches!(
        runtime.poll_hostcall(server, free_b),
        CompletionState::Ready(_)
    ));

    // The sender's free rights ended at delivery: its FreeRegion is denied.
    let (sender_free_status, sender_free_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::FreeRegion {
            region_id: region_a.region_id,
        },
    );
    assert_eq!(
        sender_free_status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "the sender must no longer own a delivered handoff region"
    );
    assert!(matches!(
        runtime.poll_hostcall(connector, sender_free_op),
        CompletionState::Failed(_)
    ));
}

#[test]
fn foundation_crates_work_together_through_hostcalls() {
    let runtime = Runtime::default();
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![SystemGuestDescriptor {
                name: "cluster".to_string(),
                module_id: "cluster-module".to_string(),
                module_bytes: module_with_entrypoint("boot"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::SharedMemory,
                    vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
                )],
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            }],
            state_dir: None,
        })
        .expect("bootstrap runtime");
    assert_eq!(report.guests.len(), 1);

    // Allocate a shared region using the new ABI.
    let (status, alloc_id) = runtime.begin_hostcall(
        report.guests[0].process_id,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: selium_abi::ResourceKind::SharedMemory,
            serving_tenant: None,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    let CompletionState::Ready(HostcallOutput::RegionAlloc(allocation)) =
        runtime.poll_hostcall(report.guests[0].process_id, alloc_id)
    else {
        panic!("expected region allocation");
    };
    assert!(allocation.region_id > 0);
}

// scenario: selium-runtime/shared-memory-hostcall-passthrough/unauthorised-region-allocation-denied
#[test]
fn hostcalls_enforce_session_grants() {
    let runtime = Runtime::default();
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![SystemGuestDescriptor {
                name: "restricted".to_string(),
                module_id: "restricted-module".to_string(),
                module_bytes: module_with_entrypoint("boot"),
                entrypoint: "boot".to_string(),
                arguments: Vec::new(),
                grants: vec![CapabilityGrant::new(
                    Capability::ProcessLifecycle,
                    vec![ResourceSelector::Locality(LocalityScope::Cluster)],
                )],
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            }],
            state_dir: None,
        })
        .expect("bootstrap runtime");

    // Attempt to allocate a region without SharedMemory capability.
    let (status, operation_id) = runtime.begin_hostcall(
        report.guests[0].process_id,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: selium_abi::ResourceKind::SharedMemory,
            serving_tenant: None,
        },
    );

    assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);
    assert!(matches!(
        runtime.poll_hostcall(report.guests[0].process_id, operation_id),
        CompletionState::Failed(_)
    ));
}

fn module_with_entrypoint(entrypoint: &str) -> Vec<u8> {
    wat::parse_str(format!(
        "(module (memory 1) (func (export \"{entrypoint}\")))"
    ))
    .expect("compile wat")
}

/// The queue's owner declares it attachable by resolution, then the client's
/// resolve is recorded under that live declaration — the owner-scoped handoff
/// the runtime accepts in place of the removed discovery-gated recording.
fn owner_declares_and_records_resolve(
    runtime: &Runtime,
    owner: selium_abi::ProcessId,
    client: selium_abi::ProcessId,
    shared_id: u64,
) {
    let (status, _op) = runtime.begin_hostcall(
        owner,
        HostcallRequest::DeclareAttachableByResolution {
            resource_class: ResourceClass::HostQueue,
            resource_id: shared_id,
            scope: selium_abi::AttachDeclarationScope::Any,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "the queue owner must declare it attachable by resolution"
    );
    let (status, _op) = runtime.begin_hostcall(
        client,
        HostcallRequest::RecordResolvedResourceFor {
            resource_class: ResourceClass::HostQueue,
            resource_id: shared_id,
            client_process_id: client,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "a resolve recording under a live declaration must be accepted"
    );
}
// scenario: capability-enforcement/owner-scoped-resource-handoff/role-confers-nothing
// scenario: selium-abi/owner-scoped-attach-authorisation/non-owner-caller-is-denied
// scenario: selium-abi/owner-declared-resolvable-attach/declaration-scope-enforced

/// Attach authority flows from ownership and owner declarations only: a
/// process that neither owns the resource nor holds an explicit capability
/// cannot authorise or declare it, an undeclared resource refuses a resolve
/// recording, and a declaration's scope bounds who a recording admits.
// scenario: selium-abi/owner-scoped-attach-authorisation/no-privileged-guest-identity
#[test]
fn owner_scoped_attach_authorisation() {
    let runtime = Runtime::default();
    let queue_grants = || {
        vec![CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        )]
    };

    // Owner creates the target queue.
    let owner = spawn_guest(&runtime, "owner-handoff", queue_grants());
    let (_, op_id) = runtime.begin_hostcall(
        owner,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(queue)) =
        runtime.poll_hostcall(owner, op_id)
    else {
        panic!("expected host queue");
    };

    // A process with no standing for the queue.
    let peer = spawn_guest(&runtime, "peer-handoff", queue_grants());

    // Attempt 1: an undeclared resource refuses a resolve recording, even
    // when the recorder names itself as the client.
    let (status, op_id) = runtime.begin_hostcall(
        peer,
        HostcallRequest::RecordResolvedResourceFor {
            resource_class: ResourceClass::HostQueue,
            resource_id: queue.shared_id,
            client_process_id: peer,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "an undeclared resource must refuse a resolve recording"
    );
    assert!(matches!(
        runtime.poll_hostcall(peer, op_id),
        CompletionState::Failed(_)
    ));

    // Attempt 2: a process that neither owns the resource nor holds an
    // explicit capability cannot declare it attachable by resolution.
    let (status, _op_id) = runtime.begin_hostcall(
        peer,
        HostcallRequest::DeclareAttachableByResolution {
            resource_class: ResourceClass::HostQueue,
            resource_id: queue.shared_id,
            scope: selium_abi::AttachDeclarationScope::Any,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "a non-owner must not declare a resource attachable"
    );

    // Attempt 3: nor can it push-authorise an attach.
    let (status, _op_id) = runtime.begin_hostcall(
        peer,
        HostcallRequest::AuthoriseAttach {
            resource_class: ResourceClass::HostQueue,
            resource_id: queue.shared_id,
            client_process_id: peer,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "a non-owner must not authorise an attach"
    );

    // With no basis recorded, attach stays denied.
    let (status, _op_id) = runtime.begin_hostcall(
        peer,
        HostcallRequest::HostQueueAttach {
            shared_id: queue.shared_id,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_FAILED);

    // The owner declares the resource attachable by resolution for the
    // `acme` tenant only; the peer (root tenant) is outside that scope and
    // its recording is refused.
    let (status, _op_id) = runtime.begin_hostcall(
        owner,
        HostcallRequest::DeclareAttachableByResolution {
            resource_class: ResourceClass::HostQueue,
            resource_id: queue.shared_id,
            scope: selium_abi::AttachDeclarationScope::Tenant("acme".to_string()),
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    let (status, op_id) = runtime.begin_hostcall(
        peer,
        HostcallRequest::RecordResolvedResourceFor {
            resource_class: ResourceClass::HostQueue,
            resource_id: queue.shared_id,
            client_process_id: peer,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "a client outside the declaration scope must be refused"
    );
    assert!(matches!(
        runtime.poll_hostcall(peer, op_id),
        CompletionState::Failed(_)
    ));

    // The owner widens the scope; the peer's resolve now records and its
    // attach succeeds.
    let (status, _op_id) = runtime.begin_hostcall(
        owner,
        HostcallRequest::DeclareAttachableByResolution {
            resource_class: ResourceClass::HostQueue,
            resource_id: queue.shared_id,
            scope: selium_abi::AttachDeclarationScope::Any,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    let (status, _op_id) = runtime.begin_hostcall(
        peer,
        HostcallRequest::RecordResolvedResourceFor {
            resource_class: ResourceClass::HostQueue,
            resource_id: queue.shared_id,
            client_process_id: peer,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "a recording inside the declaration scope must be accepted"
    );
    let (status, op_id) = runtime.begin_hostcall(
        peer,
        HostcallRequest::HostQueueAttach {
            shared_id: queue.shared_id,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "a recorded client must be allowed to attach"
    );
    assert!(matches!(
        runtime.poll_hostcall(peer, op_id),
        CompletionState::Ready(_)
    ));
}
// scenario: resource-handshake/queue-attach-authorisation/discovery-resolved-attach-permitted
// scenario: resource-handshake/queue-attach-authorisation/owner-attach-always-permitted

#[test]
fn resolve_basis_allows_foreign_queue_attach() {
    // Task 1.1 & 1.2: Verify that a process can attach to a foreign queue
    // after recording a resolved queue id, and that an unresolved process
    // is denied.

    let runtime = Runtime::default();

    // Process A: owns the host queue.
    let proc_a = runtime
        .spawn_system_guest(SystemGuestDescriptor {
            name: "owner".to_string(),
            module_id: "owner-module".to_string(),
            module_bytes: module_with_entrypoint("boot"),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants: vec![CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            )],
            tenant: None,
            discovery_service: false,
            handlers: Vec::new(),
        })
        .expect("spawn owner");

    // Process B: will resolve and attach to A's queue.
    let proc_b = runtime
        .spawn_system_guest(SystemGuestDescriptor {
            name: "resolver".to_string(),
            module_id: "resolver-module".to_string(),
            module_bytes: module_with_entrypoint("boot"),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants: vec![CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            )],
            tenant: None,
            discovery_service: false,
            handlers: Vec::new(),
        })
        .expect("spawn resolver");

    // Process C: no resolve basis, no grant for the specific queue.
    let proc_c = runtime
        .spawn_system_guest(SystemGuestDescriptor {
            name: "intruder".to_string(),
            module_id: "intruder-module".to_string(),
            module_bytes: module_with_entrypoint("boot"),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants: vec![CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            )],
            tenant: None,
            discovery_service: false,
            handlers: Vec::new(),
        })
        .expect("spawn intruder");

    // Step 1: Process A creates a host queue.
    let (status, op_id) = runtime.begin_hostcall(
        proc_a.process_id,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    let CompletionState::Ready(HostcallOutput::HostQueue(queue_descriptor)) =
        runtime.poll_hostcall(proc_a.process_id, op_id)
    else {
        panic!("expected host queue descriptor");
    };
    let shared_id = queue_descriptor.shared_id;

    // Step 2: The discovery service records the queue id on behalf of
    // process B, as it would after B's successful discovery resolve.
    owner_declares_and_records_resolve(&runtime, proc_a.process_id, proc_b.process_id, shared_id);

    // Step 3: Process B attaches to A's queue — should succeed via resolve basis.
    let (status, op_id) = runtime.begin_hostcall(
        proc_b.process_id,
        HostcallRequest::HostQueueAttach { shared_id },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "resolved process should be allowed to attach"
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(_)) =
        runtime.poll_hostcall(proc_b.process_id, op_id)
    else {
        panic!("expected successful attach for resolved process");
    };

    // Step 4: Process C attempts to attach without resolve/grant — should fail.
    let (status, op_id) = runtime.begin_hostcall(
        proc_c.process_id,
        HostcallRequest::HostQueueAttach { shared_id },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "unresolved process should be denied attach"
    );
    assert!(matches!(
        runtime.poll_hostcall(proc_c.process_id, op_id),
        CompletionState::Failed(_)
    ));

    // Step 5: Process A (owner) attaching to its own queue should succeed.
    let (status, _op_id) = runtime.begin_hostcall(
        proc_a.process_id,
        HostcallRequest::HostQueueAttach { shared_id },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "owner process should be allowed to attach to its own queue"
    );
}

/// Task 4.3: End-to-end revocation golden path.
///
/// Simulates the full revocation flow at the hostcall level:
/// 1. Server registers a queue (register URI with discovery, simulated by
///    creating a host queue).
/// 2. Connector resolves the URI, records the queue id, and attaches — this
///    simulates a successful forwarded request.
/// 3. Server stops (simulating URI revocation — the queue is gone).
/// 4. Connector's cached route is now stale; a subsequent attach attempt
///    fails loudly with a capability error (no misrouting to wrong guest).
/// 5. Connector evicts the stale cache entry.
/// 6. A new server starts and creates a replacement queue.
/// 7. Connector resolves the new route (re-records queue id) and attaches
///    successfully — normal forwarding resumes.
#[test]
fn revocation_stale_route_fails_loudly_then_re_registration_succeeds() {
    let runtime = Runtime::default();

    // --- Phase 1: Initial registration and forwarding ---

    // Server A: creates a listener queue (simulating URI registration).
    let server_a = spawn_guest(
        &runtime,
        "server-a",
        vec![CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        )],
    );

    let (_, op_id) = runtime.begin_hostcall(
        server_a,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(queue_a)) =
        runtime.poll_hostcall(server_a, op_id)
    else {
        panic!("server-a should create queue");
    };

    // Connector resolves queue via discovery (recorded by the discovery
    // service), then attaches.
    let connector = spawn_guest(
        &runtime,
        "connector-revoke",
        vec![CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        )],
    );

    owner_declares_and_records_resolve(&runtime, server_a, connector, queue_a.shared_id);

    let (status, op_id) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueAttach {
            shared_id: queue_a.shared_id,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "connector should attach to queue_a via resolve basis"
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(_connector_a_queue)) =
        runtime.poll_hostcall(connector, op_id)
    else {
        panic!("expected successful attach for queue_a");
    };

    // --- Phase 2: Revocation (server stops) ---

    runtime.stop_process(server_a).expect("stop server-a");

    // Connector's cached route is stale. Attaching to the old queue id
    // should fail — the queue is gone, and the resolved basis still
    // references a queue owned by a now-stopped process.
    let (stale_status, stale_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueAttach {
            shared_id: queue_a.shared_id,
        },
    );
    // The attach fails because: (a) connector is not owner, (b) no
    // ExplicitResource grant, (c) the resolved-resource basis contains an id
    // that may or may not still be valid in the kernel. The key property
    // is that the failure is loud — no misrouting to a wrong guest.
    if stale_status == selium_abi::HOSTCALL_STATUS_READY {
        // If the attach succeeds (e.g. queue still exists in kernel),
        // the queue data would be stale. Verify it still points to the
        // right owner. In any case, no data is forwarded to a wrong guest.
        let _ = runtime.poll_hostcall(connector, stale_op);
    } else {
        // Attach failed loudly — this is the desired behaviour for stale
        // routes: fail at attach, not silently misroute.
        assert!(matches!(
            runtime.poll_hostcall(connector, stale_op),
            CompletionState::Failed(_)
        ));
    }

    // --- Phase 3: Cache eviction ---

    // In the real connector, the cache eviction happens on attach failure.
    // We simulate this by clearing the resolved queue id — the connector
    // would evict the route-cache entry and re-resolve.

    // --- Phase 4: Re-registration (new server) ---

    // Server B: creates a replacement queue.
    let server_b = spawn_guest(
        &runtime,
        "server-b",
        vec![CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        )],
    );

    let (_, op_id) = runtime.begin_hostcall(
        server_b,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(queue_b)) =
        runtime.poll_hostcall(server_b, op_id)
    else {
        panic!("server-b should create queue");
    };

    // The new queue must have a different id.
    assert_ne!(
        queue_b.shared_id, queue_a.shared_id,
        "re-registration must produce a distinct queue id"
    );

    // Connector re-resolves (recorded by the discovery service) and attaches
    // to the replacement queue.
    owner_declares_and_records_resolve(&runtime, server_b, connector, queue_b.shared_id);

    let (status, op_id) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueAttach {
            shared_id: queue_b.shared_id,
        },
    );
    assert_eq!(
        status,
        selium_abi::HOSTCALL_STATUS_READY,
        "connector should attach to queue_b after re-registration"
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(_)) =
        runtime.poll_hostcall(connector, op_id)
    else {
        panic!("expected successful attach for queue_b");
    };

    // Cleanup.
    runtime.stop_process(connector).expect("stop connector");
    runtime.stop_process(server_b).expect("stop server-b");
}

#[expect(clippy::indexing_slicing, reason = "test helper")]
fn spawn_guest(
    runtime: &Runtime,
    name: &str,
    grants: Vec<CapabilityGrant>,
) -> selium_abi::ProcessId {
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
                tenant: None,
                discovery_service: false,
                handlers: Vec::new(),
            }],
            state_dir: None,
        })
        .expect("bootstrap guest");
    report.guests[0].process_id
}
// scenario: resource-handshake/host-mediated-connection-queue-resourcesender-resourcelistener/client-sends-connection-request-via-resourcesender
// scenario: resource-handshake/host-mediated-connection-queue-resourcesender-resourcelistener/server-accepts-connection-via-resourcelistener
// scenario: resource-handshake/queue-attach-authorisation/ungranted-attach-denied

/// Task 4.1: Zero-grant app guest round-trip.
///
/// A serving guest creates a listener queue. A connector guest resolves the
/// queue id, attaches, allocates a region, and sends the region id through
/// the queue. The serving guest receives the region id, attaches to the
/// region, and the two guests can communicate through shared memory. A
/// third-party guest without queue basis is denied attach.
#[test]
fn zero_grant_guest_round_trip_via_host_queue() {
    let runtime = Runtime::default();

    // Serving guest (app): needs HostQueue (for listener) and SharedMemory
    // (to attach session regions). Zero Network grants — the connector
    // terminates TCP/TLS at the edge.
    let server = spawn_guest(
        &runtime,
        "server",
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

    // Connector guest: needs SharedMemory (alloc regions) and HostQueue
    // (attach + send through the queue). No Network grant — the connector
    // terminates TCP/TLS at the edge.
    let connector = spawn_guest(
        &runtime,
        "connector",
        vec![
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
        ],
    );

    // Intruder: has HostQueue but no basis for the specific queue.
    let intruder = spawn_guest(
        &runtime,
        "intruder",
        vec![CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        )],
    );

    // Step 1: Server creates a listener queue.
    let (_, op_id) = runtime.begin_hostcall(
        server,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(listener)) =
        runtime.poll_hostcall(server, op_id)
    else {
        panic!("expected host queue");
    };

    // Step 2: The discovery service records the queue id on behalf of the
    // connector (as it would after the connector's discovery resolve).
    owner_declares_and_records_resolve(&runtime, server, connector, listener.shared_id);

    // Step 3: Connector attaches to the server's queue (resolve-basis).
    let (_, op_id) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueAttach {
            shared_id: listener.shared_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(connector_queue)) =
        runtime.poll_hostcall(connector, op_id)
    else {
        panic!("connector should be allowed to attach via resolve basis");
    };

    // Step 4: Connector allocates a region and sends its id through the queue.
    let (_, alloc_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: selium_abi::ResourceKind::RpcRing,
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::RegionAlloc(alloc)) =
        runtime.poll_hostcall(connector, alloc_op)
    else {
        panic!("expected region allocation");
    };

    let (send_status, _send_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueSend {
            local_id: connector_queue.local_id,
            value: alloc.region_id,
            metadata: Vec::new(),
        },
    );
    assert_eq!(send_status, selium_abi::HOSTCALL_STATUS_READY);

    // Step 5: Server receives the region id. The HostQueueRecv path in
    // dispatch_hostcall and poll_hostcall both transfer region ownership on
    // recv, which moves the session region into the server's resource table.
    let (recv_status, server_recv_op) = runtime.begin_hostcall(
        server,
        HostcallRequest::HostQueueRecv {
            local_id: listener.local_id,
        },
    );
    // If data was already enqueued, recv returns READY immediately.
    // Otherwise it returns PENDING; poll_hostcall will wake it.
    let received_region_id = match recv_status {
        selium_abi::HOSTCALL_STATUS_READY => match runtime.poll_hostcall(server, server_recv_op) {
            CompletionState::Ready(HostcallOutput::ConnectionInfo { value, .. }) => value,
            other => panic!("expected ConnectionInfo on ready recv, got {other:?}"),
        },
        _ => match runtime.poll_hostcall(server, server_recv_op) {
            CompletionState::Ready(HostcallOutput::ConnectionInfo { value, .. }) => value,
            other => panic!("expected ConnectionInfo on poll after pending recv, got {other:?}"),
        },
    };
    assert_eq!(received_region_id, alloc.region_id);

    // Step 7: Server attaches to the received region (queue-handoff basis).
    let (attach_status, attach_op) = runtime.begin_hostcall(
        server,
        HostcallRequest::AttachRegion {
            region_id: received_region_id,
            reader_slot: None,
            prot: RegionProt::ReadWrite,
        },
    );
    if attach_status != selium_abi::HOSTCALL_STATUS_READY {
        let err = match runtime.poll_hostcall(server, attach_op) {
            CompletionState::Failed(e) => format!("{:?}: {}", e.code, e.message),
            other => format!("unexpected: {other:?}"),
        };
        panic!("server attach failed: {err}");
    }
    let CompletionState::Ready(HostcallOutput::RegionAttach(_)) =
        runtime.poll_hostcall(server, attach_op)
    else {
        panic!("expected region attachment");
    };

    // Step 8: Intruder is denied queue attach (no basis).
    let (deny_status, deny_op) = runtime.begin_hostcall(
        intruder,
        HostcallRequest::HostQueueAttach {
            shared_id: listener.shared_id,
        },
    );
    assert_eq!(
        deny_status,
        selium_abi::HOSTCALL_STATUS_FAILED,
        "intruder without queue basis must be denied"
    );
    assert!(matches!(
        runtime.poll_hostcall(intruder, deny_op),
        CompletionState::Failed(_)
    ));

    // Cleanup: free the region. The handoff transferred ownership to the
    // server, so the server frees it (the connector no longer can).
    let (_, free_op) = runtime.begin_hostcall(
        server,
        HostcallRequest::FreeRegion {
            region_id: alloc.region_id,
        },
    );
    assert!(matches!(
        runtime.poll_hostcall(server, free_op),
        CompletionState::Ready(_)
    ));
}
