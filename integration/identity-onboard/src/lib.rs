//! Onboarding operator test guest for the identity golden-path test.
//!
//! Drives the identity guest's operator and tenant tiers exactly as a
//! platform control guest (external-api, control) will: resolves
//! `sel:///identity`, onboards the `acme` tenant's CA, issues a user leaf from
//! a test-supplied client SPKI, records that principal's baseline grants, and
//! writes the issued leaf back to a blob store where the host-side test reads
//! it for the mTLS client. The test supplies the client SPKI as a pointer
//! entrypoint argument and the mode (`0` = onboard, `1` = revoke tenant) as an
//! integer entrypoint argument, so a second invocation of the same guest can
//! drive the revocation leg of the golden path.

use anyhow::{Context as _, bail};
use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_guest::{BlobStore, Context, ResourceSender, entrypoint, info, mark_ready};
use selium_service::{IdentityRequest, IdentityResponse};
use selium_shm::rpc;
use sha2::{Digest, Sha256};

/// The served identity route.
const IDENTITY_ROUTE: &str = "sel:///identity";
const LEAF_MANIFEST: &str = "acme-leaf";
/// Entry mode: onboard the tenant, issue a user leaf, record grants.
pub const MODE_ONBOARD: u64 = 0;
/// Entry mode: revoke the tenant (removes its anchor, deletes its CA key).
pub const MODE_REVOKE: u64 = 1;
/// Blob store + manifest the issued leaf is written to, for host-side pickup.
const OUTPUT_STORE: &str = "selium.identity-onboard.out";
/// The tenant this driver onboards and revokes.
const TENANT: &str = "acme";

/// The baseline data-plane grants conferred on the onboarded principals:
/// tenant-scoped shared memory, host queues, and network streams.
fn acme_client_grants() -> Vec<CapabilityGrant> {
    vec![
        CapabilityGrant::new(
            Capability::SharedMemory,
            vec![
                ResourceSelector::Tenant(TENANT.to_string()),
                ResourceSelector::ResourceClass(ResourceClass::SharedRegion),
            ],
        ),
        CapabilityGrant::new(
            Capability::HostQueue,
            vec![
                ResourceSelector::Tenant(TENANT.to_string()),
                ResourceSelector::ResourceClass(ResourceClass::HostQueue),
            ],
        ),
        CapabilityGrant::new(
            Capability::Network,
            vec![
                ResourceSelector::Tenant(TENANT.to_string()),
                ResourceSelector::ResourceClass(ResourceClass::TcpStream),
            ],
        ),
    ]
}

/// Resolves the identity guest's serving route and connects to its tiered
/// request surface.
async fn identity_client(
    ctx: &mut Context,
) -> anyhow::Result<rpc::OwnedRpcClient<IdentityRequest, IdentityResponse>> {
    let target = ctx
        .lookup(IDENTITY_ROUTE)
        .await
        .with_context(|| "identity-onboard: identity route resolve failed")?
        .ok_or_else(|| anyhow::anyhow!("identity-onboard: identity route not found"))?;
    let sender = ResourceSender::attach(target.resource_id)
        .with_context(|| "identity-onboard: identity listener attach failed")?;
    rpc::connect::<IdentityRequest, IdentityResponse, _>(sender, 4096, 4096)
        .await
        .with_context(|| "identity-onboard: identity rpc connect failed")
}

/// Onboard `acme`, mint a user certificate from the supplied client SPKI, and
/// record its baseline grants for the bridge-server to confer.
#[entrypoint]
async fn onboard(mut ctx: Context, spki: (u64, u64), mode: u64) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("identity-onboard: started");

    let mut client = identity_client(&mut ctx).await?;
    match mode {
        MODE_ONBOARD => {
            // SAFETY: the runtime wrote the client SPKI into this guest's
            // linear memory for this entrypoint invocation.
            let spki_der = unsafe { selium_guest::args::bytes(spki.0, spki.1) }.to_vec();
            onboard_acme(&mut client, spki_der).await?;
            info!("identity-onboard: {TENANT} onboarded and client certificate issued");
        }
        MODE_REVOKE => {
            let revoked = client
                .request(IdentityRequest::RevokeTenant {
                    tenant: TENANT.to_string(),
                })
                .await
                .with_context(|| "identity-onboard: revoke tenant failed")?;
            if !matches!(revoked, IdentityResponse::Revoked { .. }) {
                bail!("identity-onboard: unexpected revoke response: {revoked:?}");
            }
            info!("identity-onboard: {TENANT} tenant revoked");
        }
        other => bail!("identity-onboard: unknown entry mode {other}"),
    }

    mark_ready();
    Ok(())
}

/// Onboards the tenant, issues a user leaf from the client SPKI, and records
/// the principal's baseline grants.
async fn onboard_acme(
    client: &mut rpc::OwnedRpcClient<IdentityRequest, IdentityResponse>,
    spki_der: Vec<u8>,
) -> anyhow::Result<()> {
    let fingerprint = Sha256::digest(&spki_der).to_vec();

    let minted = client
        .request(IdentityRequest::MintTenantCa {
            tenant: TENANT.to_string(),
        })
        .await
        .with_context(|| "identity-onboard: mint tenant CA failed")?;
    if !matches!(minted, IdentityResponse::TenantRecorded { .. }) {
        bail!("identity-onboard: unexpected mint response: {minted:?}");
    }

    let issued = client
        .request(IdentityRequest::IssueUserCert {
            tenant: TENANT.to_string(),
            spki_der,
        })
        .await
        .with_context(|| "identity-onboard: issue user cert failed")?;
    let IdentityResponse::UserCertIssued { certificate_der } = issued else {
        bail!("identity-onboard: unexpected issue response: {issued:?}");
    };

    let grants = selium_abi::encode_rkyv(&acme_client_grants())
        .with_context(|| "identity-onboard: grant encode failed")?;
    let recorded = client
        .request(IdentityRequest::SetPrincipalGrants {
            tenant: TENANT.to_string(),
            fingerprint,
            grants,
        })
        .await
        .with_context(|| "identity-onboard: set principal grants failed")?;
    if !matches!(recorded, IdentityResponse::PrincipalRecorded { .. }) {
        bail!("identity-onboard: unexpected grant response: {recorded:?}");
    }

    // Publish the issued leaf for host-side pickup.
    let blobs = BlobStore::open(OUTPUT_STORE)
        .with_context(|| "identity-onboard: output blob store open failed")?;
    let blob_id = blobs
        .put(certificate_der)
        .with_context(|| "identity-onboard: leaf blob put failed")?;
    blobs
        .set_manifest(LEAF_MANIFEST, blob_id)
        .with_context(|| "identity-onboard: leaf manifest failed")?;

    Ok(())
}
