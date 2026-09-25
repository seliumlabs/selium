//! HTTP/1.1 edge connector system guest.
//!
//! Terminates external TCP/TLS/HTTP-1.1 at the edge and forwards typed,
//! schema-encoded HTTP messages over shared-memory channels, so application
//! guests serve web traffic with no network capabilities of their own.
//!
//! # Architecture
//!
//! Each accepted connection gets its own task running a windowed pipeline
//! ([`pipeline::handle_connection`]): requests are parsed off the socket,
//! routed via discovery, and forwarded through typed sessions with a
//! bounded in-flight window; replies are reordered into request order and
//! written back to the wire. Connections are served concurrently; routes
//! are resolved through a shared, cached discovery resolver.

use std::sync::Arc;

use anyhow::Context as _;
use rustls_pemfile as pemfile;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use selium_guest::{
    Context, TcpListener, TcpStream, debug, entrypoint, error, info, mark_ready, spawn, warn,
};
use thiserror::Error;
use tokio_rustls::TlsAcceptor;
// Feature-unification anchor, not a code dependency: pulls in `ring` (with its
// `wasm32_unknown_unknown_js` feature) so `SystemRandom` compiles on
// wasm32-unknown-unknown — the backend actually used is getrandom's `custom`
// (see `.cargo/config.toml`). Guards against cargo-shear removing the dep.
#[cfg(target_arch = "wasm32")]
use ring as _;

pub use pipeline::{
    ConnectionConfig, ForwardError, ForwardSession, HTTP_STREAM_INTERFACE, ReplyEvent, ReplySink,
    SessionFactory, ShmSession, ShmSessionFactory, handle_connection,
};
pub use resolve::{ResolveError, ResolverHandle, RouteResolver};

pub mod codec;
pub mod pipeline;
pub mod resolve;
pub mod wire_out;

/// Manifest name for the certificate chain PEM.
const TLS_CERT_MANIFEST: &str = "cert-pem";
/// Manifest name for the private key PEM.
const TLS_KEY_MANIFEST: &str = "key-pem";
/// Storage blob store name for TLS material.
const TLS_STORE_NAME: &str = "tls-certs";

#[derive(Debug, Error)]
enum TlsError {
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
    #[error("TLS configuration error")]
    ConfigError,
}

/// Custom `getrandom` backend for wasm32, invoked by the `getrandom` crate
/// when built with the `custom` backend (`getrandom_backend = "custom"`).
///
/// Fills the caller's (possibly uninitialized) buffer by calling the host
/// `RandomBytes` hostcall, then copies the bytes into the destination.
///
/// # Safety
/// The contract is defined by `getrandom`: `dest` must be valid for writes of
/// `len` bytes, and on success the entire buffer must be initialized.
#[cfg(target_arch = "wasm32")]
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    use selium_guest::random_bytes;

    // `getrandom` may request a zero-length buffer; the hostcall expects a
    // real length, so short-circuit before touching the host.
    if len == 0 {
        return Ok(());
    }

    let bytes = match random_bytes(len as u32) {
        Ok(bytes) => bytes,
        // The hostcall error carries no meaning for `getrandom` consumers, so
        // collapse it into a generic unexpected-error.
        Err(_) => return Err(getrandom::Error::UNEXPECTED),
    };

    // Safety: `getrandom` guarantees `dest` is valid for `len` bytes of writes.
    unsafe {
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), dest, len);
    }
    Ok(())
}

/// Entrypoint for the HTTP connector system guest.
///
/// Receives a discovery `Context` handle for route resolution. The runtime
/// wires this at bootstrap via the entrypoint args mechanism.
///
/// TLS certificates are loaded from blob storage at startup. The connector
/// fails loudly if certificate material is missing or invalid, and never
/// serves plaintext HTTP on the TLS listener.
/// Each accepted connection is handled by its own task: TLS handshake, then
/// the windowed forwarding pipeline. Connections are served concurrently;
/// one slow (or parked-on-backpressure) connection never blocks the others.
#[entrypoint]
async fn connector_http(mut ctx: Context) -> anyhow::Result<()> {
    // On wasm32 the host provides randomness and time through hostcalls;
    // register both backends before any TLS operation touches `ring`/
    // `getrandom` or `rustls-pki-types`/`web-time`.
    #[cfg(target_arch = "wasm32")]
    {
        // The custom getrandom backend (`__getrandom_v03_custom`) is declared
        // at the bottom of this file; it forwards to the host `RandomBytes`
        // hostcall. Register the time backend before any TLS operation touches
        // `rustls-pki-types`/`web-time`.
        web_time::set_custom_time_source(web_time::TimeSource {
            monotonic_ns: || {
                selium_guest::time::Instant::now()
                    .expect("TimeMonotonic hostcall")
                    .as_nanos()
            },
            wall_clock_ns: || selium_guest::time::now().expect("TimeNow hostcall"),
        });
    }

    drop(selium_guest::log::init());
    info!("http-connector started");

    // Load TLS configuration. This fails loudly on missing/invalid material
    // per spec: "loud failure on missing/invalid cert material".
    let tls_config = load_tls_config().with_context(
        || "http-connector: TLS setup failed; refusing to serve plaintext on TLS listener",
    )?;
    info!("http-connector: TLS configured");

    let listener =
        TcpListener::bind("0.0.0.0:443").with_context(|| "http-connector: bind failed")?;
    info!("http-connector: bound to 0.0.0.0:443");

    mark_ready();

    // Fetch the advisory domain table once, so Host resolution can project
    // wire names onto the tenant tree locally before the discovery lookup.
    let domains = ctx
        .load_domains()
        .await
        .with_context(|| "http-connector: failed to load domain table")?;
    let acceptor = TlsAcceptor::from(tls_config);
    let resolver: ResolverHandle =
        Arc::new(tokio::sync::Mutex::new(RouteResolver::new(ctx, domains)));
    let config = ConnectionConfig::default();

    loop {
        let stream: TcpStream = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                warn!("http-connector: accept failed: {e}");
                continue;
            }
        };

        info!("http-connector: accepted connection");

        // Per-connection task: handshake, then the forwarding pipeline.
        // Spawning keeps connections independent — a slow or
        // backpressure-parked connection never blocks the accept loop or
        // any other connection.
        let acceptor = acceptor.clone();
        let resolver = resolver.clone();
        let factory = ShmSessionFactory;
        spawn(async move {
            let tls_stream = match acceptor.accept(stream).await {
                Ok(s) => s,
                Err(e) => {
                    warn!("http-connector: TLS handshake failed: {e}");
                    return;
                }
            };
            debug!("http-connector: TLS handshake complete");
            handle_connection(tls_stream, resolver, factory, config).await;
        });
    }
}

/// Load TLS certificate and key material from storage via the connector's
/// `Storage` grant. Fails loudly on missing or invalid material.
fn load_tls_config() -> Result<Arc<rustls::ServerConfig>, TlsError> {
    use selium_guest::BlobStore;

    let store = BlobStore::open(TLS_STORE_NAME).map_err(|e| {
        error!("http-connector: failed to open blob store '{TLS_STORE_NAME}': {e}");
        TlsError::StorageUnavailable
    })?;

    // Load certificate chain.
    let cert_blob_id = store
        .manifest(TLS_CERT_MANIFEST)
        .map_err(|e| {
            error!("http-connector: cert manifest '{TLS_CERT_MANIFEST}' not found: {e}");
            TlsError::MissingCertificate
        })?
        .ok_or_else(|| {
            error!("http-connector: cert manifest '{TLS_CERT_MANIFEST}' is empty");
            TlsError::MissingCertificate
        })?;
    let cert_pem = store
        .get(&cert_blob_id)
        .map_err(|e| {
            error!("http-connector: failed to read cert blob: {e}");
            TlsError::MissingCertificate
        })?
        .ok_or_else(|| {
            error!("http-connector: cert blob is empty");
            TlsError::MissingCertificate
        })?;

    // Load private key.
    let key_blob_id = store
        .manifest(TLS_KEY_MANIFEST)
        .map_err(|e| {
            error!("http-connector: key manifest '{TLS_KEY_MANIFEST}' not found: {e}");
            TlsError::MissingKey
        })?
        .ok_or_else(|| {
            error!("http-connector: key manifest '{TLS_KEY_MANIFEST}' is empty");
            TlsError::MissingKey
        })?;
    let key_pem = store
        .get(&key_blob_id)
        .map_err(|e| {
            error!("http-connector: failed to read key blob: {e}");
            TlsError::MissingKey
        })?
        .ok_or_else(|| {
            error!("http-connector: key blob is empty");
            TlsError::MissingKey
        })?;

    // Parse certificates.
    let mut cert_reader = std::io::BufReader::new(cert_pem.as_slice());
    let certs: Vec<CertificateDer<'static>> = pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            error!("http-connector: invalid cert PEM: {e}");
            TlsError::InvalidCertificate
        })?;

    if certs.is_empty() {
        error!("http-connector: empty certificate chain");
        return Err(TlsError::InvalidCertificate);
    }

    // Parse private key.
    let mut key_reader = std::io::BufReader::new(key_pem.as_slice());
    let key = loop {
        match pemfile::read_one(&mut key_reader).map_err(|e| {
            error!("http-connector: invalid key PEM: {e}");
            TlsError::InvalidKey
        })? {
            Some(pemfile::Item::Pkcs1Key(k)) => break PrivateKeyDer::Pkcs1(k),
            Some(pemfile::Item::Pkcs8Key(k)) => break PrivateKeyDer::Pkcs8(k),
            Some(pemfile::Item::Sec1Key(k)) => break PrivateKeyDer::Sec1(k),
            None => {
                error!("http-connector: no private key found in key PEM");
                return Err(TlsError::InvalidKey);
            }
            _ => continue,
        }
    };

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| {
            error!("http-connector: failed to build TLS config: {e}");
            TlsError::ConfigError
        })?;

    info!("http-connector: TLS configured successfully");
    Ok(Arc::new(config))
}
