//! Discovery taxonomy substrate tests.
//!
//! Exercises the Tier-1 discovery flow the runtime publishes over the
//! discovery feed: process-node registration at spawn, principal-provenance
//! region allocation with tenant-scoped delegation, and teardown revocation.

use selium_abi::{
    AbiErrorCode, Capability, CapabilityGrant, CompletionState, HostcallOutput, HostcallRequest,
    ProcessId, RegionProt, ResourceClass, ResourceKind, ResourceSelector,
};
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};
use selium_service::DiscoveryRequest;
use selium_shm::{Channel, transport::ShmTransport};
use selium_wire::{framed::FramedRead, pubsub::Subscriber};

#[expect(
    clippy::panic_in_result_fn,
    reason = "test helper surfaces unexpected hostcall output as a panic"
)]
#[expect(clippy::panic, reason = "unexpected hostcall output indicates a bug")]
fn alloc_region(
    runtime: &Runtime,
    process_id: ProcessId,
    serving_tenant: Option<&str>,
) -> Result<u64, AbiErrorCode> {
    let (status, op_id) = runtime.begin_hostcall(
        process_id,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: ResourceKind::SharedMemory,
            serving_tenant: serving_tenant.map(str::to_string),
        },
    );
    if status != selium_abi::HOSTCALL_STATUS_READY {
        let state = runtime.poll_hostcall(process_id, op_id);
        return match state {
            CompletionState::Failed(error) => Err(error.code),
            other => panic!("expected failure, got {other:?}"),
        };
    }
    match runtime.poll_hostcall(process_id, op_id) {
        CompletionState::Ready(HostcallOutput::RegionAlloc(alloc)) => Ok(alloc.region_id),
        other => panic!("expected RegionAlloc, got {other:?}"),
    }
}

/// Creates a host queue through `HostQueueCreate`, optionally minted under a
/// serving tenant, and returns its shared id.
#[expect(
    clippy::panic_in_result_fn,
    reason = "test helper surfaces unexpected hostcall output as a panic"
)]
#[expect(clippy::panic, reason = "unexpected hostcall output indicates a bug")]
fn create_queue(
    runtime: &Runtime,
    process_id: ProcessId,
    serving_tenant: Option<&str>,
) -> Result<u64, AbiErrorCode> {
    let (status, op_id) = runtime.begin_hostcall(
        process_id,
        HostcallRequest::HostQueueCreate {
            serving_tenant: serving_tenant.map(str::to_string),
        },
    );
    if status != selium_abi::HOSTCALL_STATUS_READY {
        let state = runtime.poll_hostcall(process_id, op_id);
        return match state {
            CompletionState::Failed(error) => Err(error.code),
            other => panic!("expected failure, got {other:?}"),
        };
    }
    match runtime.poll_hostcall(process_id, op_id) {
        CompletionState::Ready(HostcallOutput::HostQueue(descriptor)) => Ok(descriptor.shared_id),
        other => panic!("expected HostQueue, got {other:?}"),
    }
}

#[test]
fn discovery_lifecycle_over_the_feed() {
    // 5.2 runtime-substrate golden path: spawn-node → allocate-region →
    // teardown-revoke, observed end-to-end on the discovery feed.
    let (runtime, mut subscriber) = runtime_with_feed();
    let grants = vec![CapabilityGrant::new(
        Capability::SharedMemory,
        vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
    )];
    let pid = spawn_guest(&runtime, "lifecycle-guest", Some("acme"), grants);

    let (registered, _) = drain_uris(&mut subscriber);
    assert!(registered.contains(&format!("sel://acme/proc/{pid}")));

    let region_id = alloc_region(&runtime, pid, None).expect("allocate region");
    let (registered, _) = drain_uris(&mut subscriber);
    assert!(registered.contains(&format!("sel://acme/region/{region_id}")));

    runtime.stop_process(pid).expect("stop process");
    let (_, revoked) = drain_uris(&mut subscriber);
    assert!(revoked.contains(&format!("sel://acme/proc/{pid}")));
    assert!(revoked.contains(&format!("sel://acme/region/{region_id}")));
}

#[expect(clippy::panic, reason = "feed read errors in test indicate a bug")]
fn drain_uris(
    subscriber: &mut Subscriber<DiscoveryRequest, ShmTransport>,
) -> (
    std::collections::HashSet<String>,
    std::collections::HashSet<String>,
) {
    let mut registered = std::collections::HashSet::new();
    let mut revoked = std::collections::HashSet::new();
    loop {
        match subscriber.read_with_tag() {
            Ok((request, _tag)) => match request {
                DiscoveryRequest::Register { uri, .. } => {
                    registered.insert(uri);
                }
                DiscoveryRequest::Revoke { uri } => {
                    revoked.insert(uri);
                }
                _ => {}
            },
            Err(selium_wire::error::Error::BufferEmpty) => break,
            Err(error) => panic!("feed read failed: {error}"),
        }
    }
    (registered, revoked)
}

#[test]
fn host_queue_registration_uses_principal_provenance() {
    let (runtime, mut subscriber) = runtime_with_feed();
    let grants = vec![CapabilityGrant::new(
        Capability::HostQueue,
        vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
    )];
    let pid = spawn_guest(&runtime, "queue-guest", Some("acme"), grants);
    let _ = drain_uris(&mut subscriber); // process-node registration

    // Own-tenant queue: registered under `sel://acme/queue/<id>`.
    let queue_id = create_queue(&runtime, pid, None).expect("own-tenant queue");
    let (registered, _) = drain_uris(&mut subscriber);
    assert!(
        registered.contains(&format!("sel://acme/queue/{queue_id}")),
        "expected queue registered under the caller's tenant, got: {registered:?}"
    );

    // A cross-tenant queue mint without delegation is denied.
    let denied = create_queue(&runtime, pid, Some("beta"));
    assert_eq!(denied, Err(AbiErrorCode::PermissionDenied));

    // Teardown revokes the queue under the tenant it was minted for.
    runtime.stop_process(pid).expect("stop process");
    let (_, revoked) = drain_uris(&mut subscriber);
    assert!(
        revoked.contains(&format!("sel://acme/queue/{queue_id}")),
        "expected queue revocation at teardown, got: {revoked:?}"
    );
}

fn module_with_entrypoint(entrypoint: &str) -> Vec<u8> {
    wat::parse_str(format!("(module (func (export \"{entrypoint}\")))")).expect("compile wat")
}

#[test]
fn process_node_registered_at_spawn_and_revoked_at_teardown() {
    let (runtime, mut subscriber) = runtime_with_feed();
    let grants = vec![CapabilityGrant::new(
        Capability::SharedMemory,
        vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
    )];
    let pid = spawn_guest(&runtime, "acme-guest", Some("acme"), grants);

    let (registered, _) = drain_uris(&mut subscriber);
    assert!(
        registered.contains(&format!("sel://acme/proc/{pid}")),
        "expected process-node registration, got: {registered:?}"
    );

    runtime.stop_process(pid).expect("stop process");
    let (_, revoked) = drain_uris(&mut subscriber);
    assert!(
        revoked.contains(&format!("sel://acme/proc/{pid}")),
        "expected process-node revocation, got: {revoked:?}"
    );
}

#[test]
fn root_process_allocates_for_any_tenant_without_delegation() {
    // Option B: a root principal (no tenant) may mint for any tenant — this
    // is the path connector-quic takes under mTLS, where the connector
    // (root) allocates per-stream regions under the authenticated client's
    // tenant without holding per-tenant delegation grants.
    let (runtime, mut subscriber) = runtime_with_feed();
    let grants = vec![CapabilityGrant::new(
        Capability::SharedMemory,
        vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
    )];
    let pid = spawn_guest(&runtime, "root-connector", None, grants);

    let region_id = alloc_region(&runtime, pid, Some("acme")).expect("root allocation");
    let (registered, _) = drain_uris(&mut subscriber);
    assert!(
        registered.contains(&format!("sel://acme/region/{region_id}")),
        "expected region minted under the serving tenant, got: {registered:?}"
    );
}

/// Creates a runtime with the discovery feed enabled and a subscriber on it.
fn runtime_with_feed() -> (Runtime, Subscriber<DiscoveryRequest, ShmTransport>) {
    let runtime = Runtime::default();
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![],
            domain_table: Vec::new(),
        })
        .expect("bootstrap discovery");
    assert!(report.guests.is_empty());

    let feed_region_id = runtime
        .discovery_feed_region_id()
        .expect("discovery feed region id");
    let channel = Channel::attach(feed_region_id).expect("attach to discovery feed");
    let capacity = channel.ring().capacity();
    let transport = ShmTransport::new(&channel, &channel).expect("feed transport");
    (
        runtime,
        Subscriber::new(FramedRead::new(transport), Some(capacity)),
    )
}

fn spawn_guest(
    runtime: &Runtime,
    name: &str,
    tenant: Option<&str>,
    grants: Vec<CapabilityGrant>,
) -> ProcessId {
    runtime
        .spawn_system_guest(SystemGuestDescriptor {
            name: name.to_string(),
            module_id: format!("{name}-module"),
            module_bytes: module_with_entrypoint("boot"),
            entrypoint: "boot".to_string(),
            arguments: Vec::new(),
            grants,
            dependencies: Vec::new(),
            readiness: ReadinessCondition::Immediate,
            tenant: tenant.map(str::to_string),
            serving_role: None,
            handlers: Vec::new(),
        })
        .expect("spawn guest")
        .process_id
}

#[test]
fn system_process_allocates_for_another_tenant_with_delegation() {
    let (runtime, mut subscriber) = runtime_with_feed();
    // A root/system process (no tenant) holding tenant-scoped delegation for
    // tenant "acme".
    let grants = vec![
        CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        ),
        CapabilityGrant::new(
            Capability::DelegateGrants,
            vec![ResourceSelector::Tenant("acme".to_string())],
        ),
    ];
    let pid = spawn_guest(&runtime, "system-allocator", None, grants);

    let region_id = alloc_region(&runtime, pid, Some("acme")).expect("delegated allocation");
    let (registered, _) = drain_uris(&mut subscriber);
    assert!(
        registered.contains(&format!("sel://acme/region/{region_id}")),
        "expected region minted under the principal tenant, got: {registered:?}"
    );
}

#[test]
fn unauthorized_cross_tenant_allocation_is_denied() {
    let (runtime, _subscriber) = runtime_with_feed();
    // A tenant-scoped process (tenant "acme") without delegation for "beta"
    // cannot mint a region for it.
    let grants = vec![CapabilityGrant::new(
        Capability::SharedMemory,
        vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
    )];
    let pid = spawn_guest(&runtime, "unprivileged", Some("acme"), grants);

    let result = alloc_region(&runtime, pid, Some("beta"));
    assert_eq!(result, Err(AbiErrorCode::PermissionDenied));
}
