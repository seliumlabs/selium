//! QUIC edge connector system guest.
//!
//! Terminates external QUIC (TLS 1.3) at the edge with a quinn [`Endpoint`]
//! running over the guest's shared-memory [`UdpSocket`], and relays each
//! accepted bidirectional stream's bytes over per-stream shared-memory
//! channels, so application guests serve QUIC byte transport with **zero
//! `Network` grants** and no quinn dependency of their own.
//!
//! # Architecture
//!
//! - The [`QuicUdpSocket`](udp_adapter::QuicUdpSocket) adapter maps quinn
//!   datagram I/O onto the shm send/recv rings; the
//!   [`ConnectorRuntime`](runtime::ConnectorRuntime) gives quinn its executor
//!   and timers.
//! - TLS server material (certificate + key) is loaded from blob storage via
//!   the connector's `Storage` grant, failing loudly when missing or invalid.
//! - **Client authentication (mTLS) is opt-in and endpoint-global.** With no
//!   per-tenant trust anchors configured in the TLS blob store, the connector
//!   serves without client authentication. When anchors are configured, a
//!   client certificate verifying against the union of all anchors is required
//!   for *every* route it serves (there is no per-route or per-SNI client-auth
//!   policy today; introducing one is deployment nuance tracked as an open
//!   question, not current spec behaviour). Bridge routes must only be
//!   deployed on connectors with anchors configured: the bridge-server
//!   attributes authority from the identity the connector attaches to each
//!   handoff, so an mTLS-disabled connector must not serve bridge traffic.
//! - One quinn server endpoint accepts connections; the serving guest for each
//!   connection is resolved from the handshake SNI (a bare server name), and
//!   each accepted bidirectional stream is relayed over its own two-ring
//!   shared-memory channel (see [`pipeline`]). Under mTLS, each stream handoff
//!   carries the authenticated client [`identity`] as metadata; without mTLS,
//!   handoffs carry empty metadata.

use std::{net::SocketAddr, sync::Arc};

use anyhow::Context as _;
use quinn::ServerConfig;
use rustls_pemfile as pemfile;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use selium_guest::{
    Context, Instant, ResourceSender, UdpSocket, entrypoint, error, info, mark_ready, spawn, warn,
};
use selium_shm::{Channel, transport::ShmTransport};
use selium_wire::{LiveTableView, framed::FramedRead, pubsub::Subscriber};
use thiserror::Error;
// Feature-unification anchor, not a code dependency: pulls in `ring` (with its
// `wasm32_unknown_unknown_js` feature) so `SystemRandom` compiles on
// wasm32-unknown-unknown — the backend actually used is getrandom's `custom`
// (see `.cargo/config.toml`). Guards against cargo-shear removing the dep.
#[cfg(target_arch = "wasm32")]
use ring as _;

use crate::{
    identity::{ClientAnchorSet, build_server_config},
    pipeline::relay_stream,
    rate::{
        AdmissionLimiter, DEFAULT_ADMISSION_BURST, DEFAULT_ADMISSION_TOKENS_PER_SEC, refuse_stream,
    },
    resolve::{ResolveError, ResolverHandle, RouteResolver},
    runtime::ConnectorRuntime,
    udp_adapter::QuicUdpSocket,
};

pub mod identity;
pub mod pipeline;
pub mod rate;
pub mod resolve;
pub mod runtime;
pub mod stream;
pub mod udp_adapter;

/// QUIC error code used to reset an admitted-refused stream: distinct from the
/// handshake refusal code so a peer can tell "stream refused" from "connection
/// refused" while the connection itself stays up.
pub const ADMISSION_REFUSED_ERROR_CODE: u32 = 0x1_0100;
/// Anchor-table key prefix: keys are `client-ca-<tenant>`.
const ANCHOR_KEY_PREFIX: &str = "client-ca-";
/// The identity guest's published anchor live-table route.
const ANCHOR_TABLE_ROUTE: &str = "sel:///identity-anchors";
/// Maximum concurrent bidirectional streams a single connection may open.
/// quinn enforces this natively via the advertised MAX_STREAMS, so streams
/// beyond the cap are refused before any per-stream region is allocated. The
/// exact value is operator configuration; this is the deployable default.
pub const MAX_CONCURRENT_BIDI_STREAMS: u32 = 128;
/// Default listener address for the QUIC connector.
///
/// Deferred policy: recorded in the connector's config, not spec behaviour
/// (see design open questions).
const QUIC_LISTEN_ADDR: &str = "0.0.0.0:4433";
/// QUIC close code used for handshake refusal (crypto error, unspecified).
const REFUSE_ERROR_CODE: u32 = 0x100;
/// Manifest name for the certificate chain PEM.
const TLS_CERT_MANIFEST: &str = "cert-pem";
/// Manifest name for the private key PEM.
const TLS_KEY_MANIFEST: &str = "key-pem";
/// Storage blob store name for TLS material.
const TLS_STORE_NAME: &str = "tls-certs";

#[derive(Debug, Error)]
pub enum TlsError {
    #[error("TLS storage unavailable")]
    StorageUnavailable,
    #[error("TLS certificate not found")]
    MissingCertificate,
    #[error("TLS private key not found")]
    MissingKey,
    #[error("invalid TLS certificate")]
    InvalidCertificate,
    #[error("invalid TLS private key")]
    InvalidKey,
    #[error("no client trust anchors configured")]
    MissingClientAnchors,
    #[error("invalid client trust anchor")]
    InvalidClientAnchor,
    #[error("TLS configuration error")]
    ConfigError,
}

/// The outcome of trying to attach the identity guest's anchor live table.
enum AnchorTableSource {
    /// Identity is deployed and its anchor table attached: mTLS is mandatory.
    Attached(Box<LiveTableView<String, Vec<u8>, ShmTransport>>),
    /// No identity anchor-table route is registered: the deployment opted out
    /// of mTLS, so the connector serves without client authentication.
    NotDeployed,
    /// The route is registered but the table could not be attached: fail
    /// closed by refusing every client certificate.
    Unavailable,
}

/// Builds a quinn server endpoint from an abstract UDP socket and runtime.
///
/// This is the quinn-on-wasm32 seam: every endpoint in this crate is built via
/// [`quinn::Endpoint::new_with_abstract_socket`], with the connector supplying
/// both halves quinn needs (the shm datagram adapter and the guest runtime).
pub fn build_endpoint(
    socket: Arc<dyn quinn::AsyncUdpSocket>,
    runtime: Arc<dyn quinn::Runtime>,
    server_config: Option<ServerConfig>,
) -> std::io::Result<quinn::Endpoint> {
    quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        server_config,
        socket,
        runtime,
    )
}

/// Serves one QUIC connection: derive its authenticated client identity, resolve
/// its serving guest once from SNI, then relay every accepted bidirectional
/// stream to that guest over a per-stream byte channel, attaching the identity
/// as handoff metadata.
///
/// `anchors` is the **opt-in** mTLS policy: `Some(anchors)` requires every
/// client to present a certificate verifying against the configured trust
/// anchors and attaches the derived identity to each handoff; `None` serves
/// without client authentication and attaches empty metadata.
///
/// `limiter` rate-limits new stream admissions per tenant before a per-stream
/// channel is allocated; a stream over the rate is reset with a distinct error
/// code while the connection itself stays up.
///
/// Exposed for the connector's integration tests: the refusal path (unknown
/// or absent SNI, or missing/unverifiable client identity under mTLS)
/// closes the connection before any guest contact.
pub async fn handle_connection(
    connection: quinn::Connection,
    resolver: ResolverHandle,
    anchors: Option<ClientAnchorSet>,
    limiter: AdmissionLimiter,
) {
    // Route from the handshake SNI. Unknown/absent SNI refuses the connection
    // without ever contacting an app guest.
    let Some(server_name) = sni_of(&connection) else {
        warn!("quic-connector: refusing connection with no SNI");
        connection.close(REFUSE_ERROR_CODE.into(), b"no server name");
        return;
    };

    let target = match resolver.lock().await.resolve(&server_name).await {
        Ok(target) => target,
        Err(ResolveError::NotFound) => {
            warn!("quic-connector: refusing connection: no route for {server_name}");
            connection.close(REFUSE_ERROR_CODE.into(), b"unknown server name");
            return;
        }
    };

    // Authenticated identity, attached to each stream handoff so the serving
    // guest can attribute authority (the bridge-server maps it to grants).
    // Without configured anchors, mTLS is off and handoffs carry empty
    // metadata.
    let (identity_metadata, client_tenant) = match &anchors {
        Some(anchors) => match anchors.identity_for(&connection) {
            Some(identity) => (identity.encode(), Some(identity.tenant)),
            None => {
                warn!("quic-connector: refusing connection: unverifiable client identity");
                connection.close(REFUSE_ERROR_CODE.into(), b"untrusted client certificate");
                return;
            }
        },
        None => (Vec::new(), None),
    };

    // Deliver every accepted stream over its own byte channel.
    let sender = match ResourceSender::attach(target.resource_id) {
        Ok(sender) => sender,
        Err(e) => {
            warn!("quic-connector: attach to guest queue failed: {e}");
            return;
        }
    };

    // Admission key: the authenticated client's tenant, falling back to the
    // resolved serving tenant when client authentication is disabled. A
    // root/tenant-less route admits under the platform (empty) key.
    let rate_key = client_tenant
        .clone()
        .or_else(|| target.tenant.clone())
        .unwrap_or_default();

    loop {
        let (send, recv) = match connection.accept_bi().await {
            Ok(streams) => streams,
            Err(e) => {
                warn!("quic-connector: accept_bi failed: {e}");
                break;
            }
        };

        // Refuse streams over the per-tenant admission rate before allocating
        // a per-stream region: reset the stream with a distinct error code,
        // keeping the connection up for conforming streams.
        let now = match Instant::now() {
            Ok(now) => now,
            Err(error) => {
                warn!("quic-connector: clock unavailable for admission: {error}");
                refuse_stream(send, recv);
                continue;
            }
        };
        if !limiter.allow(&rate_key, now).await {
            warn!(
                tenant = %rate_key,
                "quic-connector: stream admission rate limit exceeded"
            );
            refuse_stream(send, recv);
            continue;
        }

        let channel =
            match crate::stream::QuicChannel::allocate_for_tenant(client_tenant.as_deref()) {
                Ok(channel) => channel,
                Err(e) => {
                    warn!("quic-connector: stream channel allocation failed: {e}");
                    continue;
                }
            };

        if let Err(e) = sender
            .send_with_metadata(channel.shared_id(), identity_metadata.clone())
            .await
        {
            // Stale route: evict so the next connection re-resolves.
            warn!("quic-connector: stream delivery failed: {e}");
            resolver.lock().await.evict(&server_name);
            continue;
        }

        let (guest_reader, guest_writer) = channel.into_halves();
        spawn(relay_stream(recv, send, guest_reader, guest_writer));
    }
}

/// Registers the wasm32 time source backing `web_time::Instant`, forwarding to
/// the hostcall monotonic and wall clocks.
///
/// Must run before any TLS/quinn operation on wasm32.
#[cfg(target_arch = "wasm32")]
pub fn register_wasm_time_source() {
    web_time::set_custom_time_source(web_time::TimeSource {
        monotonic_ns: || {
            selium_guest::time::Instant::now()
                .expect("TimeMonotonic hostcall")
                .as_nanos()
        },
        wall_clock_ns: || selium_guest::time::now().expect("TimeNow hostcall"),
    });
}

/// Extracts the rustls server name (SNI) from an established connection.
pub fn sni_of(connection: &quinn::Connection) -> Option<String> {
    let data = connection.handshake_data()?;
    let handshake = data
        .downcast::<quinn::crypto::rustls::HandshakeData>()
        .ok()?;
    handshake.server_name
}

/// Custom `getrandom` backend for wasm32, invoked by the `getrandom` crate
/// when built with the `custom` backend (`getrandom_backend = "custom"`).
///
/// # Safety
/// The contract is defined by `getrandom`: `dest` must be valid for writes of
/// `len` bytes, and on success the entire buffer must be initialised.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    use selium_guest::random_bytes;

    if len == 0 {
        return Ok(());
    }

    let bytes = match random_bytes(len as u32) {
        Ok(bytes) => bytes,
        Err(_) => return Err(getrandom::Error::UNEXPECTED),
    };

    // SAFETY: `getrandom` guarantees `dest` is valid for `len` bytes of writes.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), dest, len);
    }
    Ok(())
}

/// Watches the anchor table and rebuilds the connector's union verifier on
/// every change, so tenant onboarding and revocation take effect for the next
/// handshake.
async fn anchor_refresher(
    table: Option<LiveTableView<String, Vec<u8>, ShmTransport>>,
    anchors: Arc<Option<ClientAnchorSet>>,
) {
    let Some(table) = table else {
        return;
    };
    let Some(anchors) = anchors.as_ref() else {
        return;
    };
    loop {
        if let Err(e) = table.sync_async().await {
            warn!("quic-connector: anchor table wait failed: {e}");
            selium_guest::yield_now().await;
            continue;
        }
        if let Err(e) = rebuild_anchors(anchors, &table) {
            warn!("quic-connector: anchor rebuild failed: {e}");
        }
    }
}

/// Attaches to the identity guest's published anchor live table, returning a
/// read-only view of it. Distinguishes "identity not deployed" (mTLS opt-out)
/// from "identity deployed but unusable" (fail closed).
async fn attach_anchor_table(ctx: &mut Context) -> AnchorTableSource {
    let target = match ctx.lookup(ANCHOR_TABLE_ROUTE).await {
        Ok(Some(target)) => target,
        Ok(None) => {
            info!("quic-connector: no identity anchor table route registered");
            return AnchorTableSource::NotDeployed;
        }
        Err(e) => {
            warn!("quic-connector: anchor table resolve failed: {e}");
            return AnchorTableSource::Unavailable;
        }
    };
    let channel = match Channel::attach(target.resource_id) {
        Ok(channel) => channel,
        Err(e) => {
            warn!("quic-connector: anchor table region attach failed: {e}");
            return AnchorTableSource::Unavailable;
        }
    };
    // Replay from the ring start: anchors may predate this attachment.
    let transport = match ShmTransport::new_replay(&channel, &channel) {
        Ok(transport) => transport,
        Err(e) => {
            warn!("quic-connector: anchor table transport failed: {e}");
            return AnchorTableSource::Unavailable;
        }
    };
    let subscriber = Subscriber::new(FramedRead::new(transport), None);
    match LiveTableView::new(subscriber).map(Box::new) {
        Ok(view) => AnchorTableSource::Attached(view),
        Err(e) => {
            warn!("quic-connector: anchor table view failed: {e}");
            AnchorTableSource::Unavailable
        }
    }
}

/// Entrypoint for the QUIC connector system guest.
///
#[entrypoint]
async fn connector_quic(ctx: Context) -> anyhow::Result<()> {
    match connector_quic_inner(ctx).await {
        Ok(()) => Ok(()),
        Err(e) => {
            error!("quic-connector: startup failed: {e:#}");
            Err(e)
        }
    }
}

/// Receives a discovery `Context` for SNI route resolution. On wasm32 the
/// host provides randomness and time through hostcalls; both backends are
/// registered before any TLS operation. The server's own certificate/key are
/// loaded from blob storage (loud failure on missing/invalid material). Client
/// trust anchors are **opt-in via identity deployment**: when the identity
/// guest's anchor-table route is registered, the connector sources its client
/// trust anchors from that live table, refuses every client certificate until
/// identity's initial anchor state is applied, and rebuilds the union verifier
/// whenever the table changes. When no identity guest is deployed (no
/// anchor-table route registered), the connector serves without client
/// authentication and stream handoffs carry empty identity metadata — the
/// deployment's choice for public, unauthorised endpoints; identity-requiring
/// guests (e.g. the bridge) then refuse those handoffs. A registered but
/// unusable anchor source fails closed: the connector keeps serving but refuses
/// every client certificate, never silently downgrading to no client
/// authentication. A UDP socket is bound, and the quinn endpoint accepts
/// connections; each accepted connection is routed by SNI and served by its
/// own relay task.
async fn connector_quic_inner(mut ctx: Context) -> anyhow::Result<()> {
    #[cfg(target_arch = "wasm32")]
    register_wasm_time_source();

    drop(selium_guest::log::init());
    info!("quic-connector: started");

    let (certs, key) = load_server_identity().with_context(
        || "quic-connector: TLS setup failed; refusing to serve QUIC without TLS material",
    )?;

    // Client authentication is opt-in via identity deployment. With identity
    // deployed, trust anchors are live-table-sourced: the set starts empty —
    // refusing every client certificate — until identity's initial anchor
    // state is applied below; it never silently disables client
    // authentication. Without identity, mTLS is off and handoffs carry empty
    // identity metadata.
    let mut anchors = None;
    let mut anchor_table = None;
    match attach_anchor_table(&mut ctx).await {
        AnchorTableSource::Attached(table) => {
            info!("quic-connector: client trust anchors sourced from the identity anchor table");
            let set = ClientAnchorSet::empty()
                .with_context(|| "quic-connector: empty anchor set build failed")?;
            anchors = Some(set);
            anchor_table = Some(*table);
        }
        AnchorTableSource::NotDeployed => {
            info!(
                "quic-connector: no identity guest deployed; serving without client authentication"
            );
        }
        AnchorTableSource::Unavailable => {
            // Fail closed: a registered-but-unusable anchor source must not
            // downgrade to unauthenticated serving.
            warn!(
                "quic-connector: identity anchor table unusable; refusing all client certificates"
            );
            anchors = Some(
                ClientAnchorSet::empty()
                    .with_context(|| "quic-connector: empty anchor set build failed")?,
            );
        }
    }

    let server_config = build_server_config(certs, key, anchors.as_ref())
        .with_context(|| "quic-connector: TLS config build failed")?;

    let local_addr: SocketAddr = QUIC_LISTEN_ADDR
        .parse()
        .with_context(|| "quic-connector: invalid listen address")?;

    let socket = UdpSocket::bind(QUIC_LISTEN_ADDR)
        .await
        .with_context(|| "quic-connector: UDP bind failed")?;

    let quic_socket = QuicUdpSocket::new(socket, local_addr);
    let endpoint = build_endpoint(
        Arc::new(quic_socket),
        Arc::new(ConnectorRuntime),
        Some(server_config),
    )
    .with_context(|| "quic-connector: endpoint creation failed")?;

    info!("quic-connector: listening on {QUIC_LISTEN_ADDR}");

    // Fetch the advisory domain table once, so SNI resolution can project
    // wire names onto the tenant tree locally before the discovery lookup.
    let domains = ctx
        .load_domains()
        .await
        .with_context(|| "quic-connector: failed to load domain table")?;
    let resolver: ResolverHandle =
        Arc::new(tokio::sync::Mutex::new(RouteResolver::new(ctx, domains)));

    // Apply identity's initial anchor state before reporting ready, so the
    // connector never reports ready with no anchor snapshot applied.
    if let Some(table) = &anchor_table {
        if let Err(e) = table.sync() {
            warn!("quic-connector: initial anchor table sync failed: {e}");
        }
        if let Some(set) = &anchors
            && let Err(e) = rebuild_anchors(set, table)
        {
            warn!("quic-connector: initial anchor rebuild failed: {e}");
        }
    }
    mark_ready();

    // Watch the anchor table and rebuild the union verifier on change.
    let anchors = Arc::new(anchors);
    spawn(anchor_refresher(anchor_table, anchors.clone()));

    // Per-tenant stream-admission rate limiter shared across every connection
    // task: keyed by the authenticated client's tenant (falling back to the
    // resolved serving tenant), refusing streams over the rate before a
    // per-stream region is allocated.
    let limiter = AdmissionLimiter::new(DEFAULT_ADMISSION_TOKENS_PER_SEC, DEFAULT_ADMISSION_BURST);

    loop {
        let Some(incoming) = endpoint.accept().await else {
            break;
        };

        let connection = match incoming.await {
            Ok(connection) => connection,
            Err(e) => {
                warn!("quic-connector: incoming connection failed: {e}");
                continue;
            }
        };

        info!("quic-connector: QUIC handshake complete");
        let resolver = resolver.clone();
        let anchor_set = (*anchors).clone();
        let limiter = limiter.clone();
        spawn(async move {
            handle_connection(connection, resolver, anchor_set, limiter).await;
        });
    }

    Ok(())
}

/// Loads the QUIC server's own identity (certificate chain + private key)
/// from blob storage via the connector's `Storage` grant. Client trust anchors
/// are not loaded here: they are sourced from the identity anchor live table.
fn load_server_identity() -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), TlsError>
{
    use selium_guest::BlobStore;

    let store = BlobStore::open(TLS_STORE_NAME).map_err(|e| {
        error!("quic-connector: failed to open blob store '{TLS_STORE_NAME}': {e}");
        TlsError::StorageUnavailable
    })?;

    let cert_blob_id = store
        .manifest(TLS_CERT_MANIFEST)
        .map_err(|e| {
            error!("quic-connector: cert manifest '{TLS_CERT_MANIFEST}' not found: {e}");
            TlsError::MissingCertificate
        })?
        .ok_or_else(|| {
            error!("quic-connector: cert manifest '{TLS_CERT_MANIFEST}' is empty");
            TlsError::MissingCertificate
        })?;
    let cert_pem = store
        .get(&cert_blob_id)
        .map_err(|e| {
            error!("quic-connector: failed to read cert blob: {e}");
            TlsError::MissingCertificate
        })?
        .ok_or_else(|| {
            error!("quic-connector: cert blob is empty");
            TlsError::MissingCertificate
        })?;

    let key_blob_id = store
        .manifest(TLS_KEY_MANIFEST)
        .map_err(|e| {
            error!("quic-connector: key manifest '{TLS_KEY_MANIFEST}' not found: {e}");
            TlsError::MissingKey
        })?
        .ok_or_else(|| {
            error!("quic-connector: key manifest '{TLS_KEY_MANIFEST}' is empty");
            TlsError::MissingKey
        })?;
    let key_pem = store
        .get(&key_blob_id)
        .map_err(|e| {
            error!("quic-connector: failed to read key blob: {e}");
            TlsError::MissingKey
        })?
        .ok_or_else(|| {
            error!("quic-connector: key blob is empty");
            TlsError::MissingKey
        })?;

    let mut cert_reader = std::io::BufReader::new(cert_pem.as_slice());
    let certs: Vec<CertificateDer<'static>> = pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            error!("quic-connector: invalid cert PEM: {e}");
            TlsError::InvalidCertificate
        })?;

    if certs.is_empty() {
        error!("quic-connector: empty certificate chain");
        return Err(TlsError::InvalidCertificate);
    }

    let mut key_reader = std::io::BufReader::new(key_pem.as_slice());
    let key = loop {
        match pemfile::read_one(&mut key_reader).map_err(|e| {
            error!("quic-connector: invalid key PEM: {e}");
            TlsError::InvalidKey
        })? {
            Some(pemfile::Item::Pkcs1Key(k)) => break PrivateKeyDer::Pkcs1(k),
            Some(pemfile::Item::Pkcs8Key(k)) => break PrivateKeyDer::Pkcs8(k),
            Some(pemfile::Item::Sec1Key(k)) => break PrivateKeyDer::Sec1(k),
            None => {
                error!("quic-connector: no private key found in key PEM");
                return Err(TlsError::InvalidKey);
            }
            _ => continue,
        }
    };

    Ok((certs, key))
}

/// Rebuilds the anchor set from the current table state. Keys are
/// `client-ca-<tenant>`; tombstones (removed tenants) are skipped so their
/// anchors drop out of the rebuilt union verifier.
fn rebuild_anchors(
    anchors: &ClientAnchorSet,
    table: &LiveTableView<String, Vec<u8>, ShmTransport>,
) -> anyhow::Result<()> {
    let entries = table
        .scan(usize::MAX)
        .map_err(|e| anyhow::anyhow!("anchor table scan failed: {e}"))?;
    let mut pairs = Vec::with_capacity(entries.len());
    for (key, record) in entries {
        let Some(cert_der) = record.value else {
            continue;
        };
        let Some(tenant) = key.strip_prefix(ANCHOR_KEY_PREFIX) else {
            warn!("quic-connector: ignoring non-anchor table key {key:?}");
            continue;
        };
        pairs.push((tenant.to_string(), CertificateDer::from(cert_der)));
    }
    let count = pairs.len();
    anchors
        .replace(pairs)
        .map_err(|e| anyhow::anyhow!("anchor replace failed: {e}"))?;
    info!("quic-connector: rebuilt client anchor set ({count} anchors)");
    Ok(())
}
