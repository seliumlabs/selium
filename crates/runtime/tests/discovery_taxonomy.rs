//! Discovery taxonomy substrate tests.
//!
//! Exercises the host authority the runtime keeps for discovery: mint-time
//! tenant records, the authority read, and the revocation-only feed. The host
//! no longer publishes registrations; owners publish their own records.

use std::collections::HashSet;

use selium_abi::{
    AbiErrorCode, Capability, CapabilityGrant, CompletionState, HostcallOutput, HostcallRequest,
    ProcessId, RegionProt, ResourceAuthority, ResourceClass, ResourceKind, ResourceSelector,
};
use selium_runtime::{Runtime, RuntimeConfig, SystemGuestDescriptor};
use selium_service::DiscoveryRequest;
use selium_shm::{Channel, transport::ShmTransport};
use selium_wire::{framed::FramedRead, pubsub::Subscriber};

/// Events observed on the revocation-only feed.
#[derive(Default)]
struct FeedEvents {
    registered: HashSet<String>,
    revoked: HashSet<String>,
    gone: HashSet<ProcessId>,
    freed: HashSet<(String, u64)>,
}

/// A resource minted under a serving tenant is attributed to that tenant by
/// the authority and released under it when the owner exits — the case the
/// tenant side maps used to miss.
// scenario: selium-runtime/resource-revocation-by-identity/minted-resource-revokes-under-its-tenant
// scenario: selium-runtime/resource-revocation-by-identity/teardown-needs-no-side-map
// scenario: selium-runtime/resource-authority-record-on-allocation/mint-records-the-principal-tenant
// scenario: selium-runtime/resource-authority-record-on-allocation/authority-answers-before-the-owner-publishes
// scenario: selium-runtime/no-host-registration-publication/allocation-publishes-nothing
#[test]
fn a_cross_tenant_mint_reports_and_releases_under_the_minting_tenant() {
    let (runtime, mut subscriber) = runtime_with_feed();
    // A ceiling only `acme` is subject to, so a mis-attributed release shows.
    runtime
        .kernel()
        .quota()
        .set("acme", ResourceClass::SharedRegion, 2 * 65536);

    let grants = vec![CapabilityGrant::new(
        Capability::SharedMemory,
        vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
    )];
    // A root-principal guest mints for a named tenant without delegation.
    let pid = spawn_guest(&runtime, "root-minter", None, grants);

    let region_id = alloc_region(&runtime, pid, Some("acme")).expect("cross-tenant mint");
    let events = drain_feed(&mut subscriber);
    assert!(
        events.registered.is_empty(),
        "the host must not publish a registration, got: {:?}",
        events.registered
    );
    assert_eq!(
        authority(&runtime, pid, ResourceClass::SharedRegion, region_id),
        ResourceAuthority {
            owned: true,
            tenant: Some("acme".to_string()),
            other_classes: Vec::new(),
        },
        "the authority answers with the mint-time tenant before any publication"
    );
    assert_eq!(
        runtime
            .kernel()
            .quota()
            .used("acme", ResourceClass::SharedRegion),
        65536,
        "the reservation is charged to the serving tenant's ceiling"
    );

    // Teardown: the process is revoked by owner and the region by identity; the
    // quota returns to the tenant it was charged to.
    runtime.stop_process(pid).expect("stop process");
    let events = drain_feed(&mut subscriber);
    assert!(events.gone.contains(&pid), "expected ProcessGone for {pid}");
    assert!(
        events.freed.contains(&("region".to_string(), region_id)),
        "expected identity-keyed ResourceFreed, got: {:?}",
        events.freed
    );
    assert_eq!(
        runtime
            .kernel()
            .quota()
            .used("acme", ResourceClass::SharedRegion),
        0,
        "the reservation is released to the tenant it was charged to, not the owner's"
    );
}

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

/// The authority answer for a `(class, id)` the caller is expected to own.
#[expect(clippy::panic, reason = "unexpected hostcall output indicates a bug")]
fn authority(
    runtime: &Runtime,
    process_id: ProcessId,
    class: ResourceClass,
    id: u64,
) -> ResourceAuthority {
    let (status, op_id) = runtime.begin_hostcall(
        process_id,
        HostcallRequest::ProcessOwnsResource {
            process_id,
            resource_class: class,
            resource_id: id,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
    match runtime.poll_hostcall(process_id, op_id) {
        CompletionState::Ready(HostcallOutput::ResourceAuthority(authority)) => authority,
        other => panic!("expected ResourceAuthority, got {other:?}"),
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

// scenario: guest-context/canonical-typed-publication/canonical-publication-is-optional
#[test]
fn discovery_lifecycle_over_the_feed_is_revocation_only() {
    let (runtime, mut subscriber) = runtime_with_feed();
    let grants = vec![CapabilityGrant::new(
        Capability::SharedMemory,
        vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
    )];
    let pid = spawn_guest(&runtime, "lifecycle-guest", Some("acme"), grants);

    let events = drain_feed(&mut subscriber);
    assert!(
        events.registered.is_empty(),
        "process start publishes no registration"
    );

    let region_id = alloc_region(&runtime, pid, None).expect("allocate region");
    let events = drain_feed(&mut subscriber);
    assert!(
        events.registered.is_empty(),
        "allocation publishes no registration"
    );
    assert_eq!(
        authority(&runtime, pid, ResourceClass::SharedRegion, region_id).tenant,
        Some("acme".to_string())
    );

    runtime.stop_process(pid).expect("stop process");
    let events = drain_feed(&mut subscriber);
    assert!(events.gone.contains(&pid));
    assert!(
        events.freed.contains(&("region".to_string(), region_id)),
        "expected identity-keyed region revoke, got: {:?}",
        events.freed
    );
}

#[expect(clippy::panic, reason = "feed read errors in test indicate a bug")]
fn drain_feed(subscriber: &mut Subscriber<DiscoveryRequest, ShmTransport>) -> FeedEvents {
    let mut events = FeedEvents::default();
    loop {
        match subscriber.read_with_tag() {
            Ok((request, _tag)) => match request {
                DiscoveryRequest::Register { uri, .. } => {
                    events.registered.insert(uri);
                }
                DiscoveryRequest::Revoke { uri } => {
                    events.revoked.insert(uri);
                }
                DiscoveryRequest::ProcessGone { process_id } => {
                    events.gone.insert(process_id);
                }
                DiscoveryRequest::ResourceFreed { class, id } => {
                    events.freed.insert((class, id));
                }
                _ => {}
            },
            Err(selium_wire::error::Error::BufferEmpty) => break,
            Err(error) => panic!("feed read failed: {error}"),
        }
    }
    events
}
// scenario: selium-abi/resource-authority-hostcall/root-minted-resource-reports-the-root-tenant
// scenario: selium-abi/resource-authority-hostcall/authority-survives-projection-lag
// scenario: selium-abi/expected-claims-and-process-list/expected-claims-are-visible-to-the-owner

// scenario: route-claims/expected-claim-reconciliation/non-publishing-guest-remains-accounted
#[test]
fn expected_claims_are_visible_without_publication() {
    let (runtime, _subscriber) = runtime_with_feed();
    let claimed = "sel:///control";
    let grants = vec![CapabilityGrant::new(
        Capability::RouteClaim,
        vec![ResourceSelector::Uri(claimed.to_string())],
    )];
    let pid = spawn_guest(&runtime, "claim-holder", None, grants);

    // The host knows the expected claim even though the process published
    // nothing, so reconciliation can report drift.
    assert_eq!(runtime.expected_claims(pid), vec![claimed.to_string()]);
    assert!(
        runtime.restore_process_authority(pid).is_some(),
        "the non-publishing guest remains in the process table"
    );
    assert_eq!(
        authority(&runtime, pid, ResourceClass::Process, pid),
        ResourceAuthority {
            owned: true,
            tenant: Some(String::new()),
            other_classes: Vec::new(),
        },
        "the non-publishing guest remains in the ownership sets"
    );
}

// scenario: selium-runtime/principal-provenance-allocation/host-queue-minted-under-the-serving-tenant
// scenario: selium-abi/resource-authority-hostcall/owned-resource-reports-its-tenant
#[test]
fn host_queue_authority_uses_principal_provenance() {
    let (runtime, mut subscriber) = runtime_with_feed();
    let grants = vec![CapabilityGrant::new(
        Capability::HostQueue,
        vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
    )];
    let pid = spawn_guest(&runtime, "queue-guest", Some("acme"), grants);

    // Own-tenant queue: authority reports the caller's tenant.
    let queue_id = create_queue(&runtime, pid, None).expect("own-tenant queue");
    let events = drain_feed(&mut subscriber);
    assert!(
        events.registered.is_empty(),
        "queue creation publishes no registration"
    );
    assert_eq!(
        authority(&runtime, pid, ResourceClass::HostQueue, queue_id),
        ResourceAuthority {
            owned: true,
            tenant: Some("acme".to_string()),
            other_classes: Vec::new(),
        }
    );

    // A cross-tenant queue mint without delegation is denied.
    let denied = create_queue(&runtime, pid, Some("beta"));
    assert_eq!(denied, Err(AbiErrorCode::PermissionDenied));

    // Teardown revokes the queue by identity.
    runtime.stop_process(pid).expect("stop process");
    let events = drain_feed(&mut subscriber);
    assert!(
        events.freed.contains(&("queue".to_string(), queue_id)),
        "expected identity-keyed queue revocation at teardown, got: {:?}",
        events.freed
    );
}

fn module_with_entrypoint(entrypoint: &str) -> Vec<u8> {
    wat::parse_str(format!("(module (func (export \"{entrypoint}\")))")).expect("compile wat")
}

// scenario: selium-runtime/no-host-registration-publication/process-start-publishes-nothing
// scenario: selium-runtime/owner-published-process-nodes/unpublished-process-is-accounted-but-undiscoverable
// scenario: discovery-registration/process-node-addressing/an-unpublished-process-is-accounted-but-undiscoverable
#[test]
fn process_start_and_exit_publish_no_registration() {
    let (runtime, mut subscriber) = runtime_with_feed();
    let grants = vec![CapabilityGrant::new(
        Capability::SharedMemory,
        vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
    )];
    let pid = spawn_guest(&runtime, "acme-guest", Some("acme"), grants);

    let events = drain_feed(&mut subscriber);
    assert!(
        events.registered.is_empty() && events.revoked.is_empty(),
        "spawn publishes no process-node registration"
    );
    // The host still answers for the process: it is accounted for even though
    // it never published a node.
    assert_eq!(
        authority(&runtime, pid, ResourceClass::Process, pid),
        ResourceAuthority {
            owned: true,
            tenant: Some("acme".to_string()),
            other_classes: Vec::new(),
        }
    );

    runtime.stop_process(pid).expect("stop process");
    let events = drain_feed(&mut subscriber);
    assert!(
        events.gone.contains(&pid),
        "owner-keyed ProcessGone revokes the node if the process published one"
    );
    assert!(
        events.revoked.is_empty(),
        "the host does not revoke individual URIs"
    );
}

// scenario: selium-runtime/principal-provenance-allocation/root-principal-allocates-for-any-tenant-without-delegation
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
    let events = drain_feed(&mut subscriber);
    assert!(events.registered.is_empty());
    assert_eq!(
        authority(&runtime, pid, ResourceClass::SharedRegion, region_id).tenant,
        Some("acme".to_string()),
        "the region is attributed to the serving tenant, not the root caller"
    );
}

/// Creates a runtime with the discovery feed enabled and a subscriber on it.
fn runtime_with_feed() -> (Runtime, Subscriber<DiscoveryRequest, ShmTransport>) {
    let runtime = Runtime::default();
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: true,
            system_guests: vec![],
            state_dir: None,
        })
        .expect("bootstrap discovery");
    assert!(report.guests.is_empty());

    let feed_region_id = runtime
        .discovery_feed_region_id()
        .expect("discovery feed region id");
    let channel = Channel::attach_with_provider(runtime.region_provider().as_ref(), feed_region_id)
        .expect("attach to discovery feed");
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
            tenant: tenant.map(str::to_string),
            discovery_service: false,
            handlers: Vec::new(),
        })
        .expect("spawn guest")
        .process_id
}

// scenario: selium-runtime/principal-provenance-allocation/system-process-allocates-for-another-tenant
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
    let events = drain_feed(&mut subscriber);
    assert!(events.registered.is_empty());
    assert_eq!(
        authority(&runtime, pid, ResourceClass::SharedRegion, region_id).tenant,
        Some("acme".to_string())
    );
}

// scenario: selium-runtime/principal-provenance-allocation/unauthorised-cross-tenant-allocation-denied
#[test]
fn unauthorised_cross_tenant_allocation_is_denied() {
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
