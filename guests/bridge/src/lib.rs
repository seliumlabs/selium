//! Single-instance (per-platform) bridge server system guest.
//!
//! The QUIC connector terminates QUIC and delivers each accepted stream to the
//! single bridge server as a per-stream handoff whose [`IncomingConnection`]
//! metadata carries the authenticated client identity (`tenant` +
//! fingerprint). The bridge server:
//!
//! - creates its own listener and registers its root serving route
//!   (`sel:///bridge`, wire name `bridge`) with discovery via `Context::serve`,
//!   deriving the route from that one declaration;
//! - derives each handoff's tenant from the decoded identity — the single
//!   bridge-server is bound to no one tenant, so it never refuses an identity
//!   on cross-tenant grounds;
//! - resolves the handoff's identity to a grant set from the identity guest's
//!   published grant table (a `fingerprint -> baseline grants` live table read
//!   via [`GrantTable`]), replacing the interim `IdentityGrantMap` stub;
//! - spawns one `bridge-channel <shared_id, grants>` per stream, conferring the
//!   client's grants (narrowed for that identity's tenant) plus an
//!   `ExplicitResource` for the handed-off region via the `DelegateGrants`
//!   capability held at `Namespace::Root`;
//! - refuses unknown identities by attaching then closing the delivered region
//!   so the connector observes EOF and FINs the client stream;
//! - delegates the spawn bound to the host-enforced per-tenant process quota:
//!   a `Process::start_for_tenant` failure (e.g. the tenant's quota is
//!   exhausted) is surfaced by attach-then-close, with no guest-local spawn
//!   counter.
//!
//! The bridge server terminates no QUIC and relays no stream bytes; it is a
//! control-plane guest only.

use std::hash::Hash;

use anyhow::Context as _;
use selium_abi::{
    Capability, CapabilityGrant, ResourceClass, ResourceIdentity, ResourceSelector,
    client_identity::ClientIdentity,
};
use selium_guest::{
    Context, ModuleSource, Process, ResourceListener, RouteEvent, Serve, entrypoint, error, info,
    net::ByteStream, warn,
};
use selium_service::{FlatMsg, ResourceTarget};
use selium_shm::{Channel, transport::ShmTransport};
use selium_wire::{LiveTableView, framed::FramedRead, pubsub::Subscriber};

const BRIDGE_CHANNEL_ENTRYPOINT: &str = "bridge_channel";
/// The identity guest's published grant table route.
const GRANT_TABLE_ROUTE: &str = "sel:///identity-grants";
/// The accountant guest's published narrowing table route.
const NARROWING_TABLE_ROUTE: &str = "sel:///accounting-narrowing";

/// The identity-published grant table: a `fingerprint -> baseline grants` live
/// table read at handoff conferral. Replaces the interim `IdentityGrantMap`
/// stub. Until the table is attached (`Some`), every fingerprint is unknown and
/// the bridge refuses the handoff — fail-closed, never conferring on absence.
pub struct GrantTable {
    table: Option<LiveTableView<Vec<u8>, Vec<u8>, ShmTransport>>,
    resource_id: Option<u64>,
}

/// The accountant-published narrowing table: `tenant -> rkyv-encoded
/// Vec<Capability>` of capabilities the bridge subtracts from the
/// identity-published baseline grants at conferral. Until the table is
/// attached (`Some`), every tenant is un-narrowed (no-op), mirroring the
/// grant table's fail-closed absence for unknown identities.
pub struct NarrowingTable {
    table: Option<LiveTableView<String, Vec<u8>, ShmTransport>>,
    resource_id: Option<u64>,
}

/// What woke the bridge server's main loop.
enum Wake {
    /// A change on the identity grant table's route.
    GrantTable(anyhow::Result<RouteEvent>),
    /// A change on the accountant narrowing table's route.
    NarrowingTable(anyhow::Result<RouteEvent>),
    /// A delivered stream handoff (or a receive failure).
    Handoff(Result<selium_guest::IncomingConnection, selium_guest::GuestError>),
}

impl GrantTable {
    /// Builds an empty (unattached) grant table: every lookup misses.
    pub fn empty() -> Self {
        Self {
            table: None,
            resource_id: None,
        }
    }

    /// Attaches the identity guest's grant table from a resolved target.
    ///
    /// Resolution (not the watch event itself) is what establishes attach
    /// authority, so callers pass the target from an authoritative `lookup`.
    pub fn from_target(target: &ResourceTarget) -> anyhow::Result<Self> {
        Ok(Self {
            table: Some(open_table_view(target)?),
            resource_id: Some(target.resource_id),
        })
    }

    /// Drains any pending mutations into the local view. Best-effort.
    pub fn sync(&mut self) {
        if let Some(table) = &self.table
            && let Err(e) = table.sync()
        {
            warn!("bridge-server: grant table sync failed: {e}");
        }
    }

    /// Returns the baseline grants for a fingerprint, if known.
    pub fn grants_for(&self, fingerprint: &[u8; 32]) -> Option<Vec<CapabilityGrant>> {
        let table = self.table.as_ref()?;
        let bytes = table.get(&fingerprint.to_vec()).ok().flatten()?;
        selium_abi::decode_rkyv::<Vec<CapabilityGrant>>(&bytes).ok()
    }
}

impl NarrowingTable {
    /// Builds an empty (unattached) narrowing table: every tenant un-narrowed.
    pub fn empty() -> Self {
        Self {
            table: None,
            resource_id: None,
        }
    }

    /// Attaches the accountant's narrowing table from a resolved target.
    pub fn from_target(target: &ResourceTarget) -> anyhow::Result<Self> {
        Ok(Self {
            table: Some(open_table_view(target)?),
            resource_id: Some(target.resource_id),
        })
    }

    /// Drains any pending mutations into the local view. Best-effort.
    pub fn sync(&mut self) {
        if let Some(table) = &self.table
            && let Err(e) = table.sync()
        {
            warn!("bridge-server: narrowing table sync failed: {e}");
        }
    }

    /// Returns the capabilities to subtract from a tenant's baseline grants.
    pub fn narrowing_for(&self, tenant: &str) -> Vec<Capability> {
        let Some(table) = self.table.as_ref() else {
            return Vec::new();
        };
        let bytes = table
            .get(&tenant.to_string())
            .ok()
            .flatten()
            .unwrap_or_default();
        selium_abi::decode_rkyv::<Vec<Capability>>(&bytes).unwrap_or_default()
    }
}

// A table view attached to a discovery-resolved shared region.
trait AttachedTable {
    fn resource_id(&self) -> Option<u64>;
}

impl AttachedTable for GrantTable {
    fn resource_id(&self) -> Option<u64> {
        self.resource_id
    }
}

impl AttachedTable for NarrowingTable {
    fn resource_id(&self) -> Option<u64> {
        self.resource_id
    }
}

/// Folds the accountant's published narrowing into the baseline grants:
/// every baseline grant whose capability appears in the narrowing set is
/// removed. An empty narrowing set is a no-op.
pub fn narrow_grants(
    grants: Vec<CapabilityGrant>,
    narrowing: &[Capability],
) -> Vec<CapabilityGrant> {
    grants
        .into_iter()
        .filter(|grant| !narrowing.contains(&grant.capability))
        .collect()
}

/// Encodes a `u64` entrypoint argument in the `WasmValue::I64` wire form.
fn arg_u64(value: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(9);
    bytes.push(1);
    bytes.extend_from_slice(&value.to_le_bytes());
    bytes
}

/// Attaches the delivered stream region then closes it, so the connector
/// observes the close as EOF and FINs the client's stream.
fn attach_then_close(shared_id: u64) {
    match ByteStream::attach_blocking(shared_id) {
        Ok(stream) => drop(stream),
        Err(e) => warn!(shared_id, "bridge-server: attach-then-close failed: {e}"),
    }
}

/// Builds the grant set conferred on a spawned bridge-channel: the client's
/// resolved grants plus two tenant-scoped `ExplicitResource` grants —
///
/// - **SharedMemory** for the handed-off stream region, so the child can
///   attach the relayed byte channel (a region the server itself merely
///   handed off);
/// - **HostQueue** for the discovery listener queue, so the child can build
///   its own discovery client from the forwarded handle:
///   `Context::from_raw` attaches the listener queue, exactly as
///   bootstrap-spawned guests do via their injected discovery grant.
///
/// Both explicit grants carry the tenant selector because delegation only
/// admits child grants that are tenant-scoped within the `DelegateGrants`
/// fence (unscoped grants fall through to the subset check, which the
/// server cannot satisfy for resources it does not own).
fn bridge_channel_grants(
    client_grants: Vec<CapabilityGrant>,
    tenant: &str,
    discovery_listener: u64,
    stream_region: u64,
) -> Vec<CapabilityGrant> {
    let mut child_grants = client_grants;
    child_grants.push(CapabilityGrant::new(
        Capability::SharedMemory,
        vec![
            ResourceSelector::Tenant(tenant.to_string()),
            ResourceSelector::ExplicitResource(ResourceIdentity::Shared(stream_region)),
        ],
    ));
    child_grants.push(CapabilityGrant::new(
        Capability::HostQueue,
        vec![
            ResourceSelector::Tenant(tenant.to_string()),
            ResourceSelector::ExplicitResource(ResourceIdentity::Shared(discovery_listener)),
        ],
    ));
    child_grants
}

/// Bridge server entrypoint.
///
/// The server receives its bootstrap discovery `Context` (built by the
/// entrypoint macro) and hands the underlying discovery handle to spawned
/// bridge-channels via [`Context::raw_handle`]. The server creates its own
/// listener and self-registers its serving route via [`Context::serve`]; the
/// runtime no longer provisions the route or injects a listener argument.
#[entrypoint]
async fn bridge_server(mut ctx: Context, bridge_channel_module: (u64, u64)) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("bridge-server: started");

    // The bridge-channel module bytes are supplied by the embedder as a
    // bootstrap pointer argument — the runtime keeps no module catalogue, so
    // the spawner supplies the source for every spawn.
    // SAFETY: the runtime wrote the bridge-channel module bytes into this
    // guest's linear memory for this entrypoint invocation.
    let bridge_channel_module =
        unsafe { selium_guest::args::bytes(bridge_channel_module.0, bridge_channel_module.1) }
            .to_vec();

    // The server creates its own listener: self-registration replaces the
    // runtime's well-known-URI queue minting.
    let mut listener =
        ResourceListener::create().with_context(|| "bridge-server: create listener failed")?;

    // Pin the QUIC connector: handoff metadata is sender-controlled, so an
    // unpinned listener would let any guest that resolves and attaches the
    // bridge route forge an authenticated identity and mint grants. Handoffs
    // from any process other than the registered `sel-quic` handler are
    // refused by the listener.
    let connector = ctx
        .resolve_protocol_handler("sel-quic")
        .await
        .with_context(|| "bridge-server: connector resolve failed")?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "bridge-server: no sel-quic protocol handler registered; refusing to serve unpinned handoffs"
            )
        })?;
    listener.expect_sender(connector);

    // Register the root serving route (`sel:///bridge`) from one declaration.
    // The bridge-server is a single per-platform instance (tenant `None`): its
    // own tenant is not part of the serving identity, and the tenant it acts
    // for comes from each handoff's authenticated identity.
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
        path: vec!["bridge".to_string()],
        target,
        default: false,
    })
    .await
    .with_context(|| "bridge-server: serve failed")?;
    info!("bridge-server: route registered");

    // Grant and narrowing tables converge reactively. A table may be absent at
    // boot or replaced when its provider restarts, so each is driven by a route
    // watch instead of a bounded one-shot attachment. Until a table attaches,
    // its safe default stays in force: grants fail closed (unknown identities
    // are refused) and narrowing fails open (no subtraction).
    let mut grant_watch =
        open_table_watch(&mut ctx, GRANT_TABLE_ROUTE, "identity grant table").await;
    let mut narrowing_watch =
        open_table_watch(&mut ctx, NARROWING_TABLE_ROUTE, "narrowing table").await;
    let mut grant_table = GrantTable::empty();
    let mut narrowing_table = NarrowingTable::empty();

    // Health is published state: advertise the bridge server's health.
    if let Err(error) = ctx.publish_health("bridge-server").await {
        warn!("bridge-server: health publish failed: {error}");
    }

    // One receive future lives across loop iterations: cancelling a pending
    // host-queue receive could drop a handoff that the host already popped, so
    // table events must not tear it down. It is replaced only after it
    // completes.
    let mut handoff: std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<selium_guest::IncomingConnection, selium_guest::GuestError>,
                > + Send
                + '_,
        >,
    > = Box::pin(listener.recv());

    loop {
        let wake = std::future::poll_fn(|cx| {
            // Table events are polled first so a busy handoff queue cannot
            // starve reconvergence. The handoff future is retained across
            // iterations (never recreated until it completes), so deferring it
            // by one poll cannot drop a delivered stream.
            {
                // The watch futures borrow their watches only for this poll;
                // their progress state lives in the watch itself.
                let mut event = std::pin::pin!(next_table_event(&mut grant_watch));
                if let std::task::Poll::Ready(event) = event.as_mut().poll(cx) {
                    return std::task::Poll::Ready(Wake::GrantTable(event));
                }
            }
            {
                let mut event = std::pin::pin!(next_table_event(&mut narrowing_watch));
                if let std::task::Poll::Ready(event) = event.as_mut().poll(cx) {
                    return std::task::Poll::Ready(Wake::NarrowingTable(event));
                }
            }
            if let std::task::Poll::Ready(result) = handoff.as_mut().poll(cx) {
                return std::task::Poll::Ready(Wake::Handoff(result));
            }
            std::task::Poll::Pending
        })
        .await;

        let incoming = match wake {
            Wake::GrantTable(event) => {
                grant_table =
                    converge_grant_table(event, grant_table, &mut grant_watch, &mut ctx).await;
                continue;
            }
            Wake::NarrowingTable(event) => {
                narrowing_table = converge_narrowing_table(
                    event,
                    narrowing_table,
                    &mut narrowing_watch,
                    &mut ctx,
                )
                .await;
                continue;
            }
            Wake::Handoff(Ok(incoming)) => {
                // Restart the receive for the next iteration.
                handoff = Box::pin(listener.recv());
                // Retry any table watch whose creation failed earlier (for
                // example while the discovery route table was still
                // unpublished). Without this, initial absence would leave the
                // table detached for the process's whole life, so a table that
                // appears later could never affect a handoff. The watch seeded
                // by a successful retry converges on the next loop iteration.
                if grant_watch.is_none() {
                    grant_watch =
                        open_table_watch(&mut ctx, GRANT_TABLE_ROUTE, "identity grant table").await;
                }
                if narrowing_watch.is_none() {
                    narrowing_watch =
                        open_table_watch(&mut ctx, NARROWING_TABLE_ROUTE, "narrowing table").await;
                }
                incoming
            }
            Wake::Handoff(Err(e)) => {
                error!("bridge-server: handoff receive failed: {e}");
                handoff = Box::pin(listener.recv());
                continue;
            }
        };

        // Synchronise both views immediately after the handoff, before
        // evaluating identity and narrowing: the next handoff must see every
        // change published while the bridge was waiting.
        grant_table.sync();
        narrowing_table.sync();

        // Refuse a handoff with no usable client identity: attach-then-close
        // so the connector observes EOF and FINs the client stream.
        let Some(identity) = ClientIdentity::decode(&incoming.metadata) else {
            warn!("bridge-server: refusing handoff with unparseable identity");
            attach_then_close(incoming.shared_id);
            continue;
        };

        let Some(grants) = grant_table.grants_for(&identity.fingerprint) else {
            warn!(
                tenant = %identity.tenant,
                "bridge-server: refusing unknown client identity"
            );
            attach_then_close(incoming.shared_id);
            continue;
        };

        // Fold the accountant's published narrowing into the baseline grants
        // before conferral: a delinquent tenant's narrowing set empties the
        // baseline; an absent narrowing set is a no-op. The tenant is taken
        // from the identity — the single bridge-server is bound to no one
        // tenant, so each handoff narrows for its own tenant.
        let grants = narrow_grants(grants, &narrowing_table.narrowing_for(&identity.tenant));

        let child_grants = bridge_channel_grants(
            grants,
            &identity.tenant,
            ctx.raw_handle(),
            incoming.shared_id,
        );

        match Process::start_for_tenant(
            ModuleSource::Inline(bridge_channel_module.clone()),
            BRIDGE_CHANNEL_ENTRYPOINT,
            vec![arg_u64(ctx.raw_handle()), arg_u64(incoming.shared_id)],
            child_grants,
            Some(&identity.tenant),
        ) {
            Ok(_child) => info!(
                tenant = %identity.tenant,
                shared_id = incoming.shared_id,
                "bridge-server: spawned bridge-channel"
            ),
            Err(e) => {
                // The host-enforced per-tenant process quota denied the spawn
                // (or the spawn failed for another reason): the client observes
                // the refusal as a stream close, with no guest-local counter.
                error!("bridge-server: bridge-channel spawn failed: {e}");
                attach_then_close(incoming.shared_id);
            }
        }
    }
}

/// Applies one grant-table watch event, restoring the fail-closed default on
/// revocation or any attachment failure.
async fn converge_grant_table(
    event: anyhow::Result<RouteEvent>,
    current: GrantTable,
    watch: &mut Option<selium_guest::RouteWatch>,
    ctx: &mut Context,
) -> GrantTable {
    converge_table(
        event,
        current,
        watch,
        "bridge-server: grant table",
        || ctx.lookup(GRANT_TABLE_ROUTE),
        GrantTable::from_target,
        GrantTable::empty,
    )
    .await
}

/// Applies one narrowing-table watch event, restoring the fail-open default on
/// revocation or any attachment failure.
async fn converge_narrowing_table(
    event: anyhow::Result<RouteEvent>,
    current: NarrowingTable,
    watch: &mut Option<selium_guest::RouteWatch>,
    ctx: &mut Context,
) -> NarrowingTable {
    converge_table(
        event,
        current,
        watch,
        "bridge-server: narrowing table",
        || ctx.lookup(NARROWING_TABLE_ROUTE),
        NarrowingTable::from_target,
        NarrowingTable::empty,
    )
    .await
}

/// Applies one watch event to a table, restoring that table's safe default on
/// revocation, a stale appearance, or any attachment failure.
///
/// An appearance is only a convergence signal: `resolve` re-resolves the route
/// through discovery (which records attach authority). If it still names the
/// attached region, `current` remains live and no duplicate attach is
/// attempted; a different region is attached as the replacement. `default` is
/// the caller's policy for *not* having a table: grants stay fail-closed
/// (unknown identities are refused) and narrowing stays fail-open (no
/// subtraction).
async fn converge_table<T, Resolve, Fut, Attach>(
    event: anyhow::Result<RouteEvent>,
    current: T,
    watch: &mut Option<selium_guest::RouteWatch>,
    label: &str,
    resolve: Resolve,
    attach: Attach,
    default: fn() -> T,
) -> T
where
    T: AttachedTable,
    Resolve: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Option<ResourceTarget>, selium_guest::GuestError>>,
    Attach: FnOnce(&ResourceTarget) -> anyhow::Result<T>,
{
    match event {
        Ok(RouteEvent::Appeared(_)) | Ok(RouteEvent::Replaced(_)) => {
            match resolve().await {
                Ok(Some(target)) if current.resource_id() == Some(target.resource_id) => {
                    return current;
                }
                Ok(Some(target)) => match attach(&target) {
                    Ok(table) => return table,
                    Err(error) => warn!("{label}: attach failed: {error}"),
                },
                Ok(None) => {} // Stale event: the route went away again.
                Err(error) => warn!("{label}: re-resolve failed: {error}"),
            }
            default()
        }
        Ok(RouteEvent::Revoked) => default(),
        Err(error) => {
            warn!("{label}: watch failed: {error}; using the safe default");
            *watch = None;
            default()
        }
    }
}

/// Awaits the next event from an optional route watch.
///
/// A watch that could not be created (e.g. the route table was unavailable at
/// boot) never resolves, leaving the table's safe default in force.
async fn next_table_event(
    watch: &mut Option<selium_guest::RouteWatch>,
) -> anyhow::Result<RouteEvent> {
    match watch {
        Some(watch) => watch.next().await.map_err(anyhow::Error::from),
        None => std::future::pending::<anyhow::Result<RouteEvent>>().await,
    }
}

/// Opens a replayed live-table view over a resolved table region.
///
/// Replay starts at the ring begin so mutations published before this
/// attachment are materialised, which is what lets a late-attached table
/// describe the current state rather than only later changes.
fn open_table_view<K, V>(
    target: &ResourceTarget,
) -> anyhow::Result<LiveTableView<K, V, ShmTransport>>
where
    K: FlatMsg + Clone + Hash + Eq,
    V: FlatMsg + Clone,
{
    let channel = Channel::attach(target.resource_id)
        .map_err(|e| anyhow::anyhow!("table region attach failed: {e}"))?;
    let transport = ShmTransport::new_replay(&channel, &channel)
        .map_err(|e| anyhow::anyhow!("table transport failed: {e}"))?;
    let subscriber = Subscriber::new(FramedRead::new(transport), None);
    LiveTableView::new(subscriber)
        .map_err(|e| anyhow::anyhow!("table view construction failed: {e}"))
}

/// Creates a table's route watch, reporting (not failing) when the route table
/// is not ready.
///
/// Called at boot and again before a handoff, so a watch whose creation failed
/// while the discovery route table was still unpublished is retried rather
/// than leaving the table permanently detached.
async fn open_table_watch(
    ctx: &mut Context,
    uri: &str,
    label: &str,
) -> Option<selium_guest::RouteWatch> {
    match ctx.watch(uri).await {
        Ok(watch) => Some(watch),
        Err(error) => {
            warn!("bridge-server: {label} unavailable: {error}");
            None
        }
    }
}

/// The stub client's data-plane grants: tenant-scoped shared memory, host
/// queues, and network streams. A real identity source provisions these from
/// policy rather than a fixed table.
#[cfg(test)]
fn tenant_acme_client_grants() -> Vec<CapabilityGrant> {
    vec![
        CapabilityGrant::new(
            Capability::SharedMemory,
            vec![
                ResourceSelector::Tenant("acme".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::SharedRegion),
            ],
        ),
        CapabilityGrant::new(
            Capability::HostQueue,
            vec![
                ResourceSelector::Tenant("acme".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::HostQueue),
            ],
        ),
        CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::Tenant("acme".to_string()),
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants() -> Vec<CapabilityGrant> {
        tenant_acme_client_grants()
    }

    /// The grant table decodes a published grant set from its rkyv-encoded
    /// value; an empty table reports every fingerprint as unknown (the bridge
    /// refuses the handoff rather than conferring on absence).
    // scenario: guest-bridge/identity-to-grant-resolution/unknown-identity-refused
    #[test]
    fn empty_grant_table_misses_every_fingerprint() {
        let table = GrantTable::empty();
        assert!(table.grants_for(&[0u8; 32]).is_none());
        assert!(table.grants_for(&[7u8; 32]).is_none());
    }

    /// The grant-table encoding round-trips through the same codec the bridge
    /// uses to decode the identity guest's published grant set.
    #[test]
    fn grant_encoding_round_trips_through_rkyv() {
        let grants = grants();
        let encoded = selium_abi::encode_rkyv(&grants).expect("encode");
        let decoded: Vec<CapabilityGrant> = selium_abi::decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, grants);
    }

    /// A spawned bridge-channel receives the client's grants plus the two
    /// tenant-scoped `ExplicitResource` grants it needs: the handed-off
    /// stream region and the discovery listener queue (the child builds its
    /// own discovery client via `Context::from_raw`).
    #[test]
    fn child_grants_cover_stream_region_and_discovery_listener() {
        const DISCOVERY_LISTENER: u64 = 100;
        const STREAM_REGION: u64 = 200;

        let child = bridge_channel_grants(grants(), "acme", DISCOVERY_LISTENER, STREAM_REGION);

        // The client's grants are conferred unchanged.
        assert_eq!(child.len(), grants().len() + 2);

        // Every conferred grant is tenant-scoped (the DelegateGrants fence
        // admits only tenant-scoped child grants).
        assert!(child.iter().all(|grant| {
            grant
                .selectors
                .iter()
                .any(|selector| matches!(selector, ResourceSelector::Tenant(t) if t == "acme"))
        }));

        let explicit = |capability: Capability, id: u64| {
            child.iter().any(|grant| {
                grant.capability == capability
                    && grant.selectors.iter().any(|selector| {
                        *selector
                            == ResourceSelector::ExplicitResource(ResourceIdentity::Shared(id))
                    })
            })
        };
        assert!(
            explicit(Capability::SharedMemory, STREAM_REGION),
            "child may attach the handed-off stream region: {child:?}"
        );
        assert!(
            explicit(Capability::HostQueue, DISCOVERY_LISTENER),
            "child may attach the discovery listener queue: {child:?}"
        );
    }

    /// Two handoffs carrying different tenants confer child grants scoped to
    /// their own tenant: the single bridge-server binds to no tenant, so the
    /// handed-off identity's tenant — not the server's — drives the conferred
    /// grant scopes.
    #[test]
    fn child_grants_scoped_to_each_handoff_tenant() {
        const DISCOVERY_LISTENER: u64 = 100;
        const STREAM_REGION: u64 = 200;

        let acme = bridge_channel_grants(grants(), "acme", DISCOVERY_LISTENER, STREAM_REGION);
        let beta = bridge_channel_grants(grants(), "beta", DISCOVERY_LISTENER, STREAM_REGION);

        let explicit_scoped = |grants: &[CapabilityGrant], tenant: &str, capability, id| {
            grants.iter().any(|grant| {
                grant.capability == capability
                    && grant.selectors.iter().any(|selector| {
                        matches!(
                            selector,
                            ResourceSelector::Tenant(t) if t == tenant
                        )
                    })
                    && grant.selectors.iter().any(|selector| {
                        *selector
                            == ResourceSelector::ExplicitResource(ResourceIdentity::Shared(id))
                    })
            })
        };

        assert!(explicit_scoped(
            &acme,
            "acme",
            Capability::SharedMemory,
            STREAM_REGION
        ));
        assert!(explicit_scoped(
            &acme,
            "acme",
            Capability::HostQueue,
            DISCOVERY_LISTENER
        ));
        assert!(explicit_scoped(
            &beta,
            "beta",
            Capability::SharedMemory,
            STREAM_REGION
        ));
        assert!(explicit_scoped(
            &beta,
            "beta",
            Capability::HostQueue,
            DISCOVERY_LISTENER
        ));
        assert!(
            !explicit_scoped(&beta, "acme", Capability::SharedMemory, STREAM_REGION),
            "a beta handoff must not confer an acme-scoped grant"
        );
    }

    /// The narrowing fold removes every baseline grant whose capability is in
    /// the accountant's published narrowing set.
    // scenario: guest-bridge/accountant-narrowing-at-conferral/baseline-narrowed-before-conferral
    // scenario: selium-accountant/narrowing-publication/narrowed-conferral
    #[test]
    fn narrow_grants_subtracts_published_capabilities() {
        let baseline = grants();
        let narrowed = narrow_grants(baseline.clone(), &[Capability::Network]);
        assert_eq!(narrowed.len(), baseline.len() - 1);
        assert!(
            narrowed
                .iter()
                .all(|grant| grant.capability != Capability::Network)
        );
    }

    /// An empty narrowing set is a no-op: the baseline grants pass through.
    // scenario: guest-bridge/accountant-narrowing-at-conferral/no-narrowing-is-a-no-op
    #[test]
    fn narrow_grants_with_empty_narrowing_is_a_noop() {
        let baseline = grants();
        assert_eq!(narrow_grants(baseline.clone(), &[]), baseline);
    }

    /// An unattached narrowing table narrows nothing: every tenant is a no-op.
    // scenario: guest-bridge/accountant-narrowing-at-conferral/narrowing-table-unreachable-fails-open
    #[test]
    fn empty_narrowing_table_narrows_nothing() {
        let table = NarrowingTable::empty();
        assert!(table.narrowing_for("acme").is_empty());
        assert!(table.narrowing_for("beta").is_empty());
    }

    /// Publishes a grant table on a fresh live-table region, returning the
    /// table handle, the region id a route would point at, and the channel
    /// that keeps the region alive.
    fn publish_grant_table(
        entries: &[([u8; 32], Vec<CapabilityGrant>)],
    ) -> (
        selium_wire::LiveTable<Vec<u8>, Vec<u8>, ShmTransport>,
        u64,
        Channel,
    ) {
        drop(selium_memory::set_region_provider(Box::new(
            selium_memory::HeapRegionProvider::new(),
        )));
        let channel = Channel::create_with_backpressure(
            64 * 1024,
            selium_shm::ChannelBackpressure::Drop,
            selium_abi::ResourceKind::LiveTable,
        )
        .expect("grant table channel");
        let publisher = selium_wire::Publisher::new(selium_wire::FramedWrite::new(
            ShmTransport::new(&channel, &channel).expect("publisher transport"),
        ));
        let subscriber = Subscriber::new(
            selium_wire::FramedRead::new(
                ShmTransport::new(&channel, &channel).expect("subscriber transport"),
            ),
            None,
        );
        let table: selium_wire::LiveTable<Vec<u8>, Vec<u8>, ShmTransport> =
            selium_wire::LiveTable::new(
                selium_wire::TableWriterAuthority::claim(format!(
                    "bridge.test.{}",
                    channel.region_id()
                ))
                .expect("authority"),
                publisher,
                subscriber,
            )
            .expect("live table");
        for (fingerprint, grants) in entries {
            let encoded = selium_abi::encode_rkyv(grants).expect("encode grants");
            table
                .set(fingerprint.to_vec(), encoded)
                .expect("publish grants");
        }
        let region_id = channel.region_id();
        (table, region_id, channel)
    }

    /// A resolved target pointing at a live-table region.
    fn table_target(region_id: u64) -> ResourceTarget {
        ResourceTarget {
            uri: GRANT_TABLE_ROUTE.to_string(),
            host_id: String::new(),
            resource_id: region_id,
            interface: None,
            tenant: None,
            class: ResourceClass::SharedRegion,
            labels: Vec::new(),
        }
    }

    /// A grant table attaches from the resolved target and serves the
    /// published grants (the late-publication path: the table can be created
    /// long after boot and attached when its route appears).
    #[test]
    fn grant_table_attaches_from_a_resolved_target() {
        let fingerprint = [7u8; 32];
        let (table, region_id, channel) = publish_grant_table(&[(fingerprint, grants())]);

        let attached = GrantTable::from_target(&table_target(region_id)).expect("attach");
        assert_eq!(attached.grants_for(&fingerprint), Some(grants()));
        assert!(
            attached.grants_for(&[8u8; 32]).is_none(),
            "unknown fingerprints stay unknown"
        );

        drop(table);
        drop(channel);
    }

    /// A replacement provider publishes a different region: re-attaching from
    /// the replacement's resolved target replaces the view instead of
    /// retaining the stale table.
    // scenario: guest-bridge/bridge-tables-reconverge/table-provider-is-replaced-or-revoked
    #[test]
    fn grant_table_replacement_switches_the_attached_region() {
        let fingerprint = [3u8; 32];
        let (first, first_region, first_channel) = publish_grant_table(&[(fingerprint, grants())]);
        let (second, second_region, second_channel) = publish_grant_table(&[]);

        let before = GrantTable::from_target(&table_target(first_region)).expect("attach first");
        assert!(before.grants_for(&fingerprint).is_some());

        let after = GrantTable::from_target(&table_target(second_region)).expect("attach second");
        assert!(
            after.grants_for(&fingerprint).is_none(),
            "the replacement's (empty) table must replace the stale view"
        );

        drop(first);
        drop(second);
        drop(first_channel);
        drop(second_channel);
    }

    /// A grant change published while the bridge waits for a handoff is
    /// visible to the next evaluation after synchronising the view.
    // scenario: guest-bridge/bridge-tables-reconverge/grant-changes-while-a-handoff-arrives
    #[test]
    fn grant_change_between_handoffs_is_visible_after_sync() {
        let fingerprint = [9u8; 32];
        let (table, region_id, channel) = publish_grant_table(&[(fingerprint, grants())]);
        let mut attached = GrantTable::from_target(&table_target(region_id)).expect("attach");
        assert_eq!(attached.grants_for(&fingerprint), Some(grants()));

        // The identity provider narrows this fingerprint down to no grants.
        let narrowed: Vec<CapabilityGrant> = Vec::new();
        let encoded = selium_abi::encode_rkyv(&narrowed).expect("encode narrowed");
        table
            .set(fingerprint.to_vec(), encoded)
            .expect("publish narrowed grants");

        // The next handoff synchronises before evaluating identity.
        attached.sync();
        assert_eq!(
            attached.grants_for(&fingerprint),
            Some(Vec::new()),
            "the change published while waiting applies to the next handoff"
        );

        drop(table);
        drop(channel);
    }

    /// A narrowing table attaches from its resolved target and serves the
    /// published narrowing for each tenant.
    #[test]
    fn narrowing_table_attaches_from_a_resolved_target() {
        drop(selium_memory::set_region_provider(Box::new(
            selium_memory::HeapRegionProvider::new(),
        )));
        let channel = Channel::create_with_backpressure(
            64 * 1024,
            selium_shm::ChannelBackpressure::Drop,
            selium_abi::ResourceKind::LiveTable,
        )
        .expect("narrowing table channel");
        let publisher = selium_wire::Publisher::new(selium_wire::FramedWrite::new(
            ShmTransport::new(&channel, &channel).expect("publisher transport"),
        ));
        let subscriber = Subscriber::new(
            selium_wire::FramedRead::new(
                ShmTransport::new(&channel, &channel).expect("subscriber transport"),
            ),
            None,
        );
        let table: selium_wire::LiveTable<String, Vec<u8>, ShmTransport> =
            selium_wire::LiveTable::new(
                selium_wire::TableWriterAuthority::claim(format!(
                    "bridge.test.{}",
                    channel.region_id()
                ))
                .expect("authority"),
                publisher,
                subscriber,
            )
            .expect("live table");
        let narrowing = vec![Capability::Network];
        table
            .set(
                "acme".to_string(),
                selium_abi::encode_rkyv(&narrowing).expect("encode narrowing"),
            )
            .expect("publish narrowing");

        let attached =
            NarrowingTable::from_target(&table_target(channel.region_id())).expect("attach");
        assert_eq!(attached.narrowing_for("acme"), narrowing);
        assert!(
            attached.narrowing_for("beta").is_empty(),
            "an absent tenant stays un-narrowed"
        );

        drop(table);
        drop(channel);
    }

    /// The convergence helper attaches a table when its route appears: the
    /// watch event alone confers nothing, so the resolver's authoritative
    /// target is what the view is built from.
    // scenario: guest-bridge/bridge-tables-reconverge/table-publishes-after-bridge-startup
    #[tokio::test]
    async fn grant_watch_appearance_attaches_the_table() {
        let fingerprint = [5u8; 32];
        let (table, region_id, channel) = publish_grant_table(&[(fingerprint, grants())]);
        let mut watch = None;

        let attached = converge_table(
            Ok(RouteEvent::Appeared(table_target(region_id))),
            GrantTable::empty(),
            &mut watch,
            "test grant table",
            || std::future::ready(Ok(Some(table_target(region_id)))),
            GrantTable::from_target,
            GrantTable::empty,
        )
        .await;
        assert_eq!(attached.grants_for(&fingerprint), Some(grants()));

        drop(table);
        drop(channel);
    }

    /// Repeated discovery notifications for the same provider must not attach
    /// its region again: guest memories permit only one attachment per region.
    #[tokio::test]
    async fn grant_watch_redundant_events_preserve_the_attached_view() {
        let fingerprint = [5u8; 32];
        let (table, region_id, channel) = publish_grant_table(&[(fingerprint, grants())]);
        let attachments = std::cell::Cell::new(0usize);
        let attach = |target: &ResourceTarget| {
            attachments.set(attachments.get() + 1);
            GrantTable::from_target(target)
        };
        let mut watch = None;

        let mut attached = converge_table(
            Ok(RouteEvent::Appeared(table_target(region_id))),
            GrantTable::empty(),
            &mut watch,
            "test grant table",
            || std::future::ready(Ok(Some(table_target(region_id)))),
            &attach,
            GrantTable::empty,
        )
        .await;

        for event in [
            RouteEvent::Appeared(table_target(region_id)),
            RouteEvent::Replaced(table_target(region_id)),
        ] {
            attached = converge_table(
                Ok(event),
                attached,
                &mut watch,
                "test grant table",
                || std::future::ready(Ok(Some(table_target(region_id)))),
                &attach,
                GrantTable::empty,
            )
            .await;
        }

        assert_eq!(attachments.get(), 1, "only the initial event attaches");
        assert_eq!(attached.grants_for(&fingerprint), Some(grants()));

        drop(table);
        drop(channel);
    }

    /// Revocation restores fail-closed: no resolution, no grants.
    #[tokio::test]
    async fn grant_watch_revocation_restores_the_fail_closed_default() {
        let fingerprint = [6u8; 32];
        let (table, region_id, channel) = publish_grant_table(&[(fingerprint, grants())]);
        let mut watch = None;

        let attached = converge_table(
            Ok(RouteEvent::Revoked),
            GrantTable::empty(),
            &mut watch,
            "test grant table",
            || std::future::ready(Ok(Some(table_target(region_id)))),
            GrantTable::from_target,
            GrantTable::empty,
        )
        .await;
        assert!(
            attached.grants_for(&fingerprint).is_none(),
            "a revoked route must not leave stale grants attached"
        );

        drop(table);
        drop(channel);
    }

    /// A stale appearance (the route went away between event and re-resolve)
    /// keeps the fail-closed default.
    #[tokio::test]
    async fn grant_watch_stale_appearance_keeps_the_default() {
        let mut watch = None;
        let attached = converge_table(
            Ok(RouteEvent::Appeared(table_target(1))),
            GrantTable::empty(),
            &mut watch,
            "test grant table",
            || std::future::ready(Ok(None)),
            GrantTable::from_target,
            GrantTable::empty,
        )
        .await;
        assert!(attached.grants_for(&[7u8; 32]).is_none());

        let mut watch = None;
        let attached = converge_table(
            Ok(RouteEvent::Replaced(table_target(1))),
            GrantTable::empty(),
            &mut watch,
            "test grant table",
            || std::future::ready(Ok(None)),
            GrantTable::from_target,
            GrantTable::empty,
        )
        .await;
        assert!(attached.grants_for(&[7u8; 32]).is_none());
    }

    /// An attach failure restores fail-closed rather than retaining a stale
    /// authority snapshot.
    #[tokio::test]
    async fn grant_watch_attach_failure_keeps_the_default() {
        let mut watch = None;
        let unattachable = table_target(9_999);
        let attached = converge_table(
            Ok(RouteEvent::Replaced(unattachable.clone())),
            GrantTable::empty(),
            &mut watch,
            "test grant table",
            || std::future::ready(Ok(Some(unattachable.clone()))),
            GrantTable::from_target,
            GrantTable::empty,
        )
        .await;
        assert!(attached.grants_for(&[7u8; 32]).is_none());
    }

    /// A terminal watch failure disables the watch and fails closed, so the
    /// caller stops polling a broken view instead of retrying it forever.
    #[tokio::test]
    async fn grant_watch_error_disables_the_watch_and_fails_closed() {
        let mut watch = None;
        let attached = converge_table(
            Err(anyhow::anyhow!("watch failed")),
            GrantTable::empty(),
            &mut watch,
            "test grant table",
            || std::future::ready(Ok(Some(table_target(1)))),
            GrantTable::from_target,
            GrantTable::empty,
        )
        .await;
        assert!(attached.grants_for(&[7u8; 32]).is_none());
        assert!(watch.is_none(), "a failed watch is not polled again");
    }

    /// A replacement event attaches the replacement region instead of keeping
    /// the previous provider's grants.
    #[tokio::test]
    async fn grant_watch_replacement_attaches_the_new_region() {
        let fingerprint = [8u8; 32];
        let (first, first_region, first_channel) = publish_grant_table(&[(fingerprint, grants())]);
        let (second, second_region, second_channel) = publish_grant_table(&[]);

        let mut watch = None;
        let attached = converge_table(
            Ok(RouteEvent::Appeared(table_target(first_region))),
            GrantTable::empty(),
            &mut watch,
            "test grant table",
            || std::future::ready(Ok(Some(table_target(first_region)))),
            GrantTable::from_target,
            GrantTable::empty,
        )
        .await;
        assert!(attached.grants_for(&fingerprint).is_some());

        let replaced = converge_table(
            Ok(RouteEvent::Replaced(table_target(second_region))),
            attached,
            &mut watch,
            "test grant table",
            || std::future::ready(Ok(Some(table_target(second_region)))),
            GrantTable::from_target,
            GrantTable::empty,
        )
        .await;
        assert!(
            replaced.grants_for(&fingerprint).is_none(),
            "the replacement's table must replace the stale view"
        );

        drop(first);
        drop(second);
        drop(first_channel);
        drop(second_channel);
    }

    /// Revoking the narrowing table fails open: an absent table narrows
    /// nothing, so no tenant is refused a capability it would otherwise have.
    #[tokio::test]
    async fn narrowing_watch_revocation_fails_open() {
        let mut watch = None;
        let table = converge_table(
            Ok(RouteEvent::Revoked),
            NarrowingTable::empty(),
            &mut watch,
            "test narrowing table",
            || std::future::ready(Ok(Some(table_target(1)))),
            NarrowingTable::from_target,
            NarrowingTable::empty,
        )
        .await;
        assert!(table.narrowing_for("acme").is_empty());
    }
    #[tokio::test]
    async fn attach_then_close_surfaces_eof_to_the_connector_peer() {
        drop(selium_memory::set_region_provider(Box::new(
            selium_memory::HeapRegionProvider::new(),
        )));
        let (ring_to_guest, ring_from_guest, shared_id, _region) =
            selium_shm::byte_channel::create(4096, 4096).expect("create byte channel");

        // Connector-side peer half of the relayed stream.
        let region = selium_memory::region_provider()
            .expect("provider")
            .attach(shared_id, None, selium_abi::RegionProt::ReadWrite)
            .expect("attach");
        let mut peer = selium_guest::net::ByteStream::from_ring_channels(
            &ring_from_guest,
            &ring_to_guest,
            region,
            true,
        )
        .expect("connector peer");

        // The refusal: attach then immediately close.
        attach_then_close(shared_id);

        // The peer observes EOF (a read of zero bytes) promptly, not a hang.
        let mut buf = [0u8; 8];
        let read = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            use tokio::io::AsyncReadExt;
            peer.read(&mut buf).await
        })
        .await
        .expect("peer must observe the close promptly")
        .expect("peer read must succeed");
        assert_eq!(read, 0, "peer observes EOF after attach-then-close");
    }
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
