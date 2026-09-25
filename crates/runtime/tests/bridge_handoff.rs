//! Bridge handoff substrate test.
//!
//! Drives the golden "external client joins the fabric" flow at the hostcall
//! substrate level: a connector delivers per-stream region handoffs carrying
//! authenticated client identities as `HostQueueSend` metadata; the
//! single-instance (root) bridge-server decodes each identity, maps it to
//! grants, and spawns a bridge-channel under a `Namespace::Root`
//! `DelegateGrants` carrying the client's tenant-scoped grants plus an
//! `ExplicitResource` for the handed-off region; each child can attach its
//! own region and none other. The QUIC/TLS wire behaviour is exercised
//! separately (connector native tests and the wasm spine test).
//!
//! ```sh
//! cargo test -p selium-runtime --test bridge_handoff
//! ```

use selium_abi::{
    AbiErrorCode, ActivityKind, Capability, CapabilityGrant, CompletionState, HostcallOutput,
    HostcallRequest, Namespace, ProcessId, RegionProt, ResourceClass, ResourceIdentity,
    ResourceSelector, client_identity::ClientIdentity,
};
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};

/// Bootstraps the connector (the registered Tier-1 handler for `sel-quic`)
/// and the single per-platform bridge-server (a root guest holding a
/// `Namespace::Root` `DelegateGrants`), pins the server's listener to the
/// connector, and returns the runtime plus the connector and bridge-server
/// process ids.
#[expect(
    clippy::panic,
    reason = "unexpected bootstrap/handler outcome indicates a bug"
)]
fn bootstrap_connector_and_bridge() -> (Runtime, ProcessId, ProcessId) {
    let runtime = Runtime::default();
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            domain_table: Vec::new(),
            system_guests: vec![
                SystemGuestDescriptor {
                    name: "bridge-connector".to_string(),
                    module_id: "bridge-connector-module".to_string(),
                    module_bytes: module_with_entrypoint("boot"),
                    entrypoint: "boot".to_string(),
                    arguments: Vec::new(),
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
                    readiness: ReadinessCondition::Immediate,
                    tenant: None,
                    serving_role: None,
                    handlers: vec!["sel-quic".to_string()],
                },
                SystemGuestDescriptor {
                    name: "bridge-server".to_string(),
                    module_id: "bridge-server-module".to_string(),
                    module_bytes: module_with_entrypoint_args("boot", 1),
                    entrypoint: "boot".to_string(),
                    arguments: vec![selium_runtime::SystemGuestArg::Integer(0)],
                    grants: vec![
                        CapabilityGrant::new(
                            Capability::ProcessLifecycle,
                            vec![ResourceSelector::ResourceClass(ResourceClass::Process)],
                        ),
                        CapabilityGrant::new(
                            Capability::DelegateGrants,
                            vec![ResourceSelector::Namespace(Namespace::Root)],
                        ),
                        CapabilityGrant::new(Capability::SystemRegistration, Vec::new()),
                        CapabilityGrant::new(
                            Capability::HostQueue,
                            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
                        ),
                    ],
                    dependencies: Vec::new(),
                    readiness: ReadinessCondition::Immediate,
                    tenant: None,
                    serving_role: None,
                    handlers: Vec::new(),
                },
            ],
        })
        .expect("bootstrap connector and bridge-server");
    let find = |name: &str| {
        report
            .guests
            .iter()
            .find(|guest| guest.name == name)
            .unwrap_or_else(|| panic!("bootstrap report contains guest {name}"))
    };
    let connector = find("bridge-connector").process_id;
    let bridge_server = find("bridge-server").process_id;

    // The bridge-server pins its listener to the registered `sel-quic`
    // handler: handoffs from any other process must be refused. The handler
    // resolution is bootstrap-authoritative.
    let (_, handler_op) = runtime.begin_hostcall(
        bridge_server,
        HostcallRequest::ResolveProtocolHandler {
            scheme: "sel-quic".to_string(),
        },
    );
    match runtime.poll_hostcall(bridge_server, handler_op) {
        CompletionState::Ready(HostcallOutput::U64(handler)) => {
            assert_eq!(handler, connector, "handler pin must be the connector")
        }
        other => panic!("expected handler pid, got {other:?}"),
    }

    (runtime, connector, bridge_server)
}

/// Creates the bridge-server's own listener queue (self-registration replaces
/// the runtime's well-known-URI queue minting; the queue is minted under the
/// server's root principal and Tier-1 registered), records the discovery
/// resolve, and attaches the connector to the listener. Returns the
/// bridge-server's listener local id and the connector's queue local id.
#[expect(clippy::panic, reason = "unexpected hostcall output indicates a bug")]
fn bridge_listener_and_connector_queue(
    runtime: &Runtime,
    connector: ProcessId,
    bridge_server: ProcessId,
) -> (u64, u64) {
    let (_, create_op) = runtime.begin_hostcall(
        bridge_server,
        HostcallRequest::HostQueueCreate {
            serving_tenant: None,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(server_listener)) =
        runtime.poll_hostcall(bridge_server, create_op)
    else {
        panic!("bridge-server should create its own listener queue");
    };

    discovery_records_resolve(runtime, connector, server_listener.shared_id);
    let (_, attach_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueAttach {
            shared_id: server_listener.shared_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::HostQueue(connector_queue)) =
        runtime.poll_hostcall(connector, attach_op)
    else {
        panic!("connector should attach to the bridge queue");
    };

    (server_listener.local_id, connector_queue.local_id)
}

#[test]
fn bridge_server_delegates_and_child_attaches_stream_region() {
    // Bootstrap the connector (registered Tier-1 handler for `sel-quic`) and
    // the bridge-server, pinning the server's listener to the connector. The
    // runtime no longer provisions the bridge-server's listener; the server
    // creates its own queue and self-registers its route via `serve` (the
    // substrate test drives the hostcalls directly rather than the wasm
    // entrypoint, so the registration is exercised at the queue-minting
    // level).
    let (runtime, connector, bridge_server) = bootstrap_connector_and_bridge();

    runtime
        .register_module_bytes(
            "bridge-channel-module".to_string(),
            module_with_entrypoint("bridge_channel"),
        )
        .expect("register bridge-channel module");

    // 1. The bridge-server creates its own listener queue (self-registration
    //    replaces the runtime's well-known-URI queue minting). The queue is
    //    minted under the server's own (root) principal and Tier-1 registered;
    //    the connector resolves the bridge route via discovery and attaches
    //    the listener (gaining its own local handle, and with it an
    //    authorisation basis for the attach).
    let (server_listener_local_id, connector_queue_local_id) =
        bridge_listener_and_connector_queue(&runtime, connector, bridge_server);

    // 2. The connector allocates the stream region in the client's tenant
    //    scope and delivers the handoff with the authenticated identity.
    let stream_region = deliver_handoff(
        &runtime,
        connector,
        connector_queue_local_id,
        "acme",
        [0x7A; 32],
    );

    // 3. The bridge-server receives it on its own listener and decodes the
    //    identity.
    let (value, identity) = recv_handoff(&runtime, bridge_server, server_listener_local_id);
    assert_eq!(value, stream_region.region_id);
    assert_eq!(identity.tenant, "acme");

    // 4. The bridge-server spawns the bridge-channel with the client's grants
    //    plus a tenant-scoped ExplicitResource grant for the handed-off
    //    region, under the DelegateGrants path (the server does not itself
    //    hold these grants). The root-scoped delegation admits the grants
    //    because every child grant carries a tenant selector, and the root
    //    namespace is greater than any tenant.
    let child_pid = spawn_bridge_channel(&runtime, bridge_server, "acme", stream_region.region_id);

    // 5. The bridge-channel runs under the identity's tenant, not the root
    //    bridge-server's: the tenant-scoped spawn scopes the child to the
    //    handed-off identity, so its tenant-scoped grants are enforceable.
    assert_eq!(
        runtime.process_tenant(child_pid).as_deref(),
        Some("acme"),
        "bridge-channel must run under the handoff identity's tenant"
    );

    // 6. The child attaches the delivered region via its ExplicitResource
    //    grant, matching the Tenant and ExplicitResource selectors (its
    //    process tenant now matches the grant's tenant scope).
    try_attach_region(&runtime, child_pid, stream_region.region_id)
        .expect("bridge-channel attaches its own handed-off region");
}

/// Task 7.1: a completed bridge-channel pipe exits and its process is reaped
/// — no zombie reactor. The child's poll export reports entrypoint completion;
/// the runtime records `ProcessExited` and tears the process down (also
/// returning its process-quota slot to the tenant).
#[test]
fn completed_bridge_channel_is_reaped_without_zombie() {
    let (runtime, _connector, bridge_server) = bootstrap_connector_and_bridge();
    runtime
        .register_module_bytes(
            "bridge-channel-module".to_string(),
            module_with_completing_poll(),
        )
        .expect("register bridge-channel module");
    // An authored ceiling makes the process-slot usage observable.
    runtime
        .kernel()
        .quota()
        .set("acme", ResourceClass::Process, 5);

    let before = runtime.loaded_guest_count();
    let child = spawn_bridge_channel(&runtime, bridge_server, "acme", 42);
    assert_eq!(runtime.loaded_guest_count(), before + 1);
    assert_eq!(
        runtime
            .kernel()
            .quota()
            .used("acme", ResourceClass::Process),
        1
    );

    // The bridge-channel's pipe tears down so its entrypoint returns: the
    // reactor poll reports completion and the runtime reaps the process.
    runtime.poll_guest(child);
    assert!(
        runtime.kernel().processes().inspect_process(child).is_err(),
        "the completed bridge-channel process must be reaped"
    );
    assert_eq!(
        runtime.loaded_guest_count(),
        before,
        "the completed child must not linger as a zombie reactor"
    );
    assert!(
        runtime.activity_log().iter().any(|event| {
            event.process_id == Some(child) && event.kind == ActivityKind::ProcessExited
        }),
        "the reap must record ProcessExited"
    );
    assert_eq!(
        runtime
            .kernel()
            .quota()
            .used("acme", ResourceClass::Process),
        0,
        "the reaped child must return its process slot"
    );
}

/// Allocates a stream region in `tenant`'s scope (a root principal may mint
/// for any tenant) and delivers it to the bridge queue as a handoff carrying
/// the tenant's authenticated identity. Returns the allocated region.
#[expect(clippy::panic, reason = "unexpected hostcall output indicates a bug")]
fn deliver_handoff(
    runtime: &Runtime,
    connector: ProcessId,
    connector_queue_local_id: u64,
    tenant: &str,
    fingerprint: [u8; 32],
) -> selium_abi::RegionAllocation {
    let (_, alloc_op) = runtime.begin_hostcall(
        connector,
        HostcallRequest::AllocRegion {
            pages: 1,
            prot: RegionProt::ReadWrite,
            purpose: selium_abi::ResourceKind::SharedMemory,
            serving_tenant: Some(tenant.to_string()),
        },
    );
    let CompletionState::Ready(HostcallOutput::RegionAlloc(stream_region)) =
        runtime.poll_hostcall(connector, alloc_op)
    else {
        panic!("expected stream region allocation");
    };

    let identity = ClientIdentity {
        tenant: tenant.to_string(),
        fingerprint,
    };
    let (send_status, _) = runtime.begin_hostcall(
        connector,
        HostcallRequest::HostQueueSend {
            local_id: connector_queue_local_id,
            value: stream_region.region_id,
            metadata: identity.encode(),
        },
    );
    assert_eq!(send_status, selium_abi::HOSTCALL_STATUS_READY);

    stream_region
}

fn discovery_records_resolve(runtime: &Runtime, client: ProcessId, shared_id: u64) {
    let discovery = spawn_guest(runtime, "discovery", Vec::new(), None);
    let (status, _op) = runtime.begin_hostcall(
        discovery,
        HostcallRequest::RecordResolvedQueueFor {
            client_process_id: client,
            shared_id,
        },
    );
    assert_eq!(status, selium_abi::HOSTCALL_STATUS_READY);
}

/// A bridge-channel stub whose `__selium_guest_poll` export reports entrypoint
/// completion (`1`): its poll-owner entrypoint has returned, so the runtime
/// reaps it as a normal exit.
fn module_with_completing_poll() -> Vec<u8> {
    wat::parse_str(
        "(module
            (memory 1)
            (func (export \"bridge_channel\"))
            (func (export \"__selium_guest_poll\") (result i32) i32.const 1))",
    )
    .expect("compile wat")
}

fn module_with_entrypoint(entrypoint: &str) -> Vec<u8> {
    module_with_entrypoint_args(entrypoint, 0)
}

fn module_with_entrypoint_args(entrypoint: &str, args: usize) -> Vec<u8> {
    let params: String = "i64 ".repeat(args).trim_end().to_string();
    let params = if params.is_empty() {
        String::new()
    } else {
        format!("(param {params})")
    };
    wat::parse_str(format!(
        "(module (memory 1) (func (export \"{entrypoint}\") {params}))"
    ))
    .expect("compile wat")
}

/// Task 7.2: exceeding a tenant's process ceiling is denied by the quota and
/// surfaced to the client as a clean stream close — the bridge keeps no spawn
/// counter, so the bound is the accountant-authored quota, released on exit.
#[test]
fn process_ceiling_denies_bridge_channel_spawn() {
    let (runtime, _connector, bridge_server) = bootstrap_connector_and_bridge();
    runtime
        .register_module_bytes(
            "bridge-channel-module".to_string(),
            module_with_completing_poll(),
        )
        .expect("register bridge-channel module");
    // Author a ceiling of one bridge-channel process for the tenant.
    runtime
        .kernel()
        .quota()
        .set("acme", ResourceClass::Process, 1);

    // One spawn consumes the only slot.
    let first = spawn_bridge_channel(&runtime, bridge_server, "acme", 42);

    // The second spawn is denied before a second child is created.
    let denied = spawn_bridge_channel_result(&runtime, bridge_server, "acme", 43)
        .expect_err("a spawn over the ceiling must be denied");
    assert_eq!(denied.code, AbiErrorCode::QuotaExceeded);
    assert_eq!(
        runtime
            .kernel()
            .quota()
            .used("acme", ResourceClass::Process),
        1,
        "the denied spawn must not create a second child"
    );

    // The denial aborts only the stream: the bridge attach-then-closes it (the
    // client observes EOF — exercised in the bridge crate's denial-path test),
    // while the process table gains no second child (usage stays at one).

    // The bridge keeps no spawn counter: once the first child exits (the poll
    // completion reaps it), its slot returns and a spawn succeeds again.
    runtime.poll_guest(first);
    let _second = spawn_bridge_channel(&runtime, bridge_server, "acme", 44);
    assert_eq!(
        runtime
            .kernel()
            .quota()
            .used("acme", ResourceClass::Process),
        1,
        "the reaped child's slot lets the tenant spawn again up to the ceiling"
    );
}

/// Receives one delivered handoff on the bridge-server's listener and
/// returns the handed-off value and the decoded client identity.
#[expect(clippy::panic, reason = "unexpected hostcall output indicates a bug")]
fn recv_handoff(
    runtime: &Runtime,
    bridge_server: ProcessId,
    server_listener_local_id: u64,
) -> (u64, ClientIdentity) {
    let (_, recv_op) = runtime.begin_hostcall(
        bridge_server,
        HostcallRequest::HostQueueRecv {
            local_id: server_listener_local_id,
        },
    );
    let CompletionState::Ready(HostcallOutput::ConnectionInfo {
        value, metadata, ..
    }) = runtime.poll_hostcall(bridge_server, recv_op)
    else {
        panic!("bridge-server should receive the delivered handoff");
    };
    let identity = ClientIdentity::decode(&metadata).expect("decode handoff identity");
    (value, identity)
}

/// 4.2: the single per-platform bridge-server serves every tenant, and the
/// tenants stay isolated end to end at the substrate level. Two handoffs —
/// an `acme` identity and a `beta` identity — are delivered to the one root
/// bridge-server; each spawns a bridge-channel scoped to its own identity's
/// tenant; each child attaches only its own handed-off region, and a
/// cross-tenant attach is denied by the runtime's tenant fence.
#[test]
fn single_instance_bridge_serves_two_tenants_without_leak() {
    let (runtime, connector, bridge_server) = bootstrap_connector_and_bridge();

    runtime
        .register_module_bytes(
            "bridge-channel-module".to_string(),
            module_with_entrypoint("bridge_channel"),
        )
        .expect("register bridge-channel module");

    let (server_listener_local_id, connector_queue_local_id) =
        bridge_listener_and_connector_queue(&runtime, connector, bridge_server);

    // Two handoffs, each carrying a different tenant's authenticated
    // identity, and each stream region minted in that tenant's scope.
    let acme_region = deliver_handoff(
        &runtime,
        connector,
        connector_queue_local_id,
        "acme",
        [0x7A; 32],
    );
    let beta_region = deliver_handoff(
        &runtime,
        connector,
        connector_queue_local_id,
        "beta",
        [0x7B; 32],
    );

    // The single bridge-server receives both and decodes each identity — it
    // refuses neither on cross-tenant grounds, because it is bound to no
    // one tenant.
    let (acme_value, acme_identity) =
        recv_handoff(&runtime, bridge_server, server_listener_local_id);
    assert_eq!(acme_value, acme_region.region_id);
    assert_eq!(acme_identity.tenant, "acme");
    let (beta_value, beta_identity) =
        recv_handoff(&runtime, bridge_server, server_listener_local_id);
    assert_eq!(beta_value, beta_region.region_id);
    assert_eq!(beta_identity.tenant, "beta");

    // Each handoff spawns a bridge-channel scoped to its own identity's
    // tenant under the root `DelegateGrants`.
    let acme_child = spawn_bridge_channel(&runtime, bridge_server, "acme", acme_region.region_id);
    let beta_child = spawn_bridge_channel(&runtime, bridge_server, "beta", beta_region.region_id);
    assert_eq!(
        runtime.process_tenant(acme_child).as_deref(),
        Some("acme"),
        "the acme handoff's child runs under acme"
    );
    assert_eq!(
        runtime.process_tenant(beta_child).as_deref(),
        Some("beta"),
        "the beta handoff's child runs under beta"
    );

    // Each child attaches its own handed-off region...
    try_attach_region(&runtime, acme_child, acme_region.region_id)
        .expect("the acme child attaches its own region");
    try_attach_region(&runtime, beta_child, beta_region.region_id)
        .expect("the beta child attaches its own region");

    // ...and neither reaches the other tenant's region: the acme child holds
    // no grant admitting beta's region, and the runtime's region tenant
    // fence denies a cross-tenant attach outright.
    let cross = try_attach_region(&runtime, acme_child, beta_region.region_id)
        .expect_err("the acme child must not attach beta's region");
    assert_eq!(cross.code, selium_abi::AbiErrorCode::PermissionDenied);
    let cross = try_attach_region(&runtime, beta_child, acme_region.region_id)
        .expect_err("the beta child must not attach acme's region");
    assert_eq!(cross.code, selium_abi::AbiErrorCode::PermissionDenied);
}

/// Spawns a bridge-channel under `tenant` with the identity's tenant-scoped
/// grants plus a tenant-scoped `ExplicitResource` grant for the handed-off
/// region — the grant set the root `DelegateGrants` delegation admits.
/// Returns the child's process id.
#[expect(clippy::panic, reason = "unexpected hostcall output indicates a bug")]
fn spawn_bridge_channel(
    runtime: &Runtime,
    bridge_server: ProcessId,
    tenant: &str,
    region_id: u64,
) -> ProcessId {
    spawn_bridge_channel_result(runtime, bridge_server, tenant, region_id)
        .unwrap_or_else(|error| panic!("delegated spawn must succeed, got {error:?}"))
}

/// Like [`spawn_bridge_channel`], but returns the runtime's denial instead of
/// panicking (e.g. a `QuotaExceeded` spawn refusal).
#[expect(clippy::panic, reason = "unexpected hostcall output indicates a bug")]
#[expect(
    clippy::panic_in_result_fn,
    reason = "unexpected hostcall output indicates a bug rather than a recoverable spawn failure"
)]
fn spawn_bridge_channel_result(
    runtime: &Runtime,
    bridge_server: ProcessId,
    tenant: &str,
    region_id: u64,
) -> Result<ProcessId, selium_abi::AbiError> {
    let child_grants = vec![
        CapabilityGrant::new(
            Capability::SharedMemory,
            vec![
                ResourceSelector::Tenant(tenant.to_string()),
                ResourceSelector::ResourceClass(ResourceClass::SharedRegion),
            ],
        ),
        CapabilityGrant::new(
            Capability::SharedMemory,
            vec![
                ResourceSelector::Tenant(tenant.to_string()),
                ResourceSelector::ExplicitResource(ResourceIdentity::Shared(region_id)),
            ],
        ),
    ];
    let (status, spawn_op) = runtime.begin_hostcall(
        bridge_server,
        HostcallRequest::ProcessStart {
            module_id: "bridge-channel-module".to_string(),
            entrypoint: "bridge_channel".to_string(),
            arguments: Vec::new(),
            grants: child_grants,
            tenant: Some(tenant.to_string()),
        },
    );
    match runtime.poll_hostcall(bridge_server, spawn_op) {
        CompletionState::Ready(HostcallOutput::Process(child))
            if status == selium_abi::HOSTCALL_STATUS_READY =>
        {
            Ok(child.local_id)
        }
        CompletionState::Failed(error) => Err(error),
        other => panic!("expected spawn outcome, got status={status} outcome={other:?}"),
    }
}

fn spawn_guest(
    runtime: &Runtime,
    name: &str,
    grants: Vec<CapabilityGrant>,
    tenant: Option<&str>,
) -> ProcessId {
    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            domain_table: Vec::new(),
            system_guests: vec![SystemGuestDescriptor {
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
            }],
        })
        .expect("bootstrap guest");
    report
        .guests
        .first()
        .expect("bootstrap report contains the requested guest")
        .process_id
}

/// Attempts to attach `region_id`, returning the attach offset on success or
/// the hostcall error on failure.
#[expect(
    clippy::panic_in_result_fn,
    reason = "test helper surfaces unexpected hostcall output as a panic"
)]
#[expect(clippy::panic, reason = "unexpected hostcall output indicates a bug")]
fn try_attach_region(
    runtime: &Runtime,
    process_id: ProcessId,
    region_id: u64,
) -> Result<selium_abi::RegionAttachment, selium_abi::AbiError> {
    let (_, attach_op) = runtime.begin_hostcall(
        process_id,
        HostcallRequest::AttachRegion {
            region_id,
            reader_slot: None,
            prot: RegionProt::ReadWrite,
        },
    );
    match runtime.poll_hostcall(process_id, attach_op) {
        CompletionState::Ready(HostcallOutput::RegionAttach(attachment)) => Ok(attachment),
        CompletionState::Failed(error) => Err(error),
        other => panic!("expected attach outcome, got {other:?}"),
    }
}
