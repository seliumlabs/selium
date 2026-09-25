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

use anyhow::Context as _;
use selium_abi::{
    Capability, CapabilityGrant, ResourceClass, ResourceIdentity, ResourceSelector,
    client_identity::ClientIdentity,
};
use selium_guest::{
    Context, Process, ResourceListener, Serve, entrypoint, error, info, mark_ready,
    net::ByteStream, warn,
};
use selium_service::ResourceTarget;
use selium_shm::{Channel, transport::ShmTransport};
use selium_wire::{LiveTableView, framed::FramedRead, pubsub::Subscriber};

const BRIDGE_CHANNEL_ENTRYPOINT: &str = "bridge_channel";
/// The `bridge-channel` module id and entrypoint this server spawns.
const BRIDGE_CHANNEL_MODULE: &str = "bridge-channel-module";
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
}

/// The accountant-published narrowing table: `tenant -> rkyv-encoded
/// Vec<Capability>` of capabilities the bridge subtracts from the
/// identity-published baseline grants at conferral. Until the table is
/// attached (`Some`), every tenant is un-narrowed (no-op), mirroring the
/// grant table's fail-closed absence for unknown identities.
pub struct NarrowingTable {
    table: Option<LiveTableView<String, Vec<u8>, ShmTransport>>,
}

impl GrantTable {
    /// Builds an empty (unattached) grant table: every lookup misses.
    pub fn empty() -> Self {
        Self { table: None }
    }

    /// Attaches to the identity guest's published grant table route.
    pub async fn attach(ctx: &mut Context) -> anyhow::Result<Self> {
        let target = ctx
            .lookup(GRANT_TABLE_ROUTE)
            .await
            .with_context(|| "bridge-server: identity grant table resolve failed")?
            .ok_or_else(|| anyhow::anyhow!("identity grant table route not found"))?;
        let channel = Channel::attach(target.resource_id)
            .map_err(|e| anyhow::anyhow!("grant table region attach failed: {e}"))?;
        // Replay from the ring start: grants may predate this attachment.
        let transport = ShmTransport::new_replay(&channel, &channel)
            .map_err(|e| anyhow::anyhow!("grant table transport failed: {e}"))?;
        let subscriber = Subscriber::new(FramedRead::new(transport), None);
        let table = LiveTableView::new(subscriber)
            .map_err(|e| anyhow::anyhow!("grant table view construction failed: {e}"))?;
        Ok(Self { table: Some(table) })
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
        Self { table: None }
    }

    /// Attaches to the accountant's published narrowing table route.
    pub async fn attach(ctx: &mut Context) -> anyhow::Result<Self> {
        let target = ctx
            .lookup(NARROWING_TABLE_ROUTE)
            .await
            .with_context(|| "bridge-server: narrowing table resolve failed")?
            .ok_or_else(|| anyhow::anyhow!("narrowing table route not found"))?;
        let channel = Channel::attach(target.resource_id)
            .map_err(|e| anyhow::anyhow!("narrowing table region attach failed: {e}"))?;
        let transport = ShmTransport::new_replay(&channel, &channel)
            .map_err(|e| anyhow::anyhow!("narrowing table transport failed: {e}"))?;
        let subscriber = Subscriber::new(FramedRead::new(transport), None);
        let table = LiveTableView::new(subscriber)
            .map_err(|e| anyhow::anyhow!("narrowing table view construction failed: {e}"))?;
        Ok(Self { table: Some(table) })
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
async fn bridge_server(mut ctx: Context) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("bridge-server: started");

    // The server creates its own listener: self-registration replaces the
    // runtime's well-known-URI queue minting.
    let mut listener =
        ResourceListener::create().with_context(|| "bridge-server: create listener failed")?;

    // Pin the QUIC connector: handoff metadata is sender-controlled, so an
    // unpinned listener would let any guest that resolves and attaches the
    // bridge route forge an authenticated identity and mint grants. Handoffs
    // from any process other than the registered `sel-quic` handler are
    // refused by the listener.
    let connector = selium_guest::resolve_protocol_handler("sel-quic")
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

    let mut grant_table = match GrantTable::attach(&mut ctx).await {
        Ok(table) => table,
        Err(error) => {
            warn!("bridge-server: identity grant table unavailable: {error}");
            GrantTable::empty()
        }
    };
    let mut narrowing_table = match NarrowingTable::attach(&mut ctx).await {
        Ok(table) => table,
        Err(error) => {
            warn!("bridge-server: narrowing table unavailable: {error}");
            NarrowingTable::empty()
        }
    };
    mark_ready();

    loop {
        grant_table.sync();
        narrowing_table.sync();

        let incoming = match listener.recv().await {
            Ok(incoming) => incoming,
            Err(e) => {
                error!("bridge-server: handoff receive failed: {e}");
                continue;
            }
        };

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
            BRIDGE_CHANNEL_MODULE,
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
    #[test]
    fn narrow_grants_with_empty_narrowing_is_a_noop() {
        let baseline = grants();
        assert_eq!(narrow_grants(baseline.clone(), &[]), baseline);
    }

    /// An unattached narrowing table narrows nothing: every tenant is a no-op.
    #[test]
    fn empty_narrowing_table_narrows_nothing() {
        let table = NarrowingTable::empty();
        assert!(table.narrowing_for("acme").is_empty());
        assert!(table.narrowing_for("beta").is_empty());
    }

    /// 4.5 (test uplift): refusing an unknown identity attaches the
    /// delivered region and closes it, so the connector-side peer observes
    /// EOF rather than parking on a region nobody attaches.
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
