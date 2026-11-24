//! Operator test guest for the identity golden-path and power-cycle tests.
//!
//! Drives the platform's operator surfaces exactly as a control guest will:
//! resolves `sel:///identity`, onboards the `acme` tenant's CA, issues user
//! leaves from a test-supplied client SPKI, records that principal's baseline
//! grants, and writes issued leaves back to a blob store where the host-side
//! test reads them; and drives the control plane's `sel:///control` surface to
//! upload a module and record a deployment, then read it back.
//!
//! The test supplies the client SPKI as a pointer entrypoint argument and the
//! mode as an integer entrypoint argument, so later invocations of the same
//! guest drive the other legs (revocation, a post-restart issue, a post-restart
//! deployment check) against state the first lifetime left behind.

use anyhow::{Context as _, bail};
use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_guest::{
    Channel, Context, FramedRead, LiveTableView, ResourceSender, Subscriber, entrypoint, error,
    info,
};
use selium_service::{
    AccountantControl, AccountantControlResponse, ControlRequest, ControlResponse, IdentityRequest,
    IdentityResponse,
};
use selium_shm::{rpc, transport::ShmTransport};
use sha2::{Digest, Sha256};

/// The served accountant control route.
const ACCOUNTANT_ROUTE: &str = "sel:///accountant";
/// The served control-plane route.
const CONTROL_ROUTE: &str = "sel:///control";
/// The served identity route.
const IDENTITY_ROUTE: &str = "sel:///identity";
const LEAF_MANIFEST: &str = "acme-leaf";
const GRANT_FINGERPRINT: [u8; 32] = [0xAA; 32];
/// Manifest the post-restart leaf is published under.
const LEAF_MANIFEST_AFTER_RESTART: &str = "acme-leaf-after-restart";
/// Entry mode: upload a module and record a deployment through the control
/// plane, then log the outcome for the host-side test.
pub const MODE_DEPLOY: u64 = 3;
/// Entry mode: issue a further user leaf for an already-onboarded tenant,
/// proving the tenant CA recorded before a restart still signs leaves after it.
pub const MODE_ISSUE: u64 = 2;
/// Entry mode: onboard the tenant, issue a user leaf, record grants.
pub const MODE_ONBOARD: u64 = 0;
/// Entry mode: revoke the tenant (removes its anchor, deletes its CA key).
pub const MODE_REVOKE: u64 = 1;
/// Entry mode: author a process-count ceiling through the accountant's control
/// surface, so the durable ledger carries a policy record a restart must
/// replay and re-author.
pub const MODE_SET_QUOTA: u64 = 5;
/// Entry modes: record the pointer-supplied domain under acme or beta.
pub const MODE_SET_DOMAIN_ACME: u64 = 6;
pub const MODE_SET_DOMAIN_BETA: u64 = 7;
/// Entry mode: deploy a distinct replica count for interruption testing.
pub const MODE_DEPLOY_NINE: u64 = 9;
/// Entry modes: replace and verify a principal's opaque baseline grant bytes.
pub const MODE_SET_GRANTS_ONE: u64 = 10;
pub const MODE_SET_GRANTS_TWO: u64 = 11;
pub const MODE_VERIFY_GRANTS: u64 = 12;
/// Entry mode: rotate the already-onboarded tenant's CA.
pub const MODE_ROTATE: u64 = 13;
/// Entry mode: read a workload's recorded deployment back through the control
/// plane and log it, so a restart's survival is observable host-side.
pub const MODE_VERIFY_DEPLOYMENT: u64 = 4;
/// Module bytes the deploy leg uploads.
const MODULE_BYTES: &[u8] = b"selium power-cycle module";
/// Module the deploy leg uploads.
const MODULE_MANIFEST: &str = "power-cycle/module";
/// Blob store + manifest the issued leaf is written to, for host-side pickup.
const OUTPUT_STORE: &str = "selium.identity-onboard.out";
/// Process-count ceiling the quota leg authors.
pub const QUOTA_PROCESSES: u64 = 7;
/// How long a boot-time route lookup converges before failing. Owners publish
/// their own routes from their entrypoints, so a peer that boots alongside them
/// must retry rather than treat an early miss as permanent.
const ROUTE_READY_BOUND: std::time::Duration = std::time::Duration::from_secs(30);
/// The tenant this driver onboards and revokes.
const TENANT: &str = "acme";
/// Workload the deploy leg records.
const WORKLOAD_ID: &str = "power-cycle-worker";
/// Replica count the deploy leg records.
const WORKLOAD_REPLICAS: u32 = 2;

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

/// Resolves the control plane's serving route and connects to its surface.
async fn control_client(
    ctx: &mut Context,
) -> anyhow::Result<rpc::OwnedRpcClient<ControlRequest, ControlResponse>> {
    let target = ctx
        .resolve_ready(CONTROL_ROUTE, ROUTE_READY_BOUND)
        .await
        .with_context(|| "identity-onboard: control route resolve failed")?;
    let sender = ResourceSender::attach(target.resource_id)
        .with_context(|| "identity-onboard: control listener attach failed")?;
    rpc::connect::<ControlRequest, ControlResponse, _>(sender, 4096, 4096)
        .await
        .with_context(|| "identity-onboard: control rpc connect failed")
}

/// Resolves the identity guest's serving route and connects to its tiered
/// request surface.
async fn identity_client(
    ctx: &mut Context,
) -> anyhow::Result<rpc::OwnedRpcClient<IdentityRequest, IdentityResponse>> {
    let target = ctx
        .resolve_ready(IDENTITY_ROUTE, ROUTE_READY_BOUND)
        .await
        .with_context(|| "identity-onboard: identity route resolve failed")?;
    let sender = ResourceSender::attach(target.resource_id)
        .with_context(|| "identity-onboard: identity listener attach failed")?;
    rpc::connect::<IdentityRequest, IdentityResponse, _>(sender, 4096, 4096)
        .await
        .with_context(|| "identity-onboard: identity rpc connect failed")
}

/// Issues a further leaf for an already-onboarded tenant and publishes it under
/// a distinct manifest, leaving the recorded tenant CA untouched. A failure
/// here means either the tenant CA or the tenant's durable record did not
/// survive the restart.
async fn issue_leaf_after_restart(
    ctx: &mut Context,
    client: &mut rpc::OwnedRpcClient<IdentityRequest, IdentityResponse>,
    spki_der: Vec<u8>,
) -> anyhow::Result<()> {
    let issued = client
        .request(IdentityRequest::IssueUserCert {
            tenant: TENANT.to_string(),
            spki_der,
        })
        .await
        .with_context(|| "identity-onboard: post-restart issue failed")?;
    let IdentityResponse::UserCertIssued { certificate_der } = issued else {
        bail!("identity-onboard: unexpected issue response: {issued:?}");
    };

    let blobs = ctx
        .open_blob_store(OUTPUT_STORE)
        .await
        .with_context(|| "identity-onboard: output blob store open failed")?;
    let blob_id = blobs
        .put(certificate_der)
        .await
        .with_context(|| "identity-onboard: post-restart leaf put failed")?;
    blobs
        .set_manifest(LEAF_MANIFEST_AFTER_RESTART, blob_id)
        .await
        .with_context(|| "identity-onboard: post-restart leaf manifest failed")?;
    info!("identity-onboard: issued a further {TENANT} certificate after the restart");
    Ok(())
}

/// Onboard `acme`, mint a user certificate from the supplied client SPKI, and
/// record its baseline grants for the bridge-server to confer.
#[entrypoint]
async fn onboard(mut ctx: Context, spki: (u64, u64), mode: u64) -> anyhow::Result<()> {
    drop(selium_guest::log::init());
    info!("identity-onboard: started");

    let result = run_mode(&mut ctx, spki, mode).await;
    if let Err(error) = &result {
        error!("identity-onboard: mode {mode} failed: {error:#}");
    }
    result
}

/// Onboards the tenant, issues a user leaf from the client SPKI, and records
/// the principal's baseline grants.
async fn onboard_acme(
    ctx: &mut Context,
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
    let blobs = ctx
        .open_blob_store(OUTPUT_STORE)
        .await
        .with_context(|| "identity-onboard: output blob store open failed")?;
    let blob_id = blobs
        .put(certificate_der)
        .await
        .with_context(|| "identity-onboard: leaf blob put failed")?;
    blobs
        .set_manifest(LEAF_MANIFEST, blob_id)
        .await
        .with_context(|| "identity-onboard: leaf manifest failed")?;

    Ok(())
}

/// Runs one operator leg. Only the identity-facing legs connect to the identity
/// guest; deploy, verify, and quota talk to the control plane and accountant, so
/// they must not depend on identity being resolvable.
async fn run_mode(ctx: &mut Context, spki: (u64, u64), mode: u64) -> anyhow::Result<()> {
    match mode {
        MODE_ONBOARD => {
            let mut client = identity_client(ctx).await?;
            // SAFETY: the runtime wrote the client SPKI into this guest's
            // linear memory for this entrypoint invocation.
            let spki_der = unsafe { selium_guest::args::bytes(spki.0, spki.1) }.to_vec();
            onboard_acme(ctx, &mut client, spki_der).await?;
            info!("identity-onboard: {TENANT} onboarded and client certificate issued");
        }
        MODE_ISSUE => {
            let mut client = identity_client(ctx).await?;
            // SAFETY: the runtime wrote the client SPKI into this guest's
            // linear memory for this entrypoint invocation.
            let spki_der = unsafe { selium_guest::args::bytes(spki.0, spki.1) }.to_vec();
            issue_leaf_after_restart(ctx, &mut client, spki_der).await?;
        }
        MODE_DEPLOY => upload_and_deploy(ctx).await?,
        MODE_VERIFY_DEPLOYMENT => verify_deployment(ctx).await?,
        MODE_SET_QUOTA => set_process_quota(ctx).await?,
        MODE_SET_DOMAIN_ACME => set_domain(ctx, spki, TENANT).await?,
        MODE_SET_DOMAIN_BETA => set_domain(ctx, spki, "beta").await?,
        MODE_DEPLOY_NINE => deploy_nine(ctx).await?,
        MODE_SET_GRANTS_ONE => set_grants(ctx, 1).await?,
        MODE_SET_GRANTS_TWO => set_grants(ctx, 2).await?,
        MODE_VERIFY_GRANTS => verify_grants(ctx).await?,
        MODE_ROTATE => rotate_acme(ctx).await?,
        MODE_REVOKE => {
            let mut client = identity_client(ctx).await?;
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

    Ok(())
}

/// Rotates the already-onboarded tenant through the real identity RPC.
async fn rotate_acme(ctx: &mut Context) -> anyhow::Result<()> {
    let mut client = identity_client(ctx).await?;
    let response = client
        .request(IdentityRequest::RotateTenantCa {
            tenant: TENANT.to_string(),
        })
        .await
        .with_context(|| "identity-onboard: rotate tenant CA failed")?;
    let IdentityResponse::Rotated { .. } = response else {
        bail!("identity-onboard: unexpected rotate response: {response:?}");
    };
    info!("identity-onboard: {TENANT} tenant CA rotated");
    Ok(())
}

/// Replaces the test principal's baseline grant bytes through identity.
async fn set_grants(ctx: &mut Context, value: u8) -> anyhow::Result<()> {
    let mut client = identity_client(ctx).await?;
    let response = client
        .request(IdentityRequest::SetPrincipalGrants {
            tenant: TENANT.to_string(),
            fingerprint: GRANT_FINGERPRINT.to_vec(),
            grants: vec![value],
        })
        .await
        .with_context(|| "identity-onboard: set grants failed")?;
    let IdentityResponse::PrincipalRecorded { .. } = response else {
        bail!("identity-onboard: unexpected grants response: {response:?}");
    };
    info!("identity-onboard: principal grants [{value}] recorded");
    Ok(())
}

/// Reads the identity-published grant table after a component restart.
async fn verify_grants(ctx: &mut Context) -> anyhow::Result<()> {
    let target = ctx
        .resolve_ready("sel:///identity-grants", ROUTE_READY_BOUND)
        .await
        .with_context(|| "identity-onboard: grant table resolve failed")?;
    let channel = Channel::attach(target.resource_id)
        .with_context(|| "identity-onboard: grant table attach failed")?;
    let transport = ShmTransport::new_replay(&channel, &channel)
        .with_context(|| "identity-onboard: grant table transport failed")?;
    let subscriber = Subscriber::new(FramedRead::new(transport), None);
    let view: LiveTableView<Vec<u8>, Vec<u8>, ShmTransport> = LiveTableView::new(subscriber)
        .with_context(|| "identity-onboard: grant table view failed")?;
    view.sync()
        .with_context(|| "identity-onboard: grant table sync failed")?;
    let grants = view
        .get(&GRANT_FINGERPRINT.to_vec())
        .with_context(|| "identity-onboard: grant table read failed")?;
    info!("identity-onboard: principal grants {grants:?}");
    Ok(())
}

/// Records a pointer-supplied domain through the identity request surface.
async fn set_domain(ctx: &mut Context, domain: (u64, u64), tenant: &str) -> anyhow::Result<()> {
    // SAFETY: the runtime wrote the domain into this guest's linear memory for
    // this entrypoint invocation; identity-facing modes never treat it as SPKI.
    let domain = unsafe { selium_guest::args::str(domain.0, domain.1) }
        .context("identity-onboard: invalid domain argument")?
        .to_string();
    let mut client = identity_client(ctx).await?;
    let response = client
        .request(IdentityRequest::SetDomain {
            domain: domain.clone(),
            tenant: tenant.to_string(),
        })
        .await
        .with_context(|| "identity-onboard: set domain failed")?;
    let IdentityResponse::DomainRecorded { .. } = response else {
        bail!("identity-onboard: unexpected domain response: {response:?}");
    };
    info!("identity-onboard: domain {domain}={tenant} recorded");
    Ok(())
}

/// Records a deployment with a distinct replica count so a failed replacement
/// cannot be mistaken for the previously acknowledged deployment on replay.
async fn deploy_nine(ctx: &mut Context) -> anyhow::Result<()> {
    let mut client = control_client(ctx).await?;
    let response = client
        .request(ControlRequest::Deploy {
            workload_id: WORKLOAD_ID.to_string(),
            replicas: 9,
            module: "interrupted/module".to_string(),
        })
        .await
        .with_context(|| "identity-onboard: interrupted deploy failed")?;
    let ControlResponse::Accepted { replicas: 9, .. } = response else {
        bail!("identity-onboard: unexpected interrupted deploy response: {response:?}");
    };
    info!("power-cycle: deployed {WORKLOAD_ID} replicas=9 module=interrupted/module");
    Ok(())
}

/// Authors a process-count ceiling through the accountant's control surface.
///
/// The accountant appends a policy record to its durable ledger and re-authors
/// enforcement, so a restart that replays the ledger must re-author the same
/// ceiling.
async fn set_process_quota(ctx: &mut Context) -> anyhow::Result<()> {
    let target = ctx
        .resolve_ready(ACCOUNTANT_ROUTE, ROUTE_READY_BOUND)
        .await
        .with_context(|| "identity-onboard: accountant route resolve failed")?;
    let sender = ResourceSender::attach(target.resource_id)
        .with_context(|| "identity-onboard: accountant listener attach failed")?;
    let mut client =
        rpc::connect::<AccountantControl, AccountantControlResponse, _>(sender, 4096, 4096)
            .await
            .with_context(|| "identity-onboard: accountant rpc connect failed")?;

    let response = client
        .request(AccountantControl::SetProcessQuota {
            tenant: TENANT.to_string(),
            processes: QUOTA_PROCESSES,
        })
        .await
        .with_context(|| "identity-onboard: set process quota failed")?;
    let AccountantControlResponse::Updated { tenant } = response else {
        bail!("identity-onboard: unexpected quota response: {response:?}");
    };
    info!("power-cycle: authored {tenant} process ceiling {QUOTA_PROCESSES}");
    Ok(())
}

/// Uploads a module through the control plane and records a deployment that
/// references it, logging each outcome for the host-side test to assert.
async fn upload_and_deploy(ctx: &mut Context) -> anyhow::Result<()> {
    let mut client = control_client(ctx).await?;

    let uploaded = client
        .request(ControlRequest::Upload {
            manifest: MODULE_MANIFEST.to_string(),
            bytes: MODULE_BYTES.to_vec(),
        })
        .await
        .with_context(|| "identity-onboard: module upload failed")?;
    let ControlResponse::Uploaded { manifest } = uploaded else {
        bail!("identity-onboard: unexpected upload response: {uploaded:?}");
    };
    info!("power-cycle: uploaded module {manifest}");

    let deployed = client
        .request(ControlRequest::Deploy {
            workload_id: WORKLOAD_ID.to_string(),
            replicas: WORKLOAD_REPLICAS,
            module: MODULE_MANIFEST.to_string(),
        })
        .await
        .with_context(|| "identity-onboard: deploy failed")?;
    let ControlResponse::Accepted {
        workload_id,
        replicas,
        module,
        ..
    } = deployed
    else {
        bail!("identity-onboard: unexpected deploy response: {deployed:?}");
    };
    info!("power-cycle: deployed {workload_id} replicas={replicas} module={module}");
    Ok(())
}

/// Reads a workload's recorded deployment back through the control plane,
/// logging it so a restart's survival is observable host-side.
async fn verify_deployment(ctx: &mut Context) -> anyhow::Result<()> {
    let mut client = control_client(ctx).await?;
    let status = client
        .request(ControlRequest::Status {
            workload_id: WORKLOAD_ID.to_string(),
        })
        .await
        .with_context(|| "identity-onboard: status failed")?;
    let ControlResponse::Status { deployment } = status else {
        bail!("identity-onboard: unexpected status response: {status:?}");
    };
    match deployment {
        Some(deployment) => info!(
            "power-cycle: deployment {workload} replicas={replicas} module={module}",
            workload = deployment.workload_id,
            replicas = deployment.replicas,
            module = deployment.module
        ),
        None => info!("power-cycle: deployment {WORKLOAD_ID} absent"),
    }
    Ok(())
}
