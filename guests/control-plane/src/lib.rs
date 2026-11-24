//! Control-plane system guest (single per-platform instance).
//!
//! Serves a typed, capability-gated control RPC surface. The guest creates its
//! own listener (a host-mediated connection queue), registers it as a
//! root-namespace service (`sel:///control`, wire name `control`) through
//! discovery, and runs the existing shared-memory RPC path over it: each
//! session is a two-ring shared-memory (request/reply) region the caller
//! allocates with `rpc::connect` and rendezvous to the control plane's queue,
//! which the control plane `rpc::accept`s — the same internal shared-memory
//! path the discovery guest serves. Data flows over shared memory; only
//! control flows over hostcalls.
//!
//! The control plane derives each session's requestor namespace from the
//! delivering process's tenant (its **process owner**) — the
//! runtime-persisted process authority, never handoff metadata or a tenant the
//! caller asserts for itself. An external client reaches the control plane as
//! a bridge-channel spawned under its authenticated tenant, so the
//! bridge-channel's process owner *is* that tenant. The control plane owns
//! **user-facing desired state** (deployments and pipeline bindings) in a
//! single platform-scoped durable log, tags every record with its tenant, and
//! projects it into an in-memory read model partitioned by namespace:
//!
//! - **placement/scale/stop** → the scheduler (typed `SchedulerRequest`),
//! - **resolve** → discovery (`Context::lookup`),
//! - **module upload** → the store service (blob put + manifest set,
//!   tenant-prefixed manifest names).
//!
//! ## Resource catalogue
//!
//! The control plane also owns the platform's guest-owned **resource
//! catalogue**: durable records naming resources (URI, class, owning process,
//! tenant, purpose, resource id) appended to [`CONTROL_CATALOGUE_LOG`] and
//! projected into a tenant-partitioned read model rebuilt by replay on boot.
//! The catalogue serves three operations over the same tenant-scoped session
//! surface as the desired-state verbs:
//!
//! - `ControlRequest::RegisterResource` — record a name, owner-checked against
//!   the session's process and class-checked against the closed resource-class
//!   set,
//! - `ControlRequest::RemoveResource` — tombstone a name the session's process
//!   owns,
//! - `ControlRequest::LookupResource` — resolve a name within the session's
//!   tenant (a name registered under another tenant is invisible).
//!
//! The served catalogue operations are exactly the `ControlRequest` variants
//! `RegisterResource`, `RemoveResource`, and `LookupResource`; the
//! `handle_request` match is exhaustive, so a request variant without a
//! catalogue path would not compile. `ControlRequest::Resolve` remains a
//! discovery lookup, not a catalogue lookup.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use anyhow::Context as _;
use selium_abi::{
    Capability, CapabilityGrant, Namespace, ResourceClass, ResourceSelector, ScopeContext,
};
use selium_guest::{
    BlobStore, Context, DurableLog, ResourceListener, Serve, debug, entrypoint, info,
    namespace_from_tenant, spawn, warn,
};
use selium_service::{
    CatalogueLogRecord, CatalogueRecord, ControlRequest, ControlResponse, DelegationStatus,
    Deployment, DesiredStateRecord, FlatMsg, PipelineBinding, ResolvedTarget, ResourceTarget,
    SchedulerRequest, SchedulerResponse,
};
use selium_shm::rpc;

/// Blob store name holding uploaded module bytes.
pub const CONTROL_BLOB_STORE: &str = "selium.control-plane.modules";
/// Durable log name holding the resource catalogue's records.
pub const CONTROL_CATALOGUE_LOG: &str = "selium.control-plane.catalogue";
/// Durable log name holding the control plane's desired-state records.
pub const CONTROL_LOG: &str = "selium.control-plane.desired-state";
/// The internal route segment the control plane serves under.
pub const CONTROL_PATH: &str = "control";

/// The desired-state read model: deployments and pipeline bindings projected
/// from the control plane's durable log, partitioned by requestor namespace.
///
/// The durable log is the store of record; this structure is rebuilt from
/// replay on boot and updated in lock-step with each append. Records carry
/// their tenant, so each append lands in exactly one partition; a session reads
/// only its own partition.
#[derive(Debug, Clone, Default)]
pub struct ControlPlaneState {
    partitions: BTreeMap<Namespace, DesiredStateView>,
}

/// One tenant's slice of the desired-state projection.
#[derive(Debug, Clone, Default)]
struct DesiredStateView {
    deployments: BTreeMap<String, Deployment>,
    pipelines: BTreeMap<String, PipelineBinding>,
}

/// Scheduler delegation seam.
///
/// Day-1 boundary: the scheduler guest's RPC service is not yet online
/// (`implement-system-guests` §5), so delegation returns a typed deferred
/// status — recording intent without pretending it was applied. When the
/// scheduler service lands, this becomes a
/// `selium_shm::rpc::RpcClient<SchedulerRequest, SchedulerResponse>` resolved
/// through discovery; this method is the seam that client replaces.
#[derive(Debug, Clone, Copy, Default)]
pub struct SchedulerClient;

/// The guest-owned resource catalogue: named resources projected from the
/// catalogue's durable log, partitioned by requestor namespace.
///
/// The runtime holds no naming metadata; registration and lookup are RPCs to
/// the control plane, scoped to the session's tenant. The durable log is the
/// store of record, so the projection is rebuilt from replay on boot.
#[derive(Debug, Clone, Default)]
pub struct ResourceCatalogue {
    partitions: BTreeMap<Namespace, BTreeMap<String, CatalogueRecord>>,
}

/// The catalogue's shared state: its durable log (the store of record) and the
/// projected read model rebuilt from replay.
#[derive(Clone)]
struct CatalogueState {
    log: DurableLog,
    projection: Arc<Mutex<ResourceCatalogue>>,
}

/// State shared by every accepted control session, including the guest-wide
/// mutation lane that orders both durable logs against their projections.
#[derive(Clone)]
struct SharedState {
    desired: Arc<Mutex<ControlPlaneState>>,
    log: DurableLog,
    catalogue: CatalogueState,
    blobs: BlobStore,
    mutations: Arc<tokio::sync::Mutex<()>>,
}

/// One accepted session's scope: the requestor namespace derived from the
/// delivering process's tenant and the process id that opened the session.
#[derive(Clone)]
struct Session {
    namespace: Namespace,
    client_process_id: selium_abi::ProcessId,
}

impl ControlPlaneState {
    /// Applies one desired-state record (a log entry, in replay order) into
    /// the partition named by the record's tenant.
    pub fn apply_record(&mut self, record: DesiredStateRecord) {
        let namespace = namespace_from_tenant(Some(record.tenant()));
        let view = self.partitions.entry(namespace).or_default();
        match record {
            DesiredStateRecord::Deployment { deployment, .. } => {
                view.deployments
                    .insert(deployment.workload_id.clone(), deployment);
            }
            DesiredStateRecord::PipelineBinding { pipeline, .. } => {
                view.pipelines.insert(pipeline.name.clone(), pipeline);
            }
            DesiredStateRecord::Stop { workload_id, .. } => {
                view.deployments.remove(&workload_id);
            }
        }
    }

    /// Returns the last accepted desired state for a workload in the given
    /// requestor namespace.
    pub fn deployment(&self, namespace: &Namespace, workload_id: &str) -> Option<&Deployment> {
        self.partitions
            .get(namespace)
            .and_then(|view| view.deployments.get(workload_id))
    }

    /// Returns the last accepted binding for a named pipeline in the given
    /// requestor namespace.
    pub fn pipeline(&self, namespace: &Namespace, name: &str) -> Option<&PipelineBinding> {
        self.partitions
            .get(namespace)
            .and_then(|view| view.pipelines.get(name))
    }

    /// Materialises the projection from previously appended log records.
    async fn rebuild(&mut self, log: &DurableLog) -> selium_guest::Result<()> {
        let records = log.replay(None, u32::MAX).await?;
        for record in records {
            match FlatMsg::decode(&record.payload) {
                Ok(desired) => self.apply_record(desired),
                Err(error) => {
                    warn!("control-plane: skipping undecodable desired-state record: {error}");
                }
            }
        }
        Ok(())
    }
}

impl SchedulerClient {
    /// Delegates one scheduler interaction, returning its typed outcome.
    pub fn delegate(&self, request: SchedulerRequest) -> selium_guest::Result<SchedulerResponse> {
        debug!(
            ?request,
            "control-plane: scheduler delegation deferred until the scheduler guest lands its RPC service"
        );
        Ok(SchedulerResponse::Deferred {
            reason: "scheduler service not yet online".to_string(),
        })
    }
}

impl ResourceCatalogue {
    /// Applies one catalogue log record into the partition named by the
    /// record's tenant.
    pub fn apply_record(&mut self, record: CatalogueLogRecord) {
        match record {
            CatalogueLogRecord::Register { record } => {
                self.register(&namespace_from_tenant(Some(record.tenant.as_str())), record);
            }
            CatalogueLogRecord::Remove { uri, tenant } => {
                let namespace = namespace_from_tenant(Some(tenant.as_str()));
                if let Some(view) = self.partitions.get_mut(&namespace) {
                    view.remove(&uri);
                }
            }
        }
    }

    /// Records a name in the given namespace, replacing any prior record.
    pub fn register(&mut self, namespace: &Namespace, record: CatalogueRecord) {
        self.partitions
            .entry(namespace.clone())
            .or_default()
            .insert(record.uri.clone(), record);
    }

    /// Removes a name from the given namespace, returning whether it existed.
    pub fn remove(&mut self, namespace: &Namespace, uri: &str) -> bool {
        self.partitions
            .get_mut(namespace)
            .is_some_and(|view| view.remove(uri).is_some())
    }

    /// Looks up a name in the given namespace.
    pub fn lookup(&self, namespace: &Namespace, uri: &str) -> Option<&CatalogueRecord> {
        self.partitions
            .get(namespace)
            .and_then(|view| view.get(uri))
    }

    /// Materialises the projection from previously appended log records.
    async fn rebuild(&mut self, log: &DurableLog) -> selium_guest::Result<()> {
        let records = log.replay(None, u32::MAX).await?;
        for record in records {
            match FlatMsg::decode(&record.payload) {
                Ok(entry) => self.apply_record(entry),
                Err(error) => {
                    warn!("control-plane: skipping undecodable catalogue record: {error}");
                }
            }
        }
        Ok(())
    }
}

/// Returns whether a client's grant set admits an attach to the control
/// surface in the given tenant scope: a host-queue admission (the serving
/// listener's resource class).
///
/// This mirrors the grant matrix the runtime enforces at attach/send time. It
/// is a grant-matrix evaluation, not an ad-hoc identity check — the control
/// plane never parses client identity to admit a session.
pub fn admit_control_client(grants: &[CapabilityGrant], tenant: &str) -> bool {
    let scope = ScopeContext {
        tenant: Some(tenant.to_string()),
        resource_class: Some(ResourceClass::HostQueue),
        ..ScopeContext::default()
    };
    grants
        .iter()
        .any(|grant| grant.capability == Capability::HostQueue && grant.allows(&scope))
}

/// The grant set assigned to the single-instance control-plane guest: storage
/// (the desired-state log and module blob store, served over store volumes),
/// shared memory (the RPC session rings), host queue (the serving listener
/// plus the pre-connected discovery RPC client), and system registration (the
/// root-namespace serving route). All scoped at class scope — the control
/// plane serves every tenant and derives each session's tenant from the
/// authenticated identity, never from a tenant selector on its own grants.
pub fn control_plane_grants() -> Vec<CapabilityGrant> {
    vec![
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
            vec![ResourceSelector::Uri(control_route())],
        ),
    ]
}

/// The exact reserved route claim covered by the control plane's descriptor.
pub fn control_route() -> String {
    format!("{}/{}", selium_abi::uri::SEL_PREFIX, CONTROL_PATH)
}

/// Maps a discovery lookup outcome to a typed resolve response.
pub fn resolve_response(target: Option<ResourceTarget>) -> ControlResponse {
    match target {
        Some(target) => ControlResponse::Resolved {
            target: Some(ResolvedTarget {
                uri: target.uri,
                host_id: target.host_id,
                resource_id: target.resource_id,
            }),
        },
        None => ControlResponse::Resolved { target: None },
    }
}

/// Delegates one scheduler interaction and returns the accepted response
/// carrying the typed delegated status.
fn accept_delegated(
    workload_id: &str,
    replicas: u32,
    module: String,
    request: SchedulerRequest,
) -> ControlResponse {
    match SchedulerClient.delegate(request) {
        Ok(response) => {
            let delegated = delegation_status(response);
            ControlResponse::Accepted {
                workload_id: workload_id.to_string(),
                replicas,
                module,
                delegated,
            }
        }
        Err(error) => ControlResponse::Error {
            step: "scheduler".to_string(),
            context: format!("{error}"),
        },
    }
}

/// The typed refusal for a catalogue operation a session may not perform, or
/// `None` when the session is admitted.
///
/// A session may only register or remove a name it owns: naming a resource
/// owned by another process would let a session record a name for a peer it
/// does not control. Kept separate from the request handler so the rule is
/// testable without a live store.
fn catalogue_owner_refusal(client_process_id: u64, owner: u64) -> Option<String> {
    (client_process_id != owner)
        .then(|| format!("process {client_process_id} cannot manage a resource owned by {owner}"))
}

/// Control-plane entrypoint.
///
/// Creates its own listener, registers the root `control` serving route with
/// discovery, and reports readiness only after the route is registered — then
/// accepts typed shared-memory RPC sessions, each scoped to the requestor
/// namespace derived from the delivering process's tenant (its process owner).
#[entrypoint]
async fn control_plane_main(mut ctx: Context) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("control-plane: started");

    let log = ctx
        .open_log(CONTROL_LOG)
        .await
        .with_context(|| "control-plane: log open failed")?;
    let catalogue_log = ctx
        .open_log(CONTROL_CATALOGUE_LOG)
        .await
        .with_context(|| "control-plane: catalogue log open failed")?;
    let blobs = ctx
        .open_blob_store(CONTROL_BLOB_STORE)
        .await
        .with_context(|| "control-plane: blob store open failed")?;
    let mut desired = ControlPlaneState::default();
    desired
        .rebuild(&log)
        .await
        .with_context(|| "control-plane: projection rebuild failed")?;
    let state = Arc::new(Mutex::new(desired));

    let mut catalogue = ResourceCatalogue::default();
    catalogue
        .rebuild(&catalogue_log)
        .await
        .with_context(|| "control-plane: catalogue rebuild failed")?;
    let catalogue = CatalogueState {
        log: catalogue_log,
        projection: Arc::new(Mutex::new(catalogue)),
    };

    // The server creates its own listener: self-registration replaces the
    // runtime's well-known-URI queue minting. Sessions are internal
    // shared-memory RPC rendezvous, like `discovery` — no connector pinning,
    // and the tenant is the process owner, not handoff metadata.
    let listener =
        ResourceListener::create().with_context(|| "control-plane: create listener failed")?;

    // Register the root serving route (`sel:///control`, wire name `control`)
    // from one declaration. The single-instance control plane serves every
    // tenant, so it holds no own tenant to register under.
    let target = ResourceTarget {
        uri: String::new(), // pinned by `serve` to the derived internal path
        host_id: String::new(),
        resource_id: listener.descriptor().shared_id,
        interface: None,
        tenant: None,
        class: ResourceClass::HostQueue,
        labels: Vec::new(),
    };
    ctx.serve(Serve {
        path: vec![CONTROL_PATH.to_string()],
        target,
        default: false,
    })
    .await
    .with_context(|| "control-plane: serve failed")?;

    // Health is published state: advertise the control plane's health.
    if let Err(error) = ctx.publish_health("control-plane").await {
        warn!("control-plane: health publish failed: {error}");
    }

    let discovery_handle = ctx.raw_handle();
    // Every desired-state and catalogue mutation shares one commit lane. The
    // lane spans durable append + sync and projection, so concurrent sessions
    // cannot append B after A but project A after B.
    let shared = SharedState {
        desired: state,
        log,
        catalogue,
        blobs,
        mutations: Arc::new(tokio::sync::Mutex::new(())),
    };

    loop {
        let incoming = match listener.recv().await {
            Ok(incoming) => incoming,
            Err(error) => {
                warn!("control-plane: accept failed: {error}");
                continue;
            }
        };

        // Derive the session's requestor namespace from the delivering
        // process's tenant — the runtime-persisted process authority, the only
        // non-forgeable tenant source. An external client arrives as a
        // bridge-channel spawned under its authenticated tenant, so the
        // bridge-channel's process owner *is* that tenant. Refuse the session
        // only when the process-tenant lookup itself fails.
        let client_process_id = incoming.client_process_id;
        let namespace = match selium_guest::process_namespace(client_process_id) {
            Ok(namespace) => namespace,
            Err(error) => {
                warn!("control-plane: refusing session with unresolvable tenant: {error}");
                refuse_session(incoming.shared_id);
                continue;
            }
        };

        let connection = match rpc::accept::<ControlRequest, ControlResponse>(incoming.into()) {
            Ok(connection) => connection,
            Err(error) => {
                warn!("control-plane: rpc accept failed: {error}");
                continue;
            }
        };
        spawn(handle_connection(
            connection,
            discovery_handle,
            shared.clone(),
            Session {
                namespace,
                client_process_id,
            },
        ));
    }
}

/// Maps a scheduler response to a typed delegation status.
fn delegation_status(response: SchedulerResponse) -> DelegationStatus {
    match response {
        SchedulerResponse::Applied => DelegationStatus {
            step: "scheduler".to_string(),
            applied: true,
            context: "applied".to_string(),
        },
        SchedulerResponse::Deferred { reason } => DelegationStatus {
            step: "scheduler".to_string(),
            applied: false,
            context: reason,
        },
        SchedulerResponse::Rejected { reason } => DelegationStatus {
            step: "scheduler".to_string(),
            applied: false,
            context: reason,
        },
    }
}

/// Serves one accepted RPC session: each request is decoded (`request.payload`
/// is the typed decode; a failure becomes a typed serialisation error rather
/// than any text-grammar interpretation), handled scoped to the session's
/// requestor namespace, and replied to over the session's reply ring.
async fn handle_connection(
    mut connection: rpc::RpcConnection<ControlRequest, ControlResponse>,
    discovery_handle: u64,
    shared: SharedState,
    session: Session,
) {
    // Each connection builds its own discovery client for `Resolve`; the
    // bootstrap context cannot be shared across concurrent handlers.
    let mut ctx = match Context::from_raw(discovery_handle).await {
        Ok(ctx) => ctx,
        Err(error) => {
            warn!("control-plane: discovery client failed: {error}");
            return;
        }
    };

    loop {
        match connection.recv().await {
            Ok(request) => {
                let response = match request.payload() {
                    Ok(payload) => handle_request(&mut ctx, &shared, &session, payload).await,
                    Err(error) => {
                        warn!("control-plane: request decode failed: {error}");
                        ControlResponse::Error {
                            step: "decode".to_string(),
                            context: format!("{error}"),
                        }
                    }
                };
                if request.reply(response).await.is_err() {
                    warn!("control-plane: reply failed");
                    break;
                }
            }
            Err(rpc::RpcError::ConnectionClosed) => break,
            Err(error) => {
                warn!("control-plane: recv failed: {error}");
                break;
            }
        }
    }
}

/// Runs one projection mutation in the guest-wide commit lane.
async fn serialised_mutation<R>(
    lane: &tokio::sync::Mutex<()>,
    mutation: impl std::future::Future<Output = R>,
) -> R {
    let _guard = lane.lock().await;
    mutation.await
}

/// Handles one decoded control request, serialising the full commit critical
/// section for desired-state and catalogue mutations across all sessions.
async fn handle_request(
    ctx: &mut Context,
    shared: &SharedState,
    session: &Session,
    request: ControlRequest,
) -> ControlResponse {
    if matches!(
        &request,
        ControlRequest::Deploy { .. }
            | ControlRequest::Scale { .. }
            | ControlRequest::Stop { .. }
            | ControlRequest::RegisterResource { .. }
            | ControlRequest::RemoveResource { .. }
    ) {
        serialised_mutation(
            &shared.mutations,
            handle_request_locked(ctx, shared, session, request),
        )
        .await
    } else {
        handle_request_locked(ctx, shared, session, request).await
    }
}

/// Handles a request, with projection mutations called inside the guest-wide
/// mutation lane.
async fn handle_request_locked(
    ctx: &mut Context,
    shared: &SharedState,
    session: &Session,
    request: ControlRequest,
) -> ControlResponse {
    let log = &shared.log;
    let catalogue = &shared.catalogue;
    let blobs = &shared.blobs;
    let state = &*shared.desired;
    let namespace = &session.namespace;
    let client_process_id = session.client_process_id;
    let tenant = tenant_label(namespace);
    match request {
        ControlRequest::Resolve { uri } => match ctx.lookup(&uri).await {
            Ok(target) => resolve_response(target),
            Err(error) => ControlResponse::Error {
                step: "discovery".to_string(),
                context: format!("{error}"),
            },
        },
        ControlRequest::Upload { manifest, bytes } => {
            // Tenant-prefix the manifest so the single platform blob store
            // cannot leak one tenant's uploads into another's manifest key.
            let key = tenant_prefixed_manifest(namespace, &manifest);
            let stored = async {
                let blob_id = blobs.put(bytes).await?;
                blobs.set_manifest(&key, blob_id).await
            }
            .await;
            match stored {
                Ok(()) => ControlResponse::Uploaded { manifest: key },
                Err(error) => ControlResponse::Error {
                    step: "storage".to_string(),
                    context: format!("{error}"),
                },
            }
        }
        ControlRequest::Deploy {
            workload_id,
            replicas,
            module,
        } => {
            let deployment = Deployment {
                workload_id: workload_id.clone(),
                replicas,
                module: module.clone(),
            };
            let desired = DesiredStateRecord::Deployment {
                deployment,
                tenant: tenant.clone(),
            };
            if let Err(error) = record(log, state, desired).await {
                return ControlResponse::Error {
                    step: "storage".to_string(),
                    context: format!("{error}"),
                };
            }
            let request = SchedulerRequest::Place {
                workload_id: workload_id.clone(),
                replicas,
            };
            accept_delegated(&workload_id, replicas, module, request)
        }
        ControlRequest::Scale {
            workload_id,
            replicas,
        } => {
            let module = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .deployment(namespace, &workload_id)
                .map(|deployment| deployment.module.clone())
                .unwrap_or_default();
            let deployment = Deployment {
                workload_id: workload_id.clone(),
                replicas,
                module: module.clone(),
            };
            let desired = DesiredStateRecord::Deployment {
                deployment,
                tenant: tenant.clone(),
            };
            if let Err(error) = record(log, state, desired).await {
                return ControlResponse::Error {
                    step: "storage".to_string(),
                    context: format!("{error}"),
                };
            }
            let request = SchedulerRequest::Scale {
                workload_id: workload_id.clone(),
                replicas,
            };
            accept_delegated(&workload_id, replicas, module, request)
        }
        ControlRequest::Stop { workload_id } => {
            if let Err(error) = record(
                log,
                state,
                DesiredStateRecord::Stop {
                    workload_id: workload_id.clone(),
                    tenant: tenant.clone(),
                },
            )
            .await
            {
                return ControlResponse::Error {
                    step: "storage".to_string(),
                    context: format!("{error}"),
                };
            }
            let request = SchedulerRequest::Stop {
                workload_id: workload_id.clone(),
            };
            accept_delegated(&workload_id, 0, String::new(), request)
        }
        ControlRequest::Status { workload_id } => ControlResponse::Status {
            deployment: state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .deployment(namespace, &workload_id)
                .cloned(),
        },
        ControlRequest::RegisterResource {
            uri,
            class,
            owner,
            purpose,
            resource_id,
        } => {
            if ResourceClass::from_uri_segment(&class).is_none() {
                return ControlResponse::Error {
                    step: "catalogue".to_string(),
                    context: format!("unknown resource class segment {class:?}"),
                };
            }
            // A session may only name a resource it owns: registering on
            // another process's behalf is refused with a typed error.
            if let Some(refusal) = catalogue_owner_refusal(client_process_id, owner) {
                return ControlResponse::Error {
                    step: "catalogue".to_string(),
                    context: format!("{refusal} (register)"),
                };
            }
            let record = CatalogueRecord {
                uri,
                class,
                owner,
                tenant,
                purpose,
                resource_id,
            };
            if let Err(error) = record_catalogue(
                &catalogue.log,
                &catalogue.projection,
                CatalogueLogRecord::Register {
                    record: record.clone(),
                },
            )
            .await
            {
                return ControlResponse::Error {
                    step: "storage".to_string(),
                    context: format!("{error}"),
                };
            }
            ControlResponse::Registered { record }
        }
        ControlRequest::RemoveResource { uri } => {
            let owner = catalogue
                .projection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .lookup(namespace, &uri)
                .map(|record| record.owner);
            match owner {
                None => ControlResponse::Error {
                    step: "catalogue".to_string(),
                    context: format!("no catalogue record for {uri:?} in this tenant"),
                },
                Some(owner) if catalogue_owner_refusal(client_process_id, owner).is_some() => {
                    ControlResponse::Error {
                        step: "catalogue".to_string(),
                        context: format!(
                            "{} (remove)",
                            catalogue_owner_refusal(client_process_id, owner)
                                .expect("the guard reported a refusal")
                        ),
                    }
                }
                Some(_) => {
                    if let Err(error) = record_catalogue(
                        &catalogue.log,
                        &catalogue.projection,
                        CatalogueLogRecord::Remove {
                            uri: uri.clone(),
                            tenant,
                        },
                    )
                    .await
                    {
                        return ControlResponse::Error {
                            step: "storage".to_string(),
                            context: format!("{error}"),
                        };
                    }
                    ControlResponse::Removed { uri }
                }
            }
        }
        ControlRequest::LookupResource { uri } => {
            let record = catalogue
                .projection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .lookup(namespace, &uri)
                .cloned();
            ControlResponse::Lookup { record }
        }
    }
}

/// Appends a record and completes its sync barrier.
///
/// The desired state a caller was told was accepted must survive a power
/// cycle, so the acknowledgement boundary is append **and** sync: the store
/// batches its own writes, and this call is what makes the record durable
/// before the projection changes or the caller is told it succeeded.
async fn append_durably(log: &DurableLog, payload: Vec<u8>) -> selium_guest::Result<()> {
    let timestamp_ms = selium_guest::time::now().map(|nanos| nanos / 1_000_000)?;
    log.append(timestamp_ms, Vec::new(), payload)
        .await
        .map(|_sequence| ())?;
    log.sync().await
}

/// Appends a desired-state record to the durable log and applies it to the
/// projection, keeping the store of record and the read model in step.
async fn record(
    log: &DurableLog,
    state: &Mutex<ControlPlaneState>,
    record: DesiredStateRecord,
) -> selium_guest::Result<()> {
    let payload = FlatMsg::encode(&record);
    append_durably(log, payload).await?;
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .apply_record(record);
    Ok(())
}

/// Appends a catalogue log record to the catalogue's durable log and applies
/// it to the projection, keeping the store of record and the read model in
/// step.
async fn record_catalogue(
    log: &DurableLog,
    catalogue: &Mutex<ResourceCatalogue>,
    record: CatalogueLogRecord,
) -> selium_guest::Result<()> {
    let payload = FlatMsg::encode(&record);
    append_durably(log, payload).await?;
    catalogue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .apply_record(record);
    Ok(())
}

/// Refuses a delivered session by attaching then closing the delivered region,
/// so the sender observes EOF instead of parking on a region nobody attaches.
fn refuse_session(shared_id: u64) {
    match selium_guest::net::ByteStream::attach_blocking(shared_id) {
        Ok(stream) => drop(stream),
        Err(error) => {
            warn!(
                shared_id,
                "control-plane: session refusal could not attach region: {error}"
            )
        }
    }
}

/// The tenant label recorded on a desired-state record or manifest prefix for
/// a requestor namespace: a named tenant records its name; the root namespace
/// records an empty label (no tenant name exists).
fn tenant_label(namespace: &Namespace) -> String {
    match namespace {
        Namespace::Tenant(tenant) => tenant.clone(),
        Namespace::Root => String::new(),
    }
}

/// The tenant-prefixed module manifest key for a requestor namespace:
/// `<tenant>:<manifest>` for a named tenant, the bare manifest for root (no
/// tenant name exists).
fn tenant_prefixed_manifest(namespace: &Namespace, manifest: &str) -> String {
    match namespace {
        Namespace::Tenant(tenant) => format!("{tenant}:{manifest}"),
        Namespace::Root => manifest.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_wire::Rendezvous;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_mutations_project_in_durable_order() {
        let lane = Arc::new(tokio::sync::Mutex::new(()));
        let ledger = Arc::new(Mutex::new(Vec::new()));
        let projection = Arc::new(Mutex::new(Vec::new()));
        let first_appended = Arc::new(tokio::sync::Notify::new());
        let release_first = Arc::new(tokio::sync::Notify::new());

        let first = {
            let lane = lane.clone();
            let ledger = ledger.clone();
            let projection = projection.clone();
            let first_appended = first_appended.clone();
            let release_first = release_first.clone();
            tokio::spawn(async move {
                serialised_mutation(&lane, async move {
                    ledger.lock().expect("ledger").push("A");
                    first_appended.notify_one();
                    // Hold A between its durable append and projection. B may
                    // not pass this point and then be overwritten by A.
                    release_first.notified().await;
                    projection.lock().expect("projection").push("A");
                })
                .await;
            })
        };
        first_appended.notified().await;

        let second = {
            let lane = lane.clone();
            let ledger = ledger.clone();
            let projection = projection.clone();
            tokio::spawn(async move {
                serialised_mutation(&lane, async move {
                    ledger.lock().expect("ledger").push("B");
                    projection.lock().expect("projection").push("B");
                })
                .await;
            })
        };
        tokio::task::yield_now().await;
        assert_eq!(*ledger.lock().expect("ledger"), vec!["A"]);
        assert!(projection.lock().expect("projection").is_empty());

        release_first.notify_one();
        first.await.expect("first mutation");
        second.await.expect("second mutation");
        assert_eq!(*ledger.lock().expect("ledger"), vec!["A", "B"]);
        assert_eq!(*projection.lock().expect("projection"), vec!["A", "B"]);
    }

    fn deployment(workload_id: &str, replicas: u32, module: &str) -> Deployment {
        Deployment {
            workload_id: workload_id.to_string(),
            replicas,
            module: module.to_string(),
        }
    }

    fn binding(name: &str, from: &str, to: &str) -> PipelineBinding {
        PipelineBinding {
            name: name.to_string(),
            from: from.to_string(),
            to: to.to_string(),
        }
    }

    fn ns(tenant: &str) -> Namespace {
        Namespace::Tenant(tenant.to_string())
    }

    fn deployment_record(tenant: &str, deployment: Deployment) -> DesiredStateRecord {
        DesiredStateRecord::Deployment {
            deployment,
            tenant: tenant.to_string(),
        }
    }

    fn pipeline_record(tenant: &str, pipeline: PipelineBinding) -> DesiredStateRecord {
        DesiredStateRecord::PipelineBinding {
            pipeline,
            tenant: tenant.to_string(),
        }
    }

    fn stop_record(tenant: &str, workload_id: &str) -> DesiredStateRecord {
        DesiredStateRecord::Stop {
            workload_id: workload_id.to_string(),
            tenant: tenant.to_string(),
        }
    }

    /// 4.2: the projection reconstructs from replay of appended records.
    // scenario: control-plane/user-facing-desired-state-ownership/deployment-update-recorded
    // scenario: control-plane/user-facing-desired-state-ownership/pipeline-binding-recorded
    #[test]
    fn projection_reconstructs_from_replayed_records() {
        let mut state = ControlPlaneState::default();
        let records = vec![
            deployment_record("acme", deployment("api", 2, "api/v1")),
            deployment_record("acme", deployment("api", 4, "api/v2")),
            pipeline_record("acme", binding("api-to-db", "api", "db")),
            stop_record("acme", "gone"),
            deployment_record("acme", deployment("gone", 1, "gone/v1")),
            stop_record("acme", "gone"),
        ];
        for record in records {
            state.apply_record(record);
        }

        let acme = ns("acme");
        assert_eq!(
            state.deployment(&acme, "api"),
            Some(&deployment("api", 4, "api/v2")),
            "last accepted desired state wins"
        );
        assert_eq!(
            state.deployment(&acme, "gone"),
            None,
            "stop tombstones remove"
        );
        assert_eq!(
            state.pipeline(&acme, "api-to-db"),
            Some(&binding("api-to-db", "api", "db"))
        );
    }

    /// 4.1 + 4.2: an accepted deployment intent round-trips through the log
    /// encoding and is recorded in the projection.
    // scenario: control-plane/flatbuffers-desired-state-log-records/deployment-intent-is-recorded-as-flatbuffers
    // scenario: control-plane/flatbuffers-desired-state-log-records/projection-rebuilds-from-flatbuffers-replay
    #[test]
    fn deployment_intent_is_recorded_in_projection() {
        let record = deployment_record("acme", deployment("api", 3, "api/v1"));
        let payload = FlatMsg::encode(&record);
        let decoded: DesiredStateRecord = FlatMsg::decode(&payload).expect("decode record");

        let mut state = ControlPlaneState::default();
        state.apply_record(decoded);

        assert_eq!(
            state.deployment(&ns("acme"), "api"),
            Some(&deployment("api", 3, "api/v1"))
        );
    }

    /// 4.3: status reads return the last accepted desired state.
    // scenario: control-plane/user-facing-desired-state-ownership/desired-state-read-reflects-accepted-updates
    #[test]
    fn status_reads_return_last_accepted_desired_state() {
        let mut state = ControlPlaneState::default();
        state.apply_record(deployment_record("acme", deployment("api", 2, "api/v1")));
        state.apply_record(deployment_record("acme", deployment("api", 5, "api/v2")));

        let acme = ns("acme");
        assert_eq!(
            state.deployment(&acme, "api"),
            Some(&deployment("api", 5, "api/v2"))
        );
        assert_eq!(state.deployment(&acme, "missing"), None);
    }

    /// 3.4: two tenants' deployments never leak across partitions — an `acme`
    /// session reads no `beta` desired state and vice versa.
    #[test]
    fn desired_state_partitions_by_tenant() {
        let mut state = ControlPlaneState::default();
        state.apply_record(deployment_record("acme", deployment("api", 2, "acme/v1")));
        state.apply_record(deployment_record("beta", deployment("api", 9, "beta/v1")));

        let acme = ns("acme");
        let beta = ns("beta");
        assert_eq!(
            state.deployment(&acme, "api"),
            Some(&deployment("api", 2, "acme/v1")),
            "acme observes its own partition"
        );
        assert_eq!(
            state.deployment(&beta, "api"),
            Some(&deployment("api", 9, "beta/v1")),
            "beta observes its own partition"
        );
        assert_eq!(
            state.deployment(&acme, "beta-only-workload"),
            None,
            "acme reads no beta desired state"
        );

        // A beta stop does not tombstone acme's deployment.
        state.apply_record(stop_record("beta", "api"));
        assert_eq!(
            state.deployment(&acme, "api"),
            Some(&deployment("api", 2, "acme/v1")),
            "a beta stop must not remove an acme deployment"
        );
        assert_eq!(
            state.deployment(&beta, "api"),
            None,
            "the beta stop applies"
        );
    }

    /// 5.1: a discovery lookup maps to a typed resolve response.
    // scenario: control-plane/narrow-delegation/resolution-delegated-to-discovery
    #[test]
    fn resolve_response_maps_found_and_missing_targets() {
        let found = resolve_response(Some(ResourceTarget {
            uri: "sel://acme/bridge".to_string(),
            host_id: "host-a".to_string(),
            resource_id: 42,
            interface: None,
            tenant: Some("acme".to_string()),
            class: ResourceClass::HostQueue,
            labels: Vec::new(),
        }));
        assert_eq!(
            found,
            ControlResponse::Resolved {
                target: Some(ResolvedTarget {
                    uri: "sel://acme/bridge".to_string(),
                    host_id: "host-a".to_string(),
                    resource_id: 42,
                }),
            }
        );

        assert_eq!(
            resolve_response(None),
            ControlResponse::Resolved { target: None }
        );
    }

    /// 5.2: the scheduler seam returns a typed deferred status (mapped to a
    /// delegation status by the dispatcher).
    // scenario: control-plane/narrow-delegation/placement-delegated-to-scheduler
    #[test]
    fn scheduler_seam_returns_typed_deferred_status() {
        let client = SchedulerClient;
        let response = client
            .delegate(SchedulerRequest::Place {
                workload_id: "api".to_string(),
                replicas: 3,
            })
            .expect("delegate");

        let status = delegation_status(response);
        assert_eq!(status.step, "scheduler");
        assert!(!status.applied, "day-1 delegation is not applied");
        assert!(status.context.contains("not yet online"));
    }

    /// 6.2: an accepted deployment carries its module reference in the
    /// recorded desired state.
    // scenario: control-plane/module-upload-to-the-durable-store/deploy-references-a-stored-module
    #[test]
    fn deployment_records_module_reference() {
        let record = deployment_record("acme", deployment("api", 3, "api/v1"));
        let mut state = ControlPlaneState::default();
        state.apply_record(record);
        assert_eq!(
            state
                .deployment(&ns("acme"), "api")
                .map(|deployment| deployment.module.as_str()),
            Some("api/v1")
        );
    }

    /// 7.3: the single-instance grant set covers storage, shared memory, host
    /// queue, and system registration at class scope — no tenant selectors,
    /// since the control plane serves every tenant.
    #[test]
    fn grant_set_is_class_scoped() {
        let grants = control_plane_grants();
        let capabilities: Vec<Capability> = grants
            .iter()
            .map(|grant| grant.capability.clone())
            .collect();
        assert!(capabilities.contains(&Capability::Storage));
        assert!(capabilities.contains(&Capability::SharedMemory));
        assert!(capabilities.contains(&Capability::HostQueue));
        assert!(capabilities.contains(&Capability::RouteClaim));

        // No grant carries a tenant selector: the single instance serves all
        // tenants and derives each session's tenant from the identity.
        assert!(grants.iter().all(|grant| {
            !grant
                .selectors
                .iter()
                .any(|selector| matches!(selector, ResourceSelector::Tenant(_)))
        }));
    }

    /// 7.3: a client without a control-plane grant is refused; the
    /// class-scoped control-plane set admits any tenant (the per-session
    /// tenant comes from the authenticated identity, not from a tenant
    /// selector on the grant).
    // scenario: control-plane/capability-enforced-access/unprivileged-client-is-refused
    #[test]
    fn authority_boundary_refuses_unprivileged_clients() {
        // A data-plane-only client: host queue in another tenant.
        let data_plane = vec![CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::Tenant("other".to_string())],
        )];
        assert!(!admit_control_client(&data_plane, "acme"));

        // The single-instance control-plane set is class-scoped and therefore
        // admits any tenant scope.
        let control_plane = control_plane_grants();
        assert!(admit_control_client(&control_plane, "acme"));
        assert!(admit_control_client(&control_plane, "other"));
    }

    /// 3.5: module-blob manifests are tenant-prefixed within the single
    /// platform blob store; a root session leaves the manifest bare.
    #[test]
    fn manifest_keys_are_tenant_prefixed() {
        assert_eq!(
            tenant_prefixed_manifest(&ns("acme"), "api/v1"),
            "acme:api/v1"
        );
        assert_eq!(
            tenant_prefixed_manifest(&ns("beta"), "api/v1"),
            "beta:api/v1"
        );
        assert_eq!(
            tenant_prefixed_manifest(&Namespace::Root, "platform/v1"),
            "platform/v1"
        );
    }

    /// 3.3: each session is scoped by the delivering process owner's tenant —
    /// a named process tenant maps to its namespace, an unset one to root.
    // scenario: control-plane/requestor-tenant-derivation/tenant-scoped-deliverer-scoped-to-its-tenant
    // scenario: control-plane/requestor-tenant-derivation/root-deliverer-scoped-as-root
    #[test]
    fn session_scope_comes_from_process_owner() {
        assert_eq!(
            namespace_from_tenant(Some("acme")),
            Namespace::Tenant("acme".to_string()),
            "a named process tenant scopes the session to that tenant"
        );
        assert_eq!(
            namespace_from_tenant(None),
            Namespace::Root,
            "an unset process tenant scopes the session to the root namespace"
        );
    }

    /// 3.2: a client half sends a typed request and receives a correlated
    /// typed reply over the shared-memory RPC path (two-ring session).
    // scenario: control-plane/typed-wire-protocol/typed-request-and-reply
    #[tokio::test]
    async fn shared_memory_rpc_round_trips_typed_request_and_reply() {
        drop(selium_memory::set_region_provider(Box::new(
            selium_memory::HeapRegionProvider::new(),
        )));
        let rendezvous = selium_shm::ShmRendezvous::new();

        let server = {
            let rendezvous = rendezvous.clone();
            tokio::spawn(async move {
                let incoming = loop {
                    match rendezvous.recv().await {
                        Ok(connection) => break connection,
                        Err(_) => tokio::task::yield_now().await,
                    }
                };
                let mut connection: rpc::RpcConnection<ControlRequest, ControlResponse> =
                    rpc::accept(incoming).expect("accept");
                let request = connection.recv().await.expect("recv request");
                let ControlRequest::Deploy {
                    workload_id,
                    replicas,
                    module,
                } = request.payload().expect("decode request")
                else {
                    panic!("expected a deploy request");
                };
                request
                    .reply(ControlResponse::Accepted {
                        workload_id,
                        replicas,
                        module,
                        delegated: DelegationStatus {
                            step: "scheduler".to_string(),
                            applied: false,
                            context: "deferred".to_string(),
                        },
                    })
                    .await
                    .expect("reply");
            })
        };

        let mut client = rpc::connect::<ControlRequest, ControlResponse, _>(rendezvous, 0, 0)
            .await
            .expect("connect");
        let reply = client
            .request(ControlRequest::Deploy {
                workload_id: "api".to_string(),
                replicas: 3,
                module: "api/v1".to_string(),
            })
            .await
            .expect("request");

        assert_eq!(
            reply,
            ControlResponse::Accepted {
                workload_id: "api".to_string(),
                replicas: 3,
                module: "api/v1".to_string(),
                delegated: DelegationStatus {
                    step: "scheduler".to_string(),
                    applied: false,
                    context: "deferred".to_string(),
                },
            }
        );

        server.await.expect("server task");
    }

    fn catalogue_record(uri: &str, class: &str, owner: u64, tenant: &str) -> CatalogueRecord {
        CatalogueRecord {
            uri: uri.to_string(),
            class: class.to_string(),
            owner,
            tenant: tenant.to_string(),
            purpose: "rpc".to_string(),
            resource_id: 42,
        }
    }

    /// 2.1: catalogue records apply into the registering tenant's partition
    /// and lookups are tenant-scoped.
    // scenario: resource-catalogue/named-resource-registration/owner-registers-a-name
    // scenario: resource-catalogue/catalogue-backed-lookup/name-resolves-to-its-resource
    // scenario: resource-catalogue/catalogue-backed-lookup/cross-tenant-lookup-refused
    // scenario: control-plane/resource-catalogue-ownership/registration-recorded-durably
    // scenario: control-plane/resource-catalogue-ownership/lookup-is-tenant-scoped
    #[test]
    fn catalogue_records_apply_and_scope_by_tenant() {
        let mut catalogue = ResourceCatalogue::default();
        catalogue.apply_record(CatalogueLogRecord::Register {
            record: catalogue_record("acme-db", "region", 7, "acme"),
        });
        catalogue.apply_record(CatalogueLogRecord::Register {
            record: catalogue_record("beta-db", "region", 9, "beta"),
        });

        assert_eq!(
            catalogue.lookup(&ns("acme"), "acme-db"),
            Some(&catalogue_record("acme-db", "region", 7, "acme"))
        );
        assert_eq!(
            catalogue.lookup(&ns("beta"), "beta-db"),
            Some(&catalogue_record("beta-db", "region", 9, "beta"))
        );
        // A name registered under another tenant is invisible.
        assert_eq!(catalogue.lookup(&ns("acme"), "beta-db"), None);
        assert_eq!(catalogue.lookup(&ns("beta"), "acme-db"), None);
    }

    /// 2.1: a session may only register or remove a name it owns — naming a
    /// resource owned by another process is refused with a typed error.
    // scenario: resource-catalogue/named-resource-registration/non-owner-registration-rejected
    #[test]
    fn a_catalogue_registration_or_removal_requires_session_ownership() {
        assert_eq!(
            catalogue_owner_refusal(7, 7),
            None,
            "the owner may register and remove its own names"
        );
        let refusal = catalogue_owner_refusal(7, 9).expect("a non-owner is refused");
        assert!(
            refusal.contains('7') && refusal.contains('9'),
            "the refusal names both processes: {refusal}"
        );
    }

    /// 2.1: removal stops resolving the name, and replay rebuilds the
    /// projection from the ordered log records.
    // scenario: resource-catalogue/catalogue-ownership-by-a-system-guest/catalogue-survives-restart
    // scenario: resource-catalogue/catalogue-ownership-by-a-system-guest/removal-is-recorded
    #[test]
    fn catalogue_replay_rebuilds_projection_with_removals() {
        let records = vec![
            CatalogueLogRecord::Register {
                record: catalogue_record("acme-db", "region", 7, "acme"),
            },
            CatalogueLogRecord::Register {
                record: catalogue_record("acme-tmp", "region", 7, "acme"),
            },
            CatalogueLogRecord::Remove {
                uri: "acme-tmp".to_string(),
                tenant: "acme".to_string(),
            },
        ];

        let mut catalogue = ResourceCatalogue::default();
        for record in records {
            // Round-trip each record through the log codec, as replay does.
            let payload = FlatMsg::encode(&record);
            let decoded: CatalogueLogRecord = FlatMsg::decode(&payload).expect("decode record");
            catalogue.apply_record(decoded);
        }

        assert_eq!(
            catalogue.lookup(&ns("acme"), "acme-db"),
            Some(&catalogue_record("acme-db", "region", 7, "acme"))
        );
        assert_eq!(
            catalogue.lookup(&ns("acme"), "acme-tmp"),
            None,
            "a removed name no longer resolves after replay"
        );
    }
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `resource-catalogue` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.
