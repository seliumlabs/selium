//! Selium identity system guest — the platform's PKI policy authority.
//!
//! The identity guest owns tenant and user-certificate **policy**: who mints
//! what, when to rotate or revoke, and which principals hold which baseline
//! grants. It mints certificates through the host-held signing oracle
//! (`SignTenantCa` / `SignUserCert` / `RevokeCa`, gated by `MintCertificate`)
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
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use anyhow::Context as _;
use rkyv::{Archive, Deserialize, Serialize};
use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceKind, ResourceSelector};
use selium_guest::{
    Context, DurableLog, ResourceListener, Serve, entrypoint, info, mark_ready, spawn, warn,
};
use selium_service::{FlatMsg, IdentityRequest, IdentityResponse, ResourceTarget};
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
/// Serving route path for the grant live table (`sel:///identity-grants`).
pub const GRANT_TABLE_PATH: &str = "identity-grants";
/// Serving route path for the request surface (`sel:///identity`).
pub const IDENTITY_PATH: &str = "identity";
/// Durable log name for the principal registry.
pub const PRINCIPAL_LOG: &str = "selium.identity.principals";
/// Ring capacity for each live table.
const TABLE_CAPACITY: u64 = 64 * 1024;
/// Durable log name for the tenant registry.
pub const TENANT_LOG: &str = "selium.identity.tenants";

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
}

/// The replayed principal projection: `fingerprint -> baseline grants bytes`.
#[derive(Debug, Clone, Default)]
pub struct PrincipalRegistry {
    by_fingerprint: BTreeMap<Vec<u8>, Vec<u8>>,
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
    tenants: Arc<Mutex<TenantRegistry>>,
    principals: Arc<Mutex<PrincipalRegistry>>,
    tenant_log: DurableLog,
    principal_log: DurableLog,
    anchors: Arc<LiveTable<String, Vec<u8>, ShmTransport>>,
    grants: Arc<LiveTable<Vec<u8>, Vec<u8>, ShmTransport>>,
}

impl TenantRegistry {
    /// Applies one tenant record in replay order.
    pub fn apply_record(&mut self, record: TenantRecord) {
        match record {
            TenantRecord::Onboard {
                tenant,
                ca_cert_der,
            } => {
                self.by_tenant.insert(tenant, ca_cert_der);
            }
            TenantRecord::Revoke { tenant } => {
                self.by_tenant.remove(&tenant);
            }
        }
    }

    /// Materialises the projection from a replayed durable log.
    pub fn rebuild(&mut self, log: &DurableLog) -> selium_guest::Result<()> {
        let payloads = log
            .replay(None, u32::MAX)?
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
    pub fn rebuild(&mut self, log: &DurableLog) -> selium_guest::Result<()> {
        let payloads = log
            .replay(None, u32::MAX)?
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
/// two registries), shared memory (live-table rings and RPC session rings),
/// host queues (the serving listener plus the discovery client), and
/// root-namespace registration.
pub fn identity_grants() -> Vec<CapabilityGrant> {
    vec![
        CapabilityGrant::new(Capability::MintCertificate, Vec::new()),
        CapabilityGrant::new(
            Capability::Storage,
            vec![ResourceSelector::ResourceClass(ResourceClass::DurableLog)],
        ),
        CapabilityGrant::new(
            Capability::SharedMemory,
            vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
        ),
        CapabilityGrant::new(
            Capability::HostQueue,
            vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
        ),
        CapabilityGrant::new(Capability::SystemRegistration, Vec::new()),
    ]
}

/// Builds one live table over its own `LiveTable` ring, returning the ring id
/// and the table.
fn create_live_table<K, V>(
    capacity: u64,
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
    let table = LiveTable::new(publisher, subscriber)?;
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
                    Ok(payload) => handle_request(payload, &tier, &state),
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

/// Handles one decoded request, enforcing the caller's tier.
fn handle_request(
    request: IdentityRequest,
    tier: &Tier,
    state: &IdentityState,
) -> IdentityResponse {
    match request {
        IdentityRequest::MintTenantCa { tenant } => {
            if !tier.is_operator() {
                return tier_refused("mint a tenant CA");
            }
            match mint_tenant_ca(&tenant, state) {
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
            match mint_tenant_ca(&tenant, state) {
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
            match revoke_tenant(&tenant, state) {
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
            match issue_user_cert(&tenant, &spki_der, state) {
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
            set_principal_grants(&tenant, &fingerprint, &grants, state);
            IdentityResponse::PrincipalRecorded { fingerprint }
        }
        IdentityRequest::RemovePrincipal {
            tenant,
            fingerprint,
        } => {
            if !tier.admits_tenant(&tenant) {
                return tier_refused("manage principals");
            }
            remove_principal(&tenant, &fingerprint, state);
            IdentityResponse::PrincipalRemoved { fingerprint }
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

    let tenant_log =
        DurableLog::open(TENANT_LOG).with_context(|| "identity: tenant log open failed")?;
    let principal_log =
        DurableLog::open(PRINCIPAL_LOG).with_context(|| "identity: principal log open failed")?;

    let tenants = Arc::new(Mutex::new(TenantRegistry::default()));
    tenants
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .rebuild(&tenant_log)
        .with_context(|| "identity: tenant registry replay failed")?;
    let principals = Arc::new(Mutex::new(PrincipalRegistry::default()));
    principals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .rebuild(&principal_log)
        .with_context(|| "identity: principal registry replay failed")?;

    let (anchor_region, anchors) = create_live_table::<String, Vec<u8>>(TABLE_CAPACITY)
        .map_err(|e| anyhow::anyhow!("identity: anchor table create failed: {e}"))?;
    let (grant_region, grants) = create_live_table::<Vec<u8>, Vec<u8>>(TABLE_CAPACITY)
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
        tenants,
        principals,
        tenant_log,
        principal_log,
        anchors,
        grants,
    });

    mark_ready();

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

/// Issues a short-TTL leaf certificate from a client SPKI, recording the
/// principal's fingerprint in the registry.
fn issue_user_cert(
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
    record_principal(tenant, &fingerprint, &grants, state);
    Ok(certificate_der)
}

/// Mints (or rotates) a tenant CA, recording it in the registry and publishing
/// its trust anchor.
fn mint_tenant_ca(tenant: &str, state: &IdentityState) -> Result<(), String> {
    let ca_cert_der = selium_guest::sign_tenant_ca(tenant)
        .map_err(|error| format!("tenant {tenant} CA mint failed: {error}"))?;

    let timestamp_ms = selium_guest::time::now()
        .map(|nanos| nanos / 1_000_000)
        .unwrap_or_default();
    let payload = selium_abi::encode_rkyv(&TenantRecord::Onboard {
        tenant: tenant.to_string(),
        ca_cert_der: ca_cert_der.clone(),
    })
    .map_err(|error| format!("tenant record encode failed: {error}"))?;
    state
        .tenant_log
        .append(timestamp_ms, Vec::new(), payload)
        .map_err(|error| format!("tenant record append failed: {error}"))?;

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
        .map_err(|error| format!("anchor publish failed: {error}"))?;
    Ok(())
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

/// Appends a principal record and applies it to the projection.
fn record_principal(tenant: &str, fingerprint: &[u8], grants: &[u8], state: &IdentityState) {
    let timestamp_ms = selium_guest::time::now()
        .map(|nanos| nanos / 1_000_000)
        .unwrap_or_default();
    let record = PrincipalRecord::Set {
        tenant: tenant.to_string(),
        fingerprint: fingerprint.to_vec(),
        grants: grants.to_vec(),
    };
    match selium_abi::encode_rkyv(&record) {
        Ok(payload) => {
            if let Err(error) = state
                .principal_log
                .append(timestamp_ms, Vec::new(), payload)
            {
                warn!("identity: principal record append failed: {error}");
            }
        }
        Err(error) => warn!("identity: principal record encode failed: {error}"),
    }
    state
        .principals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .apply_record(record);
}

/// Removes a principal's baseline grants.
fn remove_principal(tenant: &str, fingerprint: &[u8], state: &IdentityState) {
    let timestamp_ms = selium_guest::time::now()
        .map(|nanos| nanos / 1_000_000)
        .unwrap_or_default();
    match selium_abi::encode_rkyv(&PrincipalRecord::Remove {
        tenant: tenant.to_string(),
        fingerprint: fingerprint.to_vec(),
    }) {
        Ok(payload) => {
            if let Err(error) = state
                .principal_log
                .append(timestamp_ms, Vec::new(), payload)
            {
                warn!("identity: principal remove append failed: {error}");
            }
        }
        Err(error) => warn!("identity: principal remove encode failed: {error}"),
    }
    state
        .principals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .apply_record(PrincipalRecord::Remove {
            tenant: tenant.to_string(),
            fingerprint: fingerprint.to_vec(),
        });
    if let Err(error) = state.grants.delete(fingerprint.to_vec()) {
        warn!("identity: grant removal failed: {error}");
    }
}

/// Revokes a tenant CA: deletes its key from the host keyring, records the
/// revocation, and removes the anchor.
///
/// The keyring deletion is attempted first and its failure aborts the
/// revocation (no registry or anchor change): the response must not claim a
/// revocation whose host-side key deletion failed. The enforcement
/// mechanisms (anchor removal, registry removal) are applied only after the
/// key is confirmed deleted.
fn revoke_tenant(tenant: &str, state: &IdentityState) -> Result<(), String> {
    selium_guest::revoke_ca(tenant)
        .map_err(|error| format!("tenant {tenant} CA key deletion failed: {error}"))?;

    let timestamp_ms = selium_guest::time::now()
        .map(|nanos| nanos / 1_000_000)
        .unwrap_or_default();
    let payload = selium_abi::encode_rkyv(&TenantRecord::Revoke {
        tenant: tenant.to_string(),
    })
    .map_err(|error| format!("tenant revoke encode failed: {error}"))?;
    state
        .tenant_log
        .append(timestamp_ms, Vec::new(), payload)
        .map_err(|error| format!("tenant revoke append failed: {error}"))?;

    state
        .tenants
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .apply_record(TenantRecord::Revoke {
            tenant: tenant.to_string(),
        });
    state
        .anchors
        .delete(anchor_key(tenant))
        .map_err(|error| format!("anchor removal failed: {error}"))?;
    Ok(())
}

/// Records a principal's baseline grant set and publishes it to the grant table.
fn set_principal_grants(tenant: &str, fingerprint: &[u8], grants: &[u8], state: &IdentityState) {
    record_principal(tenant, fingerprint, grants, state);
    if let Err(error) = publish_grants(&state.grants, fingerprint, grants) {
        warn!("identity: grant publish failed: {error}");
    }
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

    #[test]
    fn operator_tier_admits_everything() {
        let tier = Tier::Operator;
        assert!(tier.is_operator());
        assert!(tier.admits_tenant("acme"));
        assert!(tier.admits_tenant("beta"));
    }

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
        assert!(has(Capability::SystemRegistration));
    }
}
