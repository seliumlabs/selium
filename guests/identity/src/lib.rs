//! Selium identity system guest — the platform's PKI policy authority.
//!
//! The identity guest owns tenant and user-certificate **policy**: who mints
//! what, when to rotate or revoke, and which principals hold which baseline
//! grants. It mints certificates through the host-held signing oracle
//! (`SignTenantCa` / `CommitTenantCa` / `RollbackTenantCa` / `SignUserCert` /
//! `RevokeCa`, gated by `MintCertificate`)
//! and never holds private-key material itself.
//!
//! State takes the established state-machine shape:
//!
//! - two identity-owned durable logs — a **tenant registry**
//!   (`tenant -> tenant CA certificate`) and a **principal registry**
//!   (`fingerprint -> baseline grants`) — are replayed into in-memory
//!   projections on boot;
//! - the projections are published as **live tables** (`ResourceKind` rings):
//!   `client-ca-<tenant>` trust anchors for the connector's union verifier, and
//!   `fingerprint -> grants` for the bridge-server's handoff conferral.
//!
//! The request surface is **tiered**. The tier is derived from the caller's
//! process tenant, never from a field in the request: a root/system principal
//! is the operator tier (tenant create/rotate/revoke); a tenant-scoped caller
//! may only act on its own tenant (issue user certificates, manage its own
//! principals).

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use anyhow::Context as _;
use rkyv::{Archive, Deserialize, Serialize};
use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceKind, ResourceSelector};
use selium_guest::{Context, DurableLog, ResourceListener, Serve, entrypoint, info, spawn, warn};
use selium_service::{
    DiscoveryRequest, DiscoveryResponse, FlatMsg, IdentityRequest, IdentityResponse, ResourceTarget,
};
use selium_shm::{Channel, ChannelBackpressure, transport::ShmTransport};
use selium_wire::{
    LiveTable,
    framed::{FramedRead, FramedWrite},
    pubsub::{Publisher, Subscriber},
};
use sha2::{Digest, Sha256};

/// Anchor-table key prefix: the connector reads `client-ca-<tenant>` entries.
pub const ANCHOR_KEY_PREFIX: &str = "client-ca-";
/// Serving route path for the anchor live table (`sel:///identity-anchors`).
pub const ANCHOR_TABLE_PATH: &str = "identity-anchors";

/// Single-writer authority topic for the anchor live table.
const ANCHOR_TABLE_TOPIC: &str = "identity.anchors";
/// Durable log name for the advisory domain registry.
pub const DOMAIN_LOG: &str = "selium.identity.domains";
/// Serving route path for the grant live table (`sel:///identity-grants`).
pub const GRANT_TABLE_PATH: &str = "identity-grants";

/// Single-writer authority topic for the grant live table.
const GRANT_TABLE_TOPIC: &str = "identity.grants";
/// Serving route path for the request surface (`sel:///identity`).
pub const IDENTITY_PATH: &str = "identity";
/// Durable log name for the principal registry.
pub const PRINCIPAL_LOG: &str = "selium.identity.principals";
/// Ring capacity for each live table.
const TABLE_CAPACITY: u64 = 64 * 1024;
/// How often durably recorded domain mappings are republished to discovery.
/// A restarted discovery converges within one interval without operator
/// intervention.
const DOMAIN_REPUBLISH_INTERVAL_S: u64 = 5;
/// Durable log name for the tenant registry.
pub const TENANT_LOG: &str = "selium.identity.tenants";

/// An advisory domain registry record appended to the durable log.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum DomainRecord {
    /// Record a domain→tenant mapping.
    Set {
        /// Domain (e.g. `example.com`).
        domain: String,
        /// Tenant the domain maps to.
        tenant: String,
    },
    /// Remove a domain mapping.
    Remove { domain: String },
}

/// A tenant registry record appended to the durable log.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum TenantRecord {
    /// A tenant CA was minted or rotated: record its current anchor.
    Onboard {
        /// Tenant name.
        tenant: String,
        /// DER-encoded tenant CA certificate (public).
        ca_cert_der: Vec<u8>,
    },
    /// A tenant CA was revoked.
    Revoke { tenant: String },
}

/// A principal registry record appended to the durable log.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub enum PrincipalRecord {
    /// Record a principal's baseline grants (issuance records an empty set).
    Set {
        /// Tenant the principal belongs to.
        tenant: String,
        /// SHA-256 fingerprint of the principal's leaf SPKI.
        fingerprint: Vec<u8>,
        /// rkyv-encoded baseline grants.
        grants: Vec<u8>,
    },
    /// Remove a principal's baseline grants.
    Remove {
        /// Tenant the principal belongs to.
        tenant: String,
        /// SHA-256 fingerprint of the principal's leaf SPKI.
        fingerprint: Vec<u8>,
    },
}

/// The replayed tenant projection: `tenant -> current CA certificate DER`.
#[derive(Debug, Clone, Default)]
pub struct TenantRegistry {
    by_tenant: BTreeMap<String, Vec<u8>>,
    /// Current revocation tombstones. Replay retains these so host-side CA
    /// deletion is retried even when the tenant is absent from the projection.
    revoked: BTreeSet<String>,
}

/// The replayed principal projection: `fingerprint -> baseline grants bytes`.
#[derive(Debug, Clone, Default)]
pub struct PrincipalRegistry {
    by_fingerprint: BTreeMap<Vec<u8>, Vec<u8>>,
}

/// The replayed advisory domain projection: `domain -> tenant`.
#[derive(Debug, Clone, Default)]
pub struct DomainRegistry {
    by_domain: BTreeMap<String, String>,
}

/// The caller's tier, derived from its process tenant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tier {
    /// Root/system principal: operator tier, may manage any tenant.
    Operator,
    /// Tenant-scoped principal: may act only on this tenant.
    Tenant(String),
    /// The caller's tenant could not be verified: every request is refused.
    Denied,
}

/// Shared identity state handed to each request handler.
struct IdentityState {
    /// One commit lane for the whole guest. The lane covers durable append and
    /// sync through projection and enforcement, so completion order cannot
    /// invert the ledger's mutation order.
    mutations: tokio::sync::Mutex<()>,
    tenants: Arc<Mutex<TenantRegistry>>,
    principals: Arc<Mutex<PrincipalRegistry>>,
    domains: Arc<Mutex<DomainRegistry>>,
    tenant_log: DurableLog,
    principal_log: DurableLog,
    domain_log: DurableLog,
    anchors: Arc<LiveTable<String, Vec<u8>, ShmTransport>>,
    grants: Arc<LiveTable<Vec<u8>, Vec<u8>, ShmTransport>>,
    /// Current discovery listener, advanced when the route watch observes a
    /// replacement and its attach basis has been recorded.
    discovery_listener: Mutex<u64>,
}

impl TenantRegistry {
    /// Applies one tenant record in replay order.
    pub fn apply_record(&mut self, record: TenantRecord) {
        match record {
            TenantRecord::Onboard {
                tenant,
                ca_cert_der,
            } => {
                self.revoked.remove(&tenant);
                self.by_tenant.insert(tenant, ca_cert_der);
            }
            TenantRecord::Revoke { tenant } => {
                self.by_tenant.remove(&tenant);
                self.revoked.insert(tenant);
            }
        }
    }

    /// Materialises the projection from a replayed durable log.
    pub async fn rebuild(&mut self, log: &DurableLog) -> selium_guest::Result<()> {
        let payloads = log
            .replay(None, u32::MAX)
            .await?
            .into_iter()
            .map(|record| record.payload);
        self.rebuild_payloads(payloads);
        Ok(())
    }

    /// Re-applies replayed record payloads in log order. Undecodable
    /// records are skipped with a warning: a torn write must not lose the
    /// rest of the registry.
    pub fn rebuild_payloads(&mut self, payloads: impl IntoIterator<Item = Vec<u8>>) {
        for payload in payloads {
            match selium_abi::decode_rkyv::<TenantRecord>(&payload) {
                Ok(record) => self.apply_record(record),
                Err(error) => warn!("identity: skipping undecodable tenant record: {error}"),
            }
        }
    }

    /// Returns the tenant's current CA certificate, if onboarded.
    pub fn certificate(&self, tenant: &str) -> Option<&Vec<u8>> {
        self.by_tenant.get(tenant)
    }

    /// Iterates the projected `(tenant, ca_cert_der)` pairs.
    pub fn entries(&self) -> impl Iterator<Item = (&String, &Vec<u8>)> {
        self.by_tenant.iter()
    }

    /// Tenants whose latest durable record is a revocation.
    pub fn revoked(&self) -> impl Iterator<Item = &String> {
        self.revoked.iter()
    }
}

impl PrincipalRegistry {
    /// Applies one principal record in replay order.
    pub fn apply_record(&mut self, record: PrincipalRecord) {
        match record {
            PrincipalRecord::Set {
                fingerprint,
                grants,
                ..
            } => {
                self.by_fingerprint.insert(fingerprint, grants);
            }
            PrincipalRecord::Remove { fingerprint, .. } => {
                self.by_fingerprint.remove(&fingerprint);
            }
        }
    }

    /// Materialises the projection from a replayed durable log.
    pub async fn rebuild(&mut self, log: &DurableLog) -> selium_guest::Result<()> {
        let payloads = log
            .replay(None, u32::MAX)
            .await?
            .into_iter()
            .map(|record| record.payload);
        self.rebuild_payloads(payloads);
        Ok(())
    }

    /// Re-applies replayed record payloads in log order. Undecodable
    /// records are skipped with a warning: a torn write must not lose the
    /// rest of the registry.
    pub fn rebuild_payloads(&mut self, payloads: impl IntoIterator<Item = Vec<u8>>) {
        for payload in payloads {
            match selium_abi::decode_rkyv::<PrincipalRecord>(&payload) {
                Ok(record) => self.apply_record(record),
                Err(error) => warn!("identity: skipping undecodable principal record: {error}"),
            }
        }
    }

    /// Returns a principal's baseline grants, if recorded.
    pub fn grants(&self, fingerprint: &[u8]) -> Option<&Vec<u8>> {
        self.by_fingerprint.get(fingerprint)
    }

    /// Iterates the projected `(fingerprint, grants)` pairs.
    pub fn entries(&self) -> impl Iterator<Item = (&Vec<u8>, &Vec<u8>)> {
        self.by_fingerprint.iter()
    }
}

impl DomainRegistry {
    /// Applies one domain record in replay order.
    pub fn apply_record(&mut self, record: DomainRecord) {
        match record {
            DomainRecord::Set { domain, tenant } => {
                self.by_domain.insert(domain, tenant);
            }
            DomainRecord::Remove { domain } => {
                self.by_domain.remove(&domain);
            }
        }
    }

    /// Materialises the projection from a replayed durable log.
    pub async fn rebuild(&mut self, log: &DurableLog) -> selium_guest::Result<()> {
        let payloads = log
            .replay(None, u32::MAX)
            .await?
            .into_iter()
            .map(|record| record.payload);
        self.rebuild_payloads(payloads);
        Ok(())
    }

    /// Re-applies replayed record payloads in log order. Undecodable records
    /// are skipped with a warning.
    pub fn rebuild_payloads(&mut self, payloads: impl IntoIterator<Item = Vec<u8>>) {
        for payload in payloads {
            match selium_abi::decode_rkyv::<DomainRecord>(&payload) {
                Ok(record) => self.apply_record(record),
                Err(error) => warn!("identity: skipping undecodable domain record: {error}"),
            }
        }
    }

    /// Returns the recorded mappings, in domain order.
    pub fn entries(&self) -> impl Iterator<Item = (&String, &String)> {
        self.by_domain.iter()
    }
}

impl Tier {
    /// Derives the tier from a verified caller tenant.
    pub fn from_tenant(tenant: Option<String>) -> Self {
        match tenant {
            Some(tenant) => Self::Tenant(tenant),
            None => Self::Operator,
        }
    }

    /// Returns whether the caller may use operator verbs.
    pub fn is_operator(&self) -> bool {
        matches!(self, Self::Operator)
    }

    /// Returns whether the caller may act on `tenant`.
    pub fn admits_tenant(&self, tenant: &str) -> bool {
        match self {
            Self::Operator => true,
            Self::Tenant(own) => own == tenant,
            Self::Denied => false,
        }
    }
}

/// The anchor-table key for a tenant.
pub fn anchor_key(tenant: &str) -> String {
    format!("{ANCHOR_KEY_PREFIX}{tenant}")
}

/// SHA-256 of a client leaf SPKI.
pub fn fingerprint_of(spki_der: &[u8]) -> Vec<u8> {
    let digest = Sha256::digest(spki_der);
    digest.as_slice().to_vec()
}

/// The grant set assigned to the identity guest: mint authority, storage (the
/// two registries' durable volumes), shared memory (live-table rings and RPC
/// session rings), host queues (the serving listener plus the discovery
/// client), and root-namespace registration.
pub fn identity_grants() -> Vec<CapabilityGrant> {
    vec![
        CapabilityGrant::new(Capability::MintCertificate, Vec::new()),
        CapabilityGrant::new(Capability::DomainWrite, Vec::new()),
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
            vec![ResourceSelector::Uri(root_route(IDENTITY_PATH))],
        ),
        CapabilityGrant::new(
            Capability::RouteClaim,
            vec![ResourceSelector::Uri(root_route(ANCHOR_TABLE_PATH))],
        ),
        CapabilityGrant::new(
            Capability::RouteClaim,
            vec![ResourceSelector::Uri(root_route(GRANT_TABLE_PATH))],
        ),
    ]
}

/// Builds one live table over its own `LiveTable` ring, returning the ring id
/// and the table.
fn create_live_table<K, V>(
    capacity: u64,
    topic: &str,
) -> selium_wire::Result<(u64, LiveTable<K, V, ShmTransport>)>
where
    K: FlatMsg + Clone + Eq + std::hash::Hash,
    V: FlatMsg + Clone,
{
    let channel = Channel::create_with_backpressure(
        capacity,
        ChannelBackpressure::Drop,
        ResourceKind::LiveTable,
    )?;
    let region_id = channel.region_id();
    let write = ShmTransport::new(&channel, &channel)?;
    let read = ShmTransport::new(&channel, &channel)?;
    let publisher = Publisher::new(FramedWrite::new(write));
    let subscriber = Subscriber::new(FramedRead::new(read), None);
    let authority = selium_wire::TableWriterAuthority::claim(topic)?;
    let table = LiveTable::new(authority, publisher, subscriber)?;
    Ok((region_id, table))
}

/// Serves one RPC session, deriving the tier once from the caller's tenant and
/// enforcing it on every request.
async fn handle_connection(
    mut connection: selium_shm::rpc::RpcConnection<IdentityRequest, IdentityResponse>,
    state: Arc<IdentityState>,
) {
    let tier = match selium_guest::process_tenant(connection.client_process_id()) {
        Ok(tenant) => Tier::from_tenant(tenant),
        Err(error) => {
            warn!(
                client = connection.client_process_id(),
                "identity: could not resolve caller tenant: {error}"
            );
            Tier::Denied
        }
    };

    loop {
        match connection.recv().await {
            Ok(request) => {
                let response = match request.payload() {
                    Ok(payload) => handle_request(payload, &tier, &state).await,
                    Err(error) => {
                        warn!("identity: request decode failed: {error}");
                        IdentityResponse::Error {
                            step: "decode".to_string(),
                            context: format!("{error}"),
                        }
                    }
                };
                if let Err(error) = request.reply(response).await {
                    warn!("identity: reply failed: {error}");
                    break;
                }
            }
            Err(selium_shm::rpc::RpcError::ConnectionClosed) => break,
            Err(error) => {
                warn!("identity: recv failed: {error}");
                break;
            }
        }
    }
}

/// Runs one mutation in the guest-wide commit lane.
async fn serialised_mutation<R>(
    lane: &tokio::sync::Mutex<()>,
    mutation: impl std::future::Future<Output = R>,
) -> R {
    let _guard = lane.lock().await;
    mutation.await
}

/// Handles one decoded request in the commit lane, enforcing the caller's tier.
async fn handle_request(
    request: IdentityRequest,
    tier: &Tier,
    state: &IdentityState,
) -> IdentityResponse {
    serialised_mutation(
        &state.mutations,
        handle_request_locked(request, tier, state),
    )
    .await
}

async fn handle_request_locked(
    request: IdentityRequest,
    tier: &Tier,
    state: &IdentityState,
) -> IdentityResponse {
    match request {
        IdentityRequest::MintTenantCa { tenant } => {
            if !tier.is_operator() {
                return tier_refused("mint a tenant CA");
            }
            match mint_tenant_ca(&tenant, state).await {
                Ok(()) => IdentityResponse::TenantRecorded { tenant },
                Err(context) => IdentityResponse::Error {
                    step: "mint".to_string(),
                    context,
                },
            }
        }
        IdentityRequest::RotateTenantCa { tenant } => {
            if !tier.is_operator() {
                return tier_refused("rotate a tenant CA");
            }
            match mint_tenant_ca(&tenant, state).await {
                Ok(()) => IdentityResponse::Rotated { tenant },
                Err(context) => IdentityResponse::Error {
                    step: "rotate".to_string(),
                    context,
                },
            }
        }
        IdentityRequest::RevokeTenant { tenant } => {
            if !tier.is_operator() {
                return tier_refused("revoke a tenant");
            }
            match revoke_tenant(&tenant, state).await {
                Ok(()) => IdentityResponse::Revoked { tenant },
                Err(context) => IdentityResponse::Error {
                    step: "revoke".to_string(),
                    context,
                },
            }
        }
        IdentityRequest::IssueUserCert { tenant, spki_der } => {
            if !tier.admits_tenant(&tenant) {
                return tier_refused("issue a user certificate");
            }
            match issue_user_cert(&tenant, &spki_der, state).await {
                Ok(certificate_der) => IdentityResponse::UserCertIssued { certificate_der },
                Err(context) => IdentityResponse::Error {
                    step: "issue".to_string(),
                    context,
                },
            }
        }
        IdentityRequest::SetPrincipalGrants {
            tenant,
            fingerprint,
            grants,
        } => {
            if !tier.admits_tenant(&tenant) {
                return tier_refused("manage principals");
            }
            match set_principal_grants(&tenant, &fingerprint, &grants, state).await {
                Ok(()) => IdentityResponse::PrincipalRecorded { fingerprint },
                Err(context) => IdentityResponse::Error {
                    step: "principal".to_string(),
                    context,
                },
            }
        }
        IdentityRequest::RemovePrincipal {
            tenant,
            fingerprint,
        } => {
            if !tier.admits_tenant(&tenant) {
                return tier_refused("manage principals");
            }
            match remove_principal(&tenant, &fingerprint, state).await {
                Ok(()) => IdentityResponse::PrincipalRemoved { fingerprint },
                Err(context) => IdentityResponse::Error {
                    step: "principal".to_string(),
                    context,
                },
            }
        }
        IdentityRequest::SetDomain { domain, tenant } => {
            if !tier.is_operator() {
                return tier_refused("set a domain mapping");
            }
            match set_domain(&domain, &tenant, state).await {
                Ok(()) => IdentityResponse::DomainRecorded { domain },
                Err(context) => IdentityResponse::Error {
                    step: "domain".to_string(),
                    context,
                },
            }
        }
    }
}

/// Identity entrypoint: replays the registries, publishes the live tables under
/// the root namespace, registers the request surface, and serves the tiered
/// operator/tenant RPC.
#[entrypoint]
async fn identity_main(mut ctx: Context) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("identity: started");

    let tenant_log = ctx
        .open_log(TENANT_LOG)
        .await
        .with_context(|| "identity: tenant log open failed")?;
    let principal_log = ctx
        .open_log(PRINCIPAL_LOG)
        .await
        .with_context(|| "identity: principal log open failed")?;
    let domain_log = ctx
        .open_log(DOMAIN_LOG)
        .await
        .with_context(|| "identity: domain log open failed")?;

    let mut tenant_registry = TenantRegistry::default();
    tenant_registry
        .rebuild(&tenant_log)
        .await
        .with_context(|| "identity: tenant registry replay failed")?;
    let tenants = Arc::new(Mutex::new(tenant_registry));
    let mut principal_registry = PrincipalRegistry::default();
    principal_registry
        .rebuild(&principal_log)
        .await
        .with_context(|| "identity: principal registry replay failed")?;
    let principals = Arc::new(Mutex::new(principal_registry));
    let mut domain_registry = DomainRegistry::default();
    domain_registry
        .rebuild(&domain_log)
        .await
        .with_context(|| "identity: domain registry replay failed")?;
    let domains = Arc::new(Mutex::new(domain_registry));

    let (anchor_region, anchors) =
        create_live_table::<String, Vec<u8>>(TABLE_CAPACITY, ANCHOR_TABLE_TOPIC)
            .map_err(|e| anyhow::anyhow!("identity: anchor table create failed: {e}"))?;
    let (grant_region, grants) =
        create_live_table::<Vec<u8>, Vec<u8>>(TABLE_CAPACITY, GRANT_TABLE_TOPIC)
            .map_err(|e| anyhow::anyhow!("identity: grant table create failed: {e}"))?;

    // Re-publish replayed state into the fresh tables.
    let anchors = Arc::new(anchors);
    let grants = Arc::new(grants);
    {
        let tenants = tenants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (tenant, cert) in tenants.entries() {
            if let Err(e) = anchors.set(anchor_key(tenant), cert.clone()) {
                warn!("identity: anchor re-publish failed for {tenant}: {e}");
            }
        }
    }
    {
        let principals = principals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (fingerprint, grant_bytes) in principals.entries() {
            if let Err(e) = publish_grants(&grants, fingerprint, grant_bytes) {
                warn!("identity: grant re-publish failed: {e}");
            }
        }
    }

    // Re-publish replayed domain mappings to discovery (the identity guest owns
    // the advisory mapping durably; discovery stays a resolver).
    let domain_entries: Vec<(String, String)> = {
        let domains = domains
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        domains
            .entries()
            .map(|(domain, tenant)| (domain.clone(), tenant.clone()))
            .collect()
    };
    for (domain, tenant) in &domain_entries {
        if let Err(e) = publish_domain(&mut ctx, domain, tenant).await {
            warn!("identity: domain re-publish failed for {domain}: {e}");
        }
    }

    // The request surface.
    let listener =
        ResourceListener::create().with_context(|| "identity: create listener failed")?;
    let queue_target = ResourceTarget {
        uri: String::new(),
        host_id: String::new(),
        resource_id: listener.descriptor().shared_id,
        interface: None,
        tenant: None,
        class: ResourceClass::HostQueue,
        labels: Vec::new(),
    };
    ctx.serve(Serve {
        path: vec![IDENTITY_PATH.to_string()],
        target: queue_target,
        default: false,
    })
    .await
    .with_context(|| "identity: serve request surface failed")?;

    // Publish the live tables under their own resolveable routes.
    let anchor_target = ResourceTarget {
        uri: String::new(),
        host_id: String::new(),
        resource_id: anchor_region,
        interface: None,
        tenant: None,
        class: ResourceClass::SharedRegion,
        labels: Vec::new(),
    };
    ctx.serve(Serve {
        path: vec![ANCHOR_TABLE_PATH.to_string()],
        target: anchor_target,
        default: false,
    })
    .await
    .with_context(|| "identity: serve anchor table failed")?;

    let grant_target = ResourceTarget {
        uri: String::new(),
        host_id: String::new(),
        resource_id: grant_region,
        interface: None,
        tenant: None,
        class: ResourceClass::SharedRegion,
        labels: Vec::new(),
    };
    ctx.serve(Serve {
        path: vec![GRANT_TABLE_PATH.to_string()],
        target: grant_target,
        default: false,
    })
    .await
    .with_context(|| "identity: serve grant table failed")?;

    let state = Arc::new(IdentityState {
        mutations: tokio::sync::Mutex::new(()),
        tenants,
        principals,
        domains: domains.clone(),
        tenant_log,
        principal_log,
        domain_log,
        anchors,
        grants,
        discovery_listener: Mutex::new(ctx.raw_handle()),
    });

    // A committed revocation includes host-side key deletion. Retry that
    // effect on every restart; absence is an idempotent success.
    if let Err(error) = reconcile_tenant_revocations(&state) {
        warn!("identity: tenant CA replay reconciliation incomplete: {error}");
    }

    // Health is published state: advertise the identity guest's health.
    if let Err(error) = ctx.publish_health("identity").await {
        warn!("identity: health publish failed: {error}");
    }

    // Re-publish the domain mappings periodically so a discovery restart
    // converges without operator intervention. A failed session is replaced
    // through the host's current-listener resolution, never by reusing the
    // startup queue handle.
    let republish_state = state.clone();
    spawn(async move {
        loop {
            // Re-resolve before every pass, not only after an RPC error: a
            // replaced queue can leave an established session parked forever.
            // A fresh context is cheap and guarantees this pass cannot publish
            // to the startup listener after discovery has been replaced.
            match Context::rediscover().await {
                Ok(mut current) => {
                    let listener = current.raw_handle();
                    *republish_state
                        .discovery_listener
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = listener;
                    info!("identity: connected to discovery listener {listener}");
                    // Reconciliation uses the same commit lane as requests. It
                    // cannot publish an older effect after a newer request commits.
                    let _guard = republish_state.mutations.lock().await;
                    if let Err(error) = republish_domains(&republish_state, &mut current).await {
                        warn!("identity: periodic domain re-publish failed: {error}");
                    }
                    if let Err(error) = reconcile_tenant_revocations(&republish_state) {
                        warn!("identity: periodic tenant CA reconciliation failed: {error}");
                    }
                    drop(_guard);
                }
                Err(error) => {
                    warn!("identity: discovery listener resolution failed: {error}");
                }
            }
            let deadline = match selium_guest::Instant::now() {
                Ok(now) => now
                    .checked_add(std::time::Duration::from_secs(DOMAIN_REPUBLISH_INTERVAL_S))
                    .unwrap_or(selium_guest::Instant::MAX),
                Err(_) => selium_guest::Instant::MAX,
            };
            if let Err(error) = selium_guest::Timer::new(deadline).await {
                warn!("identity: domain re-publish timer failed: {error}");
            }
        }
    });

    // Serve the request surface for the lifetime of the process.
    loop {
        let incoming = match listener.recv().await {
            Ok(incoming) => incoming,
            Err(error) => {
                warn!("identity: accept failed: {error}");
                continue;
            }
        };
        let connection =
            match selium_shm::rpc::accept::<IdentityRequest, IdentityResponse>(incoming.into()) {
                Ok(connection) => connection,
                Err(error) => {
                    warn!("identity: rpc accept failed: {error}");
                    continue;
                }
            };
        spawn(handle_connection(connection, state.clone()));
    }
}

/// Re-publishes every durably recorded domain mapping over an established
/// discovery session. Any error causes the caller to drop that session and
/// resolve the current listener before the next pass.
async fn republish_domains(state: &IdentityState, ctx: &mut Context) -> Result<(), String> {
    let entries: Vec<(String, String)> = {
        let domains = state
            .domains
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        domains
            .entries()
            .map(|(domain, tenant)| (domain.clone(), tenant.clone()))
            .collect()
    };
    for (domain, tenant) in entries {
        publish_domain(ctx, &domain, &tenant)
            .await
            .map_err(|error| format!("{domain}: {error}"))?;
        info!("identity: re-published domain {domain}={tenant}");
    }
    Ok(())
}

/// Retries host CA deletion for every currently revoked tenant.
fn reconcile_tenant_revocations(state: &IdentityState) -> Result<(), String> {
    let revoked: Vec<String> = {
        let tenants = state
            .tenants
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tenants.revoked().cloned().collect()
    };
    reconcile_revocations_with(&revoked, revoke_ca_if_present)
}

fn reconcile_revocations_with(
    revoked: &[String],
    mut revoke: impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
    let mut first_error = None;
    for tenant in revoked {
        if let Err(error) = revoke(tenant)
            && first_error.is_none()
        {
            first_error = Some(format!("tenant {tenant}: {error}"));
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Host deletion is idempotent for identity reconciliation: an earlier request
/// may have deleted the key and crashed before its acknowledgement arrived.
fn revoke_ca_if_present(tenant: &str) -> Result<(), String> {
    match selium_guest::revoke_ca(tenant) {
        Ok(()) => Ok(()),
        Err(selium_guest::GuestError::Hostcall { message, .. })
            if message.contains("tenant CA not found") =>
        {
            Ok(())
        }
        Err(error) => Err(format!("tenant CA key deletion failed: {error}")),
    }
}

/// Issues a short-TTL leaf certificate from a client SPKI, recording the
/// principal's fingerprint in the registry.
async fn issue_user_cert(
    tenant: &str,
    spki_der: &[u8],
    state: &IdentityState,
) -> Result<Vec<u8>, String> {
    let certificate_der =
        selium_guest::sign_user_cert(tenant, spki_der).map_err(|error| format!("{error}"))?;
    let fingerprint = fingerprint_of(spki_der);

    // Record the fingerprint within the tenant; preserve any baseline grants
    // already held by this principal across an issuance (a leaf rotation).
    let grants = state
        .principals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .grants(&fingerprint)
        .cloned()
        .unwrap_or_default();
    record_principal(tenant, &fingerprint, &grants, state)
        .await
        .map_err(|error| format!("principal record failed: {error}"))?;
    Ok(certificate_der)
}

/// Appends a record and completes its sync barrier.
///
/// Identity mutations acknowledge only after this returns `Ok`: an append that
/// is not yet on stable storage is not a committed mutation, so neither the
/// projection nor the caller may treat it as one.
async fn append_durably(log: &DurableLog, label: &str, payload: Vec<u8>) -> Result<(), String> {
    let timestamp_ms = selium_guest::time::now()
        .map(|nanos| nanos / 1_000_000)
        .map_err(|error| format!("{label} clock failed: {error}"))?;
    log.append(timestamp_ms, Vec::new(), payload)
        .await
        .map(|_sequence| ())
        .map_err(|error| format!("{label} append failed: {error}"))?;
    log.sync()
        .await
        .map_err(|error| format!("{label} sync failed: {error}"))
}

/// Mints (or rotates) a tenant CA, recording it in the registry and publishing
/// its trust anchor.
async fn mint_tenant_ca(tenant: &str, state: &IdentityState) -> Result<(), String> {
    let timestamp_ms = selium_guest::time::now()
        .map(|nanos| nanos / 1_000_000)
        .map_err(|error| format!("tenant record clock failed: {error}"))?;
    let ca_cert_der = selium_guest::sign_tenant_ca(tenant)
        .map_err(|error| format!("tenant {tenant} CA mint failed: {error}"))?;

    let payload = match selium_abi::encode_rkyv(&TenantRecord::Onboard {
        tenant: tenant.to_string(),
        ca_cert_der: ca_cert_der.clone(),
    }) {
        Ok(payload) => payload,
        Err(error) => {
            let commit_error = format!("tenant record encode failed: {error}");
            return Err(rollback_uncommitted_ca(
                tenant,
                &ca_cert_der,
                commit_error,
                rollback_ca,
            ));
        }
    };
    if let Err(error) = state
        .tenant_log
        .append(timestamp_ms, Vec::new(), payload)
        .await
    {
        // The host mint has already replaced this tenant's key. A definite
        // append failure has no registry record to reconcile, so compensate
        // immediately and surface a rollback failure rather than hiding it.
        let commit_error = format!("tenant record append failed: {error}");
        return Err(rollback_uncommitted_ca(
            tenant,
            &ca_cert_der,
            commit_error,
            rollback_ca,
        ));
    }
    if let Err(error) = state.tenant_log.sync().await {
        // Append success makes the record potentially recoverable even though
        // its durability barrier failed. Do not delete the matching CA out
        // from under that record: retain both for restart replay and say
        // exactly which part remains unresolved.
        return Err(format!(
            "tenant {tenant} CA and registry record were appended, but tenant record sync failed: {error}; the CA is retained for restart reconciliation"
        ));
    }

    selium_guest::commit_tenant_ca(tenant, &ca_cert_der).map_err(|error| {
        format!(
            "tenant record committed but CA transaction finalisation failed: {error}; the appended CA is retained for restart reconciliation"
        )
    })?;
    state
        .tenants
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .apply_record(TenantRecord::Onboard {
            tenant: tenant.to_string(),
            ca_cert_der: ca_cert_der.clone(),
        });
    state
        .anchors
        .set(anchor_key(tenant), ca_cert_der)
        .map_err(|error| {
            format!("tenant record committed but anchor publish failed: {error}; restart reconciles the anchor")
        })?;
    Ok(())
}

fn rollback_ca(tenant: &str, ca_cert_der: &[u8]) -> Result<(), String> {
    selium_guest::rollback_tenant_ca(tenant, ca_cert_der)
        .map_err(|error| format!("tenant CA restoration failed: {error}"))
}

fn rollback_uncommitted_ca(
    tenant: &str,
    ca_cert_der: &[u8],
    commit_error: String,
    rollback: impl FnOnce(&str, &[u8]) -> Result<(), String>,
) -> String {
    match rollback(tenant, ca_cert_der) {
        Ok(()) => format!(
            "{commit_error}; the uncommitted tenant CA was rolled back and its predecessor restored"
        ),
        Err(rollback_error) => format!(
            "{commit_error}; rollback of the uncommitted tenant CA also failed: {rollback_error}"
        ),
    }
}

/// Publishes one advisory domain mapping to discovery.
async fn publish_domain(ctx: &mut Context, domain: &str, tenant: &str) -> Result<(), String> {
    match ctx
        .discovery()
        .request(DiscoveryRequest::SetDomain {
            domain: domain.to_string(),
            tenant: tenant.to_string(),
        })
        .await
    {
        Ok(DiscoveryResponse::Registered) => Ok(()),
        Ok(other) => Err(format!("unexpected discovery response: {other:?}")),
        Err(error) => Err(error.to_string()),
    }
}

/// Publishes a principal's grant set to the grant table. An empty grant set is
/// a delete: the bridge treats an absent fingerprint as "refuse the handoff".
fn publish_grants(
    table: &LiveTable<Vec<u8>, Vec<u8>, ShmTransport>,
    fingerprint: &[u8],
    grants: &[u8],
) -> selium_wire::Result<()> {
    if grants.is_empty() {
        table.delete(fingerprint.to_vec())
    } else {
        table.set(fingerprint.to_vec(), grants.to_vec())
    }
}

/// Appends a principal record and, only on success, applies it to the
/// projection.
async fn record_principal(
    tenant: &str,
    fingerprint: &[u8],
    grants: &[u8],
    state: &IdentityState,
) -> Result<(), String> {
    let record = PrincipalRecord::Set {
        tenant: tenant.to_string(),
        fingerprint: fingerprint.to_vec(),
        grants: grants.to_vec(),
    };
    let payload = selium_abi::encode_rkyv(&record)
        .map_err(|error| format!("principal record encode failed: {error}"))?;
    append_durably(&state.principal_log, "principal record", payload).await?;
    state
        .principals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .apply_record(record);
    Ok(())
}

/// Removes a principal's baseline grants after its durable removal commits.
async fn remove_principal(
    tenant: &str,
    fingerprint: &[u8],
    state: &IdentityState,
) -> Result<(), String> {
    let record = PrincipalRecord::Remove {
        tenant: tenant.to_string(),
        fingerprint: fingerprint.to_vec(),
    };
    let payload = selium_abi::encode_rkyv(&record)
        .map_err(|error| format!("principal remove encode failed: {error}"))?;
    append_durably(&state.principal_log, "principal remove", payload).await?;
    state
        .principals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .apply_record(record);
    // Post-commit enforcement is part of the operation's result. Restart
    // rebuilds a fresh table without this principal, but the caller must still
    // observe that this lifetime has not reconciled the delete yet.
    state.grants.delete(fingerprint.to_vec()).map_err(|error| {
        format!(
            "principal removal committed but grant-table deletion failed: {error}; restart reconciles the deletion"
        )
    })
}

/// Revokes a tenant CA: records the revocation durably, deletes its key from
/// the host keyring, and removes the anchor.
///
/// The durable revocation is committed first: a failed key deletion is
/// reported after commit and is reconciled by replay (the record says the
/// tenant is revoked). Failure to append leaves the prior visible state and
/// the key untouched.
async fn revoke_tenant(tenant: &str, state: &IdentityState) -> Result<(), String> {
    let payload = selium_abi::encode_rkyv(&TenantRecord::Revoke {
        tenant: tenant.to_string(),
    })
    .map_err(|error| format!("tenant revoke encode failed: {error}"))?;
    append_durably(&state.tenant_log, "tenant revoke", payload).await?;

    state
        .tenants
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .apply_record(TenantRecord::Revoke {
            tenant: tenant.to_string(),
        });

    // Attempt every committed enforcement effect even when one fails. Replay
    // retains the revocation tombstone and retries host deletion; a fresh
    // anchor table also omits the tenant on restart.
    let ca_result = revoke_ca_if_present(tenant);
    let anchor_result = state
        .anchors
        .delete(anchor_key(tenant))
        .map_err(|error| format!("anchor removal failed: {error}"));
    match (ca_result, anchor_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(ca_error), Ok(())) => Err(format!(
            "tenant revocation committed but {ca_error}; replay retries deletion"
        )),
        (Ok(()), Err(anchor_error)) => Err(format!(
            "tenant revocation committed but {anchor_error}; restart reconciles the anchor"
        )),
        (Err(ca_error), Err(anchor_error)) => Err(format!(
            "tenant revocation committed but {ca_error} and {anchor_error}; replay retries both effects"
        )),
    }
}

/// The exact root route `path` names.
fn root_route(path: &str) -> String {
    format!("{}/{}", selium_abi::uri::SEL_PREFIX, path)
}

/// Records a domain→tenant mapping durably and publishes it to discovery.
async fn set_domain(domain: &str, tenant: &str, state: &IdentityState) -> Result<(), String> {
    let record = DomainRecord::Set {
        domain: domain.to_string(),
        tenant: tenant.to_string(),
    };
    let payload = selium_abi::encode_rkyv(&record).map_err(|error| error.to_string())?;
    append_durably(&state.domain_log, "domain record", payload).await?;
    state
        .domains
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .apply_record(record);
    let listener = *state
        .discovery_listener
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut ctx = Context::from_raw(listener)
        .await
        .map_err(|error| error.to_string())?;
    publish_domain(&mut ctx, domain, tenant).await
}

/// Records a principal's baseline grant set and publishes it to the grant table.
async fn set_principal_grants(
    tenant: &str,
    fingerprint: &[u8],
    grants: &[u8],
    state: &IdentityState,
) -> Result<(), String> {
    record_principal(tenant, fingerprint, grants, state).await?;
    publish_grants(&state.grants, fingerprint, grants).map_err(|error| {
        format!(
            "principal grants committed but grant-table publication failed: {error}; restart reconciles the publication"
        )
    })
}

fn tier_refused(verb: &str) -> IdentityResponse {
    IdentityResponse::Error {
        step: "tier".to_string(),
        context: format!(
            "caller is not authorised to {verb}: a tenant-tier caller is scoped to its own tenant"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // scenario: selium-identity/tenant-ca-lifecycle/onboarding-a-tenant-publishes-its-anchor
    // scenario: selium-identity/tenant-ca-lifecycle/revoking-a-tenant-ca-removes-its-anchor
    #[test]
    fn tenant_registry_replays_onboard_and_revoke() {
        let mut registry = TenantRegistry::default();
        registry.apply_record(TenantRecord::Onboard {
            tenant: "acme".to_string(),
            ca_cert_der: vec![0x01, 0x02],
        });
        registry.apply_record(TenantRecord::Onboard {
            tenant: "beta".to_string(),
            ca_cert_der: vec![0x03],
        });
        registry.apply_record(TenantRecord::Revoke {
            tenant: "acme".to_string(),
        });

        assert_eq!(registry.certificate("acme"), None);
        assert_eq!(registry.certificate("beta"), Some(&vec![0x03]));
    }

    /// Simulates a restart replay: records appended across a prior lifetime
    /// are re-applied, in log order, to a fresh projection.
    // scenario: selium-identity/durable-state-and-replay/restart-replays-registries
    #[test]
    fn tenant_registry_replay_restores_prior_lifetime_state() {
        let prior_lifetime = [
            TenantRecord::Onboard {
                tenant: "acme".to_string(),
                ca_cert_der: vec![0x01],
            },
            TenantRecord::Onboard {
                tenant: "beta".to_string(),
                ca_cert_der: vec![0x02],
            },
            // A rotation overwrites the tenant's anchor in the same key.
            TenantRecord::Onboard {
                tenant: "beta".to_string(),
                ca_cert_der: vec![0x03],
            },
            TenantRecord::Revoke {
                tenant: "acme".to_string(),
            },
        ];
        let payloads = prior_lifetime
            .iter()
            .map(|record| selium_abi::encode_rkyv(record).expect("encode"))
            .collect::<Vec<_>>();

        let mut replayed = TenantRegistry::default();
        replayed.rebuild_payloads(payloads);

        assert_eq!(replayed.certificate("acme"), None);
        assert_eq!(replayed.certificate("beta"), Some(&vec![0x03]));
    }

    /// A torn write (undecodable payload) is skipped without losing the rest
    /// of the replayed registry.
    #[test]
    fn tenant_registry_replay_skips_torn_records() {
        let payloads = [
            selium_abi::encode_rkyv(&TenantRecord::Onboard {
                tenant: "acme".to_string(),
                ca_cert_der: vec![0x01],
            })
            .expect("encode"),
            vec![0xFF, 0x00, 0x13, 0x37],
            selium_abi::encode_rkyv(&TenantRecord::Onboard {
                tenant: "beta".to_string(),
                ca_cert_der: vec![0x02],
            })
            .expect("encode"),
        ];

        let mut replayed = TenantRegistry::default();
        replayed.rebuild_payloads(payloads);

        assert_eq!(replayed.certificate("acme"), Some(&vec![0x01]));
        assert_eq!(replayed.certificate("beta"), Some(&vec![0x02]));
    }

    #[test]
    fn tenant_record_round_trips_rkyv() {
        let record = TenantRecord::Onboard {
            tenant: "acme".to_string(),
            ca_cert_der: vec![0x30, 0x81, 0x01],
        };
        let encoded = selium_abi::encode_rkyv(&record).expect("encode");
        let decoded: TenantRecord = selium_abi::decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, record);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_mutations_project_in_ledger_order() {
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
                    // Park after A's durable append. B must not be able to
                    // append or project until A's projection has completed.
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

    #[test]
    fn failed_ca_append_rolls_back_and_returns_rollback_failure() {
        let rolled_back = std::cell::Cell::new(false);
        let error = rollback_uncommitted_ca(
            "acme",
            b"new-ca",
            "append failed".to_string(),
            |_tenant, cert| {
                assert_eq!(cert, b"new-ca");
                rolled_back.set(true);
                Ok(())
            },
        );
        assert!(rolled_back.get());
        assert!(error.contains("rolled back"));

        let error = rollback_uncommitted_ca(
            "acme",
            b"new-ca",
            "append failed".to_string(),
            |_tenant, _cert| Err("injected restoration failure".to_string()),
        );
        assert!(error.contains("append failed"));
        assert!(error.contains("injected restoration failure"));
    }

    #[test]
    fn restart_replay_retries_revoked_ca_deletion_until_it_succeeds() {
        let payloads = [
            selium_abi::encode_rkyv(&TenantRecord::Onboard {
                tenant: "acme".to_string(),
                ca_cert_der: vec![0x01],
            })
            .expect("encode onboard"),
            selium_abi::encode_rkyv(&TenantRecord::Revoke {
                tenant: "acme".to_string(),
            })
            .expect("encode revoke"),
        ];
        let mut replayed = TenantRegistry::default();
        replayed.rebuild_payloads(payloads);
        let revoked: Vec<String> = replayed.revoked().cloned().collect();
        assert_eq!(revoked, vec!["acme".to_string()]);

        let attempts = std::cell::Cell::new(0);
        let first = reconcile_revocations_with(&revoked, |_tenant| {
            attempts.set(attempts.get() + 1);
            Err("injected host failure".to_string())
        });
        assert!(first.is_err());
        let second = reconcile_revocations_with(&revoked, |_tenant| {
            attempts.set(attempts.get() + 1);
            Ok(())
        });
        second.expect("the replay retry deletes the host CA");
        assert_eq!(attempts.get(), 2);

        replayed.apply_record(TenantRecord::Onboard {
            tenant: "acme".to_string(),
            ca_cert_der: vec![0x02],
        });
        assert_eq!(replayed.revoked().count(), 0);
    }

    // scenario: selium-identity/principal-registry-and-baseline-grants/grant-table-reflects-the-registry
    // scenario: selium-identity/principal-registry-and-baseline-grants/unknown-fingerprint-is-absent
    #[test]
    fn principal_registry_replays_set_and_remove() {
        let mut registry = PrincipalRegistry::default();
        registry.apply_record(PrincipalRecord::Set {
            tenant: "acme".to_string(),
            fingerprint: vec![0xAA; 32],
            grants: vec![0x01],
        });
        registry.apply_record(PrincipalRecord::Remove {
            tenant: "acme".to_string(),
            fingerprint: vec![0xAA; 32],
        });

        assert_eq!(registry.grants(&[0xAA; 32]), None);
    }

    // scenario: addressing/advisory-domain-to-tenant-table/mapping-survives-restart
    #[test]
    fn domain_registry_replays_set_and_remove() {
        // Provisioning is durable: replaying the log restores the mapping and
        // honours removals (the identity guest owns the advisory table).
        let mut registry = DomainRegistry::default();
        let records = vec![
            DomainRecord::Set {
                domain: "example.com".to_string(),
                tenant: "acme".to_string(),
            },
            DomainRecord::Set {
                domain: "example.org".to_string(),
                tenant: "beta".to_string(),
            },
            DomainRecord::Remove {
                domain: "example.org".to_string(),
            },
        ];
        for record in records {
            let encoded = selium_abi::encode_rkyv(&record).expect("encode");
            let decoded: DomainRecord = selium_abi::decode_rkyv(&encoded).expect("decode");
            registry.apply_record(decoded);
        }

        let entries: Vec<(String, String)> = registry
            .entries()
            .map(|(domain, tenant)| (domain.clone(), tenant.clone()))
            .collect();
        assert_eq!(
            entries,
            vec![("example.com".to_string(), "acme".to_string())]
        );
    }

    /// Simulates a restart replay: a principal's grants recorded across a
    /// prior lifetime are restored to a fresh projection, with the latest
    /// grant set winning and removals honoured.
    #[test]
    fn principal_registry_replay_restores_prior_lifetime_state() {
        let prior_lifetime = [
            PrincipalRecord::Set {
                tenant: "acme".to_string(),
                fingerprint: vec![0xAA; 32],
                grants: vec![0x01],
            },
            PrincipalRecord::Set {
                tenant: "acme".to_string(),
                fingerprint: vec![0xBB; 32],
                grants: vec![0x02],
            },
            // A grant update overwrites the previous set.
            PrincipalRecord::Set {
                tenant: "acme".to_string(),
                fingerprint: vec![0xBB; 32],
                grants: vec![0x03],
            },
            PrincipalRecord::Remove {
                tenant: "acme".to_string(),
                fingerprint: vec![0xAA; 32],
            },
        ];
        let payloads = prior_lifetime
            .iter()
            .map(|record| selium_abi::encode_rkyv(record).expect("encode"))
            .collect::<Vec<_>>();

        let mut replayed = PrincipalRegistry::default();
        replayed.rebuild_payloads(payloads);

        assert_eq!(replayed.grants(&[0xAA; 32]), None);
        assert_eq!(replayed.grants(&[0xBB; 32]), Some(&vec![0x03]));
    }

    /// A torn write (undecodable payload) is skipped without losing the rest
    /// of the replayed registry.
    #[test]
    fn principal_registry_replay_skips_torn_records() {
        let payloads = [
            selium_abi::encode_rkyv(&PrincipalRecord::Set {
                tenant: "acme".to_string(),
                fingerprint: vec![0xAA; 32],
                grants: vec![0x01],
            })
            .expect("encode"),
            vec![0xFF, 0x00, 0x13, 0x37],
            selium_abi::encode_rkyv(&PrincipalRecord::Set {
                tenant: "beta".to_string(),
                fingerprint: vec![0xBB; 32],
                grants: vec![0x02],
            })
            .expect("encode"),
        ];

        let mut replayed = PrincipalRegistry::default();
        replayed.rebuild_payloads(payloads);

        assert_eq!(replayed.grants(&[0xAA; 32]), Some(&vec![0x01]));
        assert_eq!(replayed.grants(&[0xBB; 32]), Some(&vec![0x02]));
    }

    #[test]
    fn principal_record_round_trips_rkyv() {
        let record = PrincipalRecord::Set {
            tenant: "acme".to_string(),
            fingerprint: vec![0xAA; 32],
            grants: vec![0x01, 0x02],
        };
        let encoded = selium_abi::encode_rkyv(&record).expect("encode");
        let decoded: PrincipalRecord = selium_abi::decode_rkyv(&encoded).expect("decode");
        assert_eq!(decoded, record);
    }

    // scenario: selium-identity/tiered-request-surface/operator-tier-request-mints-a-tenant-ca
    #[test]
    fn operator_tier_admits_everything() {
        let tier = Tier::Operator;
        assert!(tier.is_operator());
        assert!(tier.admits_tenant("acme"));
        assert!(tier.admits_tenant("beta"));
    }

    // scenario: selium-identity/tiered-request-surface/tenant-tier-request-cannot-mint-a-tenant-ca
    #[test]
    fn tenant_tier_holds_own_tenant_only() {
        let tier = Tier::Tenant("acme".to_string());
        assert!(!tier.is_operator(), "tenant tier never mints tenant CAs");
        assert!(tier.admits_tenant("acme"));
        assert!(!tier.admits_tenant("beta"));
    }

    #[test]
    fn denied_tier_refuses_everything() {
        let tier = Tier::Denied;
        assert!(!tier.is_operator());
        assert!(!tier.admits_tenant("acme"));
    }

    #[test]
    fn fingerprint_is_sha256_of_spki() {
        let spki = b"a client subject public key info";
        let expected = Sha256::digest(spki);
        assert_eq!(fingerprint_of(spki), expected.as_slice().to_vec());
        assert_eq!(fingerprint_of(spki).len(), 32);
    }

    #[test]
    fn anchor_key_uses_client_ca_prefix() {
        assert_eq!(anchor_key("acme"), "client-ca-acme");
    }

    #[test]
    fn identity_grants_cover_the_required_authority() {
        let grants = identity_grants();
        let has =
            |capability: Capability| grants.iter().any(|grant| grant.capability == capability);
        assert!(has(Capability::MintCertificate));
        assert!(has(Capability::Storage));
        assert!(has(Capability::SharedMemory));
        assert!(has(Capability::HostQueue));
        assert!(has(Capability::RouteClaim));
    }
}

// --- Spec traceability (scripts/check-traceability.sh) -------------------
// These tests are the evidence for the `selium-identity` scenarios. One scenario may
// be claimed by several tests; the annotation lives next to the evidence.
