use std::time::Duration;

use selium_abi::{
    HostcallOutput, HostcallRequest, RegionAllocation, RegionProt, ResourceClass, ResourceKind,
    uri::{self, DomainTable},
};
use selium_service::{
    CatalogueRecord, ControlRequest, ControlResponse, DiscoveryRequest, DiscoveryResponse,
    REGISTRATION_LEASE_TTL_MS, ResourceTarget,
};
use selium_shm::{
    Channel,
    rpc::{self, OwnedRpcClient},
    transport::ShmTransport,
};
use selium_wire::{LiveTableView, framed::FramedRead, pubsub::Subscriber};

use crate::{
    GuestError,
    control::{self, CONTROL_ROUTE, ControlClient},
    hostcall::hostcall_ready,
    resource::ResourceSender,
    store::{self, BlobStore, DurableLog, STORE_ROUTE, StoreClient},
    watch::{ROUTE_TABLE_ROUTE, RouteEvent, RouteWatch},
};

/// How long a serve-side guest waits for a protocol-handler claim to be
/// published before treating it as absent.
const HANDLER_RESOLVE_BOUND: Duration = Duration::from_secs(60);
/// The root-namespace route of the discovery RPC listener.
pub const DISCOVERY_ROUTE: &str = "sel:///discovery";
/// RPC ring capacity for discovery replies.
const RPC_REP_CAPACITY: u64 = 4096;
/// RPC ring capacity for discovery requests.
const RPC_REQ_CAPACITY: u64 = 4096;
/// How long a boot-time platform-service lookup converges before failing.
///
/// System services publish their own routes from their entrypoints, so a
/// consumer that boots alongside one waits on the route watch rather than
/// treating an early miss as permanent. The bound is only a failure backstop.
/// How long a guest waits for a boot-order dependency's route (the store or
/// the control plane) before failing loudly. The platform is interpreted, so
/// a cold boot behind a loaded host can legitimately take a minute; the bound
/// still turns a genuinely absent service into an error instead of a hang.
const SERVICE_READY_BOUND: Duration = Duration::from_secs(120);

/// Declaration of a named service route: a path plus the target to map it to,
/// with an optional root-service designation for apex aliasing.
#[derive(Debug, Clone)]
pub struct Serve {
    /// The internal path segments, e.g. `["bridge"]` or `["http", "prod"]`.
    /// Every segment must project to a DNS-safe wire label.
    pub path: Vec<String>,
    /// The resource the route maps to (typically the guest's own listener
    /// queue).
    pub target: ResourceTarget,
    /// Designate this path as the tenant's root service, so the bare domain
    /// (apex) resolves to it.
    pub default: bool,
}

/// Guest context injected by the runtime during bootstrap.
///
/// Provides a pre-connected discovery client for URI resolution via RPC
/// over shared-memory ring buffers.
pub struct Context {
    /// The raw discovery host-queue handle the context was built from. Kept
    /// so a guest can hand the handle onward (e.g. to a spawned child) after
    /// constructing a [`Context`] through the entrypoint macro.
    handle: u64,
    client: OwnedRpcClient<DiscoveryRequest, DiscoveryResponse>,
    /// Already-attached discovery queue, cloned for lease-renewal sessions.
    discovery_sender: ResourceSender,
    /// Client of the durable store service, connected lazily on first use.
    store_client: Option<StoreClient>,
    /// Client of the control-plane resource catalogue, connected lazily on
    /// first use.
    control_client: Option<ControlClient>,
    /// Discovery route-table attachment shared by every route watch. Each
    /// watch receives an independent ring handle, not another memory attach.
    route_table: Option<(u64, Channel)>,
    /// URIs this context has already started a lease renewal for, so repeated
    /// registrations of one route keep a single renewal task.
    renewing: std::collections::BTreeSet<String>,
}

impl Context {
    /// Returns a mutable reference to the pre-connected discovery RPC client.
    ///
    /// Use this to send custom discovery requests beyond the convenience
    /// `lookup()` method.
    pub fn discovery(&mut self) -> &mut OwnedRpcClient<DiscoveryRequest, DiscoveryResponse> {
        &mut self.client
    }

    /// Creates a Context from a raw discovery handle.
    ///
    /// Attaches a `ResourceSender` to the discovery host queue and creates
    /// an `RpcClient` for discovery requests.
    pub async fn from_raw(discovery_handle: u64) -> Result<Self, GuestError> {
        let sender = ResourceSender::attach(discovery_handle)?;
        Self::from_attached_sender(discovery_handle, sender).await
    }

    async fn from_attached_sender(
        discovery_handle: u64,
        sender: ResourceSender,
    ) -> Result<Self, GuestError> {
        let client = rpc::connect(sender.clone(), RPC_REQ_CAPACITY, RPC_REP_CAPACITY)
            .await
            .map_err(|e| GuestError::from(e).context("create RPC client"))?;

        Ok(Self {
            handle: discovery_handle,
            client,
            discovery_sender: sender,
            store_client: None,
            control_client: None,
            route_table: None,
            renewing: std::collections::BTreeSet::new(),
        })
    }

    /// Reconnects after a discovery session failure using the host's current
    /// listener resolution. A dead discovery service cannot resolve its own
    /// replacement through the old RPC session or its replaced route table.
    pub async fn rediscover() -> Result<Self, GuestError> {
        Self::from_raw(crate::hostcall::discovery_listener()?).await
    }

    /// Publishes the canonical typed record `sel://<tenant>/<class>/<id>` for
    /// a resource this guest owns.
    ///
    /// Admission validates the URI against the host's authority record: the
    /// caller must own `(class, id)` and the URI's tenant must be the tenant
    /// the host minted it under. Publication is optional — a private resource
    /// that is never published stays fully owned and accounted for, merely
    /// undiscoverable.
    pub async fn publish_canonical(
        &mut self,
        target: &ResourceTarget,
    ) -> Result<String, GuestError> {
        let authority = crate::hostcall::resource_authority(
            crate::process_id(),
            target.class.clone(),
            target.resource_id,
        )?;
        if !authority.owned {
            return Err(GuestError::Host(format!(
                "cannot publish canonical record for {:?} {}: caller does not own it",
                target.class, target.resource_id
            )));
        }
        let tenant = authority.tenant.unwrap_or_default();
        let uri = uri::resource_uri(&tenant, target.class.clone(), target.resource_id);
        let mut canonical = target.clone();
        canonical.uri = uri.clone();
        canonical.tenant = (!tenant.is_empty()).then_some(tenant);
        self.register_inner(&uri, canonical, false).await?;
        Ok(uri)
    }

    /// Publishes this process's own node (`sel://<tenant>/proc/<id>`).
    ///
    /// The process is the owner of its node, so the host authority attests the
    /// publication. A process that never publishes stays undiscoverable while
    /// remaining fully accounted for by the host.
    pub async fn publish_process_node(&mut self) {
        let process_id = crate::process_id();
        let target = ResourceTarget {
            uri: String::new(),
            host_id: String::new(),
            resource_id: process_id,
            interface: None,
            tenant: None,
            class: ResourceClass::Process,
            labels: Vec::new(),
        };
        if let Err(error) = self.publish_canonical(&target).await {
            crate::warn!("process node publication failed for {process_id}: {error}");
        }
    }

    /// Publishes the protocol-handler routes this process holds host-issued
    /// claims for.
    ///
    /// Handler names are reserved (`sel:///handler/<scheme>`); the host
    /// provisions the claim, and this guest publishes the route under it. The
    /// route resolves to the handler process, which is exactly what a
    /// serve-side guest pins its listener to.
    pub async fn publish_handler_claims(&mut self) {
        let process_id = crate::process_id();
        let claims = match crate::hostcall::expected_claims(process_id) {
            Ok(claims) => claims,
            Err(error) => {
                crate::warn!("handler claim lookup failed for {process_id}: {error}");
                return;
            }
        };
        crate::info!(
            "process {process_id} has {} expected route claims",
            claims.len()
        );
        for uri in claims {
            if !uri.contains("/handler/") {
                // Platform routes are served by their guest with the listener
                // target, not by this generic handler publication.
                continue;
            }
            let target = ResourceTarget {
                uri: uri.clone(),
                host_id: String::new(),
                resource_id: process_id,
                interface: None,
                tenant: None,
                class: ResourceClass::Process,
                labels: Vec::new(),
            };
            if let Err(error) = self.register_inner(&uri, target, false).await {
                crate::warn!("handler claim publication failed for {uri}: {error}");
            }
        }
    }

    /// Returns a client of the durable store service, connecting on first use.
    ///
    /// The store route is resolved for the caller, so a guest never has to
    /// know it. The store publishes its own route from its entrypoint, so this
    /// converges for a bounded window before failing with a typed error.
    pub async fn store(&mut self) -> Result<StoreClient, GuestError> {
        if let Some(client) = self.store_client.clone() {
            return Ok(client);
        }
        let target = self.resolve_ready(STORE_ROUTE, SERVICE_READY_BOUND).await?;
        let sender = ResourceSender::attach(target.resource_id)?;
        let client = store::connect(sender).await?;
        self.store_client = Some(client.clone());
        Ok(client)
    }

    /// Opens or creates a durable log through the store service.
    pub async fn open_log(&mut self, name: impl Into<String>) -> Result<DurableLog, GuestError> {
        let client = self.store().await?;
        DurableLog::open(client, name).await
    }

    /// Opens or creates a blob store through the store service.
    pub async fn open_blob_store(
        &mut self,
        name: impl Into<String>,
    ) -> Result<BlobStore, GuestError> {
        let client = self.store().await?;
        BlobStore::open(client, name).await
    }

    /// Returns a client of the control-plane resource catalogue, connecting
    /// on first use.
    ///
    /// The control route is resolved for the caller, so a guest never has to
    /// know it. The control plane publishes its own route from its entrypoint,
    /// so this converges for a bounded window before failing with a typed
    /// error; name resolution is gated on the catalogue, allocation is not.
    pub async fn catalogue(&mut self) -> Result<ControlClient, GuestError> {
        if let Some(client) = self.control_client.clone() {
            return Ok(client);
        }
        let target = self
            .resolve_ready(CONTROL_ROUTE, SERVICE_READY_BOUND)
            .await?;
        let sender = ResourceSender::attach(target.resource_id)?;
        let client = control::connect(sender).await?;
        self.control_client = Some(client.clone());
        Ok(client)
    }

    /// Allocates a shared region and registers a name for it with the resource
    /// catalogue.
    ///
    /// Allocation is a host primitive (the `AllocRegion` hostcall) and stays
    /// independent of the catalogue; only the name registration is an RPC, so
    /// an unreachable catalogue never blocks allocation itself. The region is
    /// registered under the calling guest's tenant and its own process as
    /// owner.
    pub async fn create_region(
        &mut self,
        name: impl Into<String>,
        pages: u32,
        purpose: ResourceKind,
    ) -> Result<RegionAllocation, GuestError> {
        // Hot path: the host allocates the region; no catalogue dependency.
        let allocation = match hostcall_ready(HostcallRequest::AllocRegion {
            pages,
            prot: RegionProt::ReadWrite,
            purpose,
            serving_tenant: None,
        })? {
            HostcallOutput::RegionAlloc(allocation) => allocation,
            other => {
                return Err(GuestError::Host(format!(
                    "unexpected hostcall output for AllocRegion: {other:?}"
                )));
            }
        };

        let name = name.into();
        let client = self.catalogue().await?;
        let reply = client
            .request(ControlRequest::RegisterResource {
                uri: name,
                class: ResourceClass::SharedRegion.uri_segment().to_string(),
                owner: crate::process_id(),
                purpose: format!("{purpose:?}"),
                resource_id: allocation.region_id,
            })
            .await?;
        match reply {
            ControlResponse::Registered { .. } => Ok(allocation),
            other => Err(GuestError::Host(format!(
                "unexpected catalogue registration reply: {other:?}"
            ))),
        }
    }

    /// Resolves a catalogue name to the record it identifies, scoped to the
    /// calling guest's tenant.
    ///
    /// Returns `None` when the name is unknown in the caller's tenant (a name
    /// registered under another tenant is invisible).
    pub async fn resolve_region(
        &mut self,
        name: &str,
    ) -> Result<Option<CatalogueRecord>, GuestError> {
        let client = self.catalogue().await?;
        match client
            .request(ControlRequest::LookupResource {
                uri: name.to_string(),
            })
            .await?
        {
            ControlResponse::Lookup { record } => Ok(record),
            other => Err(GuestError::Host(format!(
                "unexpected catalogue lookup reply: {other:?}"
            ))),
        }
    }

    /// Watches a registry route, yielding its lifecycle changes.
    ///
    /// The watch is seeded from the route's current state (an authoritative
    /// one-shot resolve) and then applies changes from the discovery
    /// route-change table, so a consumer converges on a provider that is not
    /// yet registered and re-attaches when a provider is replaced.
    ///
    /// The discovery route-change table itself must already be published: it is
    /// resolved with an immediate [`lookup`](Self::lookup) (a connected
    /// discovery service constructs it before accepting sessions), so a watch
    /// requested before then returns [`GuestError::Host`] rather than waiting.
    /// Convergence around the table would otherwise recurse into itself.
    ///
    /// ```ignore
    /// // Guest code: converge on the store, then re-attach if it restarts.
    /// let mut watch = ctx.watch("sel:///store").await?;
    /// while let Ok(event) = watch.next().await {
    ///     match event {
    ///         RouteEvent::Appeared(target) | RouteEvent::Replaced(target) => {
    ///             let sender = ResourceSender::attach(target.resource_id)?;
    ///             // ... use the store ...
    ///         }
    ///         RouteEvent::Revoked => { /* wait for the provider to return */ }
    ///     }
    /// }
    /// ```
    pub async fn watch(&mut self, uri: &str) -> Result<RouteWatch, GuestError> {
        // The route table is resolved with an immediate lookup: a connected
        // discovery service constructs the table before it accepts RPC
        // sessions, so readiness convergence here would only recurse.
        let table = self.lookup(ROUTE_TABLE_ROUTE).await?.ok_or_else(|| {
            GuestError::Host(format!(
                "route table is not published yet: {ROUTE_TABLE_ROUTE}"
            ))
        })?;
        let channel = self.route_table_channel(&table)?;
        let transport = ShmTransport::new_replay(&channel, &channel)
            .map_err(|error| GuestError::from(error).context("route table transport"))?;
        let subscriber = Subscriber::new(FramedRead::new(transport), None);
        let view = LiveTableView::new(subscriber)
            .map_err(|error| GuestError::from(error).context("route table view"))?;
        // Authoritative seed: a one-shot resolve is not subject to ring wrap.
        let seed = self.lookup(uri).await?;
        // Read the baseline version *after* the seed, so it can only lag the
        // seed — never lead it. A change published between the resolve and this
        // read is therefore reported once as a real replacement, while a change
        // published before both is already in the seed and produces no
        // duplicate `Appeared`+`Replaced` pair.
        view.sync()
            .map_err(|error| GuestError::from(error).context("route table sync"))?;
        let version = view
            .get_record(&uri.to_string())
            .map_err(|error| GuestError::from(error).context("route table read"))?
            .map(|record| record.version)
            .unwrap_or(0);
        Ok(RouteWatch::new(uri.to_string(), seed, version, view))
    }

    fn route_table_channel(&mut self, table: &ResourceTarget) -> Result<Channel, GuestError> {
        if let Some((resource_id, channel)) = &self.route_table
            && *resource_id == table.resource_id
        {
            return channel
                .try_clone()
                .map_err(|error| GuestError::from(error).context("route table handle clone"));
        }

        let channel = Channel::attach(table.resource_id)
            .map_err(|error| GuestError::from(error).context("route table attach"))?;
        let cached = channel
            .try_clone()
            .map_err(|error| GuestError::from(error).context("route table handle clone"))?;
        self.route_table = Some((table.resource_id, cached));
        Ok(channel)
    }

    /// Resolves a route once it is ready, converging rather than failing when
    /// the provider has not registered yet.
    ///
    /// This is the watch-driven form of [`lookup`](Self::lookup): it parks on
    /// the route-change table until the route appears (or is replaced), then
    /// re-resolves authoritatively, so a consumer started before its provider
    /// converges on the registration write itself. `bound` is a failure
    /// backstop: a provider that never appears yields this method's typed
    /// readiness error rather than waiting forever.
    ///
    /// Long-lived consumers that must also observe later replacements should
    /// use [`watch`](Self::watch) instead: delivery there is event-driven.
    pub async fn resolve_ready(
        &mut self,
        uri: &str,
        bound: Duration,
    ) -> Result<ResourceTarget, GuestError> {
        match self.await_route(uri, bound).await? {
            Some(target) => Ok(target),
            None => Err(GuestError::Host(format!(
                "route did not become ready within {bound:?}: {uri}"
            ))),
        }
    }

    /// Waits for `uri` to resolve, bounded by `bound`, using route watches.
    ///
    /// Returns the target on convergence, or `None` when the bound expires.
    /// Every appearance/replacement event triggers a fresh authoritative
    /// `lookup` because resolution records attach authority; watch events are
    /// only the convergence signal. Revocations and stale events (a route that
    /// disappeared between the event and the lookup) keep the wait going.
    async fn await_route(
        &mut self,
        uri: &str,
        bound: Duration,
    ) -> Result<Option<ResourceTarget>, GuestError> {
        let deadline = crate::Instant::now()?.checked_add(bound).ok_or_else(|| {
            GuestError::Host(format!("resolve bound overflows the clock for {uri}"))
        })?;

        loop {
            // An immediate answer needs no watch at all.
            if let Some(target) = self.lookup(uri).await? {
                return Ok(Some(target));
            }
            if crate::Instant::now()? >= deadline {
                return Ok(None);
            }

            let mut watch = self.watch(uri).await?;
            loop {
                let Some(event) = next_event_before(&mut watch, deadline).await? else {
                    return Ok(None);
                };
                if matches!(event, RouteEvent::Revoked) {
                    // Still absent: keep watching.
                    continue;
                }
                match self.lookup(uri).await? {
                    Some(target) => return Ok(Some(target)),
                    // A stale event: the route went away again. Start over
                    // from a fresh watch so the next appearance is observed.
                    None => break,
                }
            }
        }
    }

    /// Publishes this guest's health as observable state.
    ///
    /// Health is a tenant-scoped record peers and the supervisor can resolve
    /// and watch — a health *record*, not a readiness signal. The record is not
    /// lease-expired (that would report every healthy long-running guest as
    /// unhealthy) and is revoked when this guest's process exits; liveness
    /// itself is carried by the route leases this guest renews.
    pub async fn publish_health(&mut self, name: &str) -> Result<(), GuestError> {
        let response = self
            .discovery()
            .request(DiscoveryRequest::SetHealth {
                name: name.to_string(),
                healthy: true,
            })
            .await
            .map_err(|e| GuestError::from(e).context("discovery health publish"))?;
        match response {
            DiscoveryResponse::Registered => Ok(()),
            other => Err(GuestError::Host(format!(
                "unexpected discovery response: {other:?}"
            ))),
        }
    }

    /// Removes this guest's health record, marking it unhealthy.
    pub async fn unpublish_health(&mut self, name: &str) -> Result<(), GuestError> {
        let response = self
            .discovery()
            .request(DiscoveryRequest::SetHealth {
                name: name.to_string(),
                healthy: false,
            })
            .await
            .map_err(|e| GuestError::from(e).context("discovery health unpublish"))?;
        match response {
            DiscoveryResponse::Revoked => Ok(()),
            other => Err(GuestError::Host(format!(
                "unexpected discovery response: {other:?}"
            ))),
        }
    }

    /// Observes a health record in the caller's tenant.
    ///
    /// Returns whether the record is present. Absence is not an error: a record
    /// may never have been published, or may have been revoked or lapsed.
    pub async fn observe_health(&mut self, name: &str) -> Result<bool, GuestError> {
        let (_, tenant) = crate::self_info()?;
        Ok(self
            .lookup(&health_uri(tenant.as_deref(), name))
            .await?
            .is_some())
    }

    /// Resolves the protocol handler for `scheme` via discovery.
    ///
    /// The handler URI (`sel:///handler/<scheme>`) is published by the guest
    /// that holds the host-issued route claim, so the result cannot be forged
    /// by another guest. Serve-side guests use it to pin the process
    /// legitimately allowed to deliver handoffs. A handler that has not yet
    /// published resolves as `None`; consumers converge rather than treat that
    /// as permanent.
    pub async fn resolve_protocol_handler(
        &mut self,
        scheme: &str,
    ) -> Result<Option<selium_abi::ProcessId>, GuestError> {
        let uri = format!("{}/handler/{}", uri::SEL_PREFIX, scheme);
        // Bootstrap is a single pass with no readiness gate: a handler guest
        // publishes its route at the start of its own entrypoint, which may
        // still be in flight when a peer starts. Converge on the claim through
        // the route watch rather than treating an early miss as permanent; a
        // handler that never appears resolves as absent.
        Ok(self
            .await_route(&uri, HANDLER_RESOLVE_BOUND)
            .await?
            .map(|target| target.resource_id))
    }

    /// Returns the raw discovery host-queue handle this context was built
    /// from. Useful when a guest needs to forward the handle onward (e.g. to
    /// a spawned child via [`crate::Process::start`]) after receiving its
    /// [`Context`] through the entrypoint macro.
    pub fn raw_handle(&self) -> u64 {
        self.handle
    }

    /// Resolves a URI (typed, named-service route, leaf alias, or external
    /// wire name) to a resource via the discovery service.
    ///
    /// Convenience method that delegates to `self.discovery().request()`.
    ///
    /// Resolution is **not** an authorisation basis on its own. When the
    /// resolved resource's owner declared it attachable by resolution, the
    /// discovery service records the resolved id with the runtime on the
    /// caller's behalf, and the *host* enforces the owner's declaration scope;
    /// the returned target carries no permission by itself.
    pub async fn lookup(&mut self, uri: &str) -> Result<Option<ResourceTarget>, GuestError> {
        let request = DiscoveryRequest::Resolve(uri.to_string());

        let response = self
            .discovery()
            .request(request)
            .await
            .map_err(|e| GuestError::from(e).context("discovery request"))?;

        match response {
            DiscoveryResponse::Found(target) => Ok(Some(target)),
            DiscoveryResponse::NotFound => Ok(None),
            DiscoveryResponse::Registered
            | DiscoveryResponse::Revoked
            | DiscoveryResponse::Forbidden
            | DiscoveryResponse::Conflict
            | DiscoveryResponse::Resolved(_)
            | DiscoveryResponse::Domains(_) => Err(GuestError::Host(
                "unexpected discovery response variant".to_string(),
            )),
        }
    }

    /// Fetches the advisory domain→tenant table from the discovery service,
    /// for local wire-name resolution (see
    /// [`uri::resolve_wire_name`](selium_abi::uri::resolve_wire_name)).
    pub async fn load_domains(&mut self) -> Result<DomainTable, GuestError> {
        let response = self
            .discovery()
            .request(DiscoveryRequest::ListDomains)
            .await
            .map_err(|e| GuestError::from(e).context("discovery domain list"))?;

        match response {
            DiscoveryResponse::Domains(entries) => {
                let mut table = DomainTable::new();
                for entry in entries {
                    table.seed(entry.domain, entry.tenant);
                }
                Ok(table)
            }
            other => Err(GuestError::Host(format!(
                "unexpected discovery response: {other:?}"
            ))),
        }
    }

    /// Registers a URI→target mapping in the discovery service.
    ///
    /// Convenience method that delegates to `self.discovery().request()`.
    /// Returns `Err(GuestError::Host("registration forbidden"))` if the discovery
    /// service rejects the registration.
    pub async fn register(&mut self, uri: &str, target: ResourceTarget) -> Result<(), GuestError> {
        self.register_inner(uri, target, false).await
    }

    /// Registers a named-service route from the guest's own declaration,
    /// deriving both the internal path (`sel://<tenant>/<path…>`) and its wire
    /// name (`<reversed path>.<tenant>`, or under an owned domain) from the one
    /// [`Serve`] value. The guest creates its own resource; the runtime no
    /// longer provisions routes on a guest's behalf.
    ///
    /// Registration in the root/system tenant is permitted only for the exact
    /// URI a `RouteClaim` covers (enforced by discovery).
    pub async fn serve(&mut self, serve: Serve) -> Result<String, GuestError> {
        // A named service must project to a wire name: reject typed-resource
        // paths and non-DNS-safe segments up front.
        let joined = serve.path.join("/");
        if uri::labels_from_path(&joined).is_none() {
            return Err(GuestError::Host(format!(
                "serve path {joined:?} cannot project to a wire name"
            )));
        }
        let (_, tenant) = crate::self_info()?;
        let uri = format!(
            "{}{}/{}",
            uri::SEL_PREFIX,
            tenant.unwrap_or_default(),
            joined
        );
        // The route's registration key is the derived internal path; pin it
        // onto the target so discovery registers under that URI.
        let mut target = serve.target;
        target.uri = uri.clone();
        // Publish the canonical typed record first, so the named route is
        // admissible on the first attempt without waiting for any host feed
        // event. The canonical tenant comes from the host's mint-time record,
        // not from the caller's own tenant.
        //
        // A served resource is published for other guests to resolve and
        // attach, so the owner declares it attachable by resolution *before*
        // any route exists. The declaration admits the whole platform:
        // discovery's own resolve is already tenant-scoped, and root-namespace
        // system guests must be able to attach a served resource too (e.g. a
        // shared connector). Declaring first means a consumer that observes the
        // route's first appearance can attach immediately, and a failed
        // declaration publishes nothing.
        let attachable_class = matches!(
            target.class,
            selium_abi::ResourceClass::SharedRegion | selium_abi::ResourceClass::HostQueue
        )
        .then(|| target.class.clone());
        let resource_id = target.resource_id;
        if let Some(class) = attachable_class {
            crate::hostcall::declare_attachable_by_resolution(
                class,
                resource_id,
                selium_abi::AttachDeclarationScope::Any,
            )?;
        }
        self.publish_canonical(&target).await?;
        self.register_inner(&uri, target, serve.default).await?;
        Ok(uri)
    }

    async fn register_inner(
        &mut self,
        uri: &str,
        target: ResourceTarget,
        root_service: bool,
    ) -> Result<(), GuestError> {
        let request = DiscoveryRequest::Register {
            uri: uri.to_string(),
            target,
            // Guests register on their own behalf; the runtime supplies the
            // Tier-1 owner over the discovery feed.
            owner: None,
            root_service,
        };

        let response = self
            .discovery()
            .request(request)
            .await
            .map_err(|e| GuestError::from(e).context("discovery register"))?;

        match response {
            DiscoveryResponse::Registered => {
                self.spawn_lease_renewal(uri.to_string());
                Ok(())
            }
            DiscoveryResponse::Forbidden => Err(GuestError::Host(
                "registration forbidden: process does not own the target or lacks a route claim"
                    .to_string(),
            )),
            DiscoveryResponse::Conflict => Err(GuestError::Host(format!(
                "registration conflict: {uri} is already claimed by another live registration"
            ))),
            other => Err(GuestError::Host(format!(
                "unexpected discovery response: {other:?}"
            ))),
        }
    }

    /// Spawns a background task that renews the lease of a registration this
    /// guest owns, keeping its route (or health record) live.
    ///
    /// At most one renewal task runs per URI: `serve` and `publish_health` may
    /// both register the same route during a guest's boot.
    fn spawn_lease_renewal(&mut self, uri: String) {
        if !self.renewing.insert(uri.clone()) {
            return;
        }
        // Renew at half the registry's lease TTL, leaving a full half-window of
        // margin before expiry.
        let interval = Duration::from_millis(REGISTRATION_LEASE_TTL_MS / 2);
        let handle = self.handle;
        let sender = self.discovery_sender.clone();
        crate::spawn(async move {
            // Clone the queue descriptor already attached to this guest. A
            // fresh `from_raw` would attach the same host queue again and the
            // renewal task would exit before its first renewal.
            let mut ctx = match Context::from_attached_sender(handle, sender).await {
                Ok(ctx) => ctx,
                Err(_) => return,
            };
            loop {
                let deadline = match crate::Instant::now() {
                    Ok(now) => now + interval,
                    Err(_) => return,
                };
                if crate::Timer::new(deadline).await.is_err() {
                    return;
                }
                let response = ctx
                    .discovery()
                    .request(DiscoveryRequest::Renew { uri: uri.clone() })
                    .await;
                match response {
                    Ok(DiscoveryResponse::Registered) => {}
                    // The route is gone or no longer this guest's to renew, so
                    // stopping here is what lets a revoked record lapse.
                    Ok(_) => return,
                    // A transient transport failure does not surrender the
                    // lease: one half-window still remains before expiry, so
                    // keep trying at the next interval instead of letting a
                    // live registration lapse on a single hiccup.
                    Err(_) => {}
                }
            }
        });
    }

    /// Revokes a URI→target mapping in the discovery service.
    ///
    /// Convenience method that delegates to `self.discovery().request()`.
    pub async fn revoke(&mut self, uri: &str) -> Result<(), GuestError> {
        let request = DiscoveryRequest::Revoke {
            uri: uri.to_string(),
        };

        let response = self
            .discovery()
            .request(request)
            .await
            .map_err(|e| GuestError::from(e).context("discovery revoke"))?;

        match response {
            DiscoveryResponse::Revoked => Ok(()),
            other => Err(GuestError::Host(format!(
                "unexpected discovery response: {other:?}"
            ))),
        }
    }
}

/// The tenant-scoped health route for `name`.
fn health_uri(tenant: Option<&str>, name: &str) -> String {
    format!(
        "{}{}/health/{}",
        uri::SEL_PREFIX,
        tenant.unwrap_or_default(),
        name
    )
}

/// Awaits the watch's next event, bounded by `deadline`.
///
/// Returns `Ok(None)` when the deadline expires first. The watch is polled in
/// place, so cancelling on the timer leaves its position intact.
async fn next_event_before(
    watch: &mut RouteWatch,
    deadline: crate::Instant,
) -> Result<Option<RouteEvent>, GuestError> {
    let mut event = std::pin::pin!(watch.next());
    let mut timer = std::pin::pin!(crate::Timer::new(deadline));
    std::future::poll_fn(|cx| {
        if let std::task::Poll::Ready(result) = event.as_mut().poll(cx) {
            return std::task::Poll::Ready(result.map(Some));
        }
        match timer.as_mut().poll(cx) {
            std::task::Poll::Ready(Ok(())) => return std::task::Poll::Ready(Ok(None)),
            std::task::Poll::Ready(Err(error)) => return std::task::Poll::Ready(Err(error)),
            std::task::Poll::Pending => {}
        }
        std::task::Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_region_preserves_allocation_abi_errors() {
        let (client, server) =
            crate::client_session::test_session_pair::<DiscoveryRequest, DiscoveryResponse>().await;
        let mut context = Context {
            handle: 0,
            client,
            discovery_sender: ResourceSender::from_queue(selium_abi::HostQueueDescriptor {
                local_id: 0,
                shared_id: 0,
            }),
            store_client: None,
            control_client: None,
            route_table: None,
            renewing: std::collections::BTreeSet::new(),
        };
        crate::platform::install_native_hostcall_script(crate::platform::NativeHostcallScript {
            immediate_failure: Some(selium_abi::AbiError::new(
                selium_abi::AbiErrorCode::QuotaExceeded,
                "allocation quota detail",
            )),
            ..crate::platform::NativeHostcallScript::default()
        });
        let error = context
            .create_region("sel://acme/region/example", 1, ResourceKind::SharedMemory)
            .await
            .expect_err("the host rejects allocation");
        assert_eq!(
            error.abi_code(),
            Some(selium_abi::AbiErrorCode::QuotaExceeded)
        );
        assert!(matches!(
            &error,
            GuestError::Hostcall { message, .. } if message == "allocation quota detail"
        ));
        crate::platform::clear_native_hostcall_script();
        server.abort();
    }

    #[tokio::test]
    async fn from_raw_with_invalid_handle_fails() {
        let result = Context::from_raw(0).await;
        assert!(result.is_err());
        // In native mode, ResourceSender::attach(0) fails because there's
        // no host queue infrastructure.
    }

    #[tokio::test]
    async fn route_table_channel_attaches_once_per_resolved_region() {
        if selium_memory::region_provider().is_err() {
            drop(selium_memory::set_region_provider(Box::new(
                selium_memory::HeapRegionProvider::new(),
            )));
        }
        let first = Channel::create_with_backpressure(
            4096,
            selium_shm::ChannelBackpressure::Drop,
            ResourceKind::LiveTable,
        )
        .expect("first route table");
        let second = Channel::create_with_backpressure(
            4096,
            selium_shm::ChannelBackpressure::Drop,
            ResourceKind::LiveTable,
        )
        .expect("second route table");

        let (client, server) =
            crate::client_session::test_session_pair::<DiscoveryRequest, DiscoveryResponse>().await;
        let mut context = Context {
            handle: 0,
            client,
            discovery_sender: ResourceSender::from_queue(selium_abi::HostQueueDescriptor {
                local_id: 0,
                shared_id: 0,
            }),
            store_client: None,
            control_client: None,
            route_table: None,
            renewing: std::collections::BTreeSet::new(),
        };
        let target = |resource_id| ResourceTarget {
            uri: ROUTE_TABLE_ROUTE.to_string(),
            host_id: String::new(),
            resource_id,
            interface: None,
            tenant: None,
            class: ResourceClass::SharedRegion,
            labels: Vec::new(),
        };

        let initial = context
            .route_table_channel(&target(first.region_id()))
            .expect("initial attach");
        assert_eq!(initial.region_id(), first.region_id());
        let cloned = context
            .route_table_channel(&target(first.region_id()))
            .expect("same-region clone");
        assert_eq!(cloned.region_id(), first.region_id());

        let replacement = context
            .route_table_channel(&target(second.region_id()))
            .expect("replacement attach");
        assert_eq!(replacement.region_id(), second.region_id());
        server.abort();
    }

    // scenario: addressing/named-service-projection-boundary/resource-id-uri-stays-internal
    #[test]
    fn serve_rejects_non_projectable_paths() {
        assert!(uri::labels_from_path("region/7").is_none());
        assert!(uri::labels_from_path("Foo").is_none());
        assert_eq!(
            uri::labels_from_path("http/prod"),
            Some(vec!["prod".to_string(), "http".to_string()])
        );
    }
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `selium-guest` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `guest-context` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.
