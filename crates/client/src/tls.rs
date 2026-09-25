//! TLS/mTLS client configuration for the QUIC connection.

use std::sync::Arc;

use quinn::{
    crypto::rustls::QuicClientConfig,
    rustls::{
        RootCertStore,
        client::WebPkiServerVerifier,
        crypto::ring::default_provider,
        pki_types::{CertificateDer, PrivateKeyDer},
        version::TLS13,
    },
};

use crate::error::{Error, Result};

/// A client certificate chain and private key for mutual TLS.
#[derive(Debug)]
pub struct ClientIdentity {
    /// The client's certificate chain, leaf first, DER-encoded.
    pub cert_chain: Vec<CertificateDer<'static>>,
    /// The private key matching the leaf certificate, DER-encoded.
    pub key: PrivateKeyDer<'static>,
}

/// Connection options: the server certificate to trust and an optional
/// client identity to present for mutual TLS.
#[derive(Debug)]
pub struct ConnectOptions {
    /// Server name used for TLS SNI and certificate verification (routed by
    /// the connector's SNI resolution).
    pub server_name: String,
    /// Server root certificate(s) to trust, DER-encoded.
    pub server_root: Vec<CertificateDer<'static>>,
    /// Optional client identity (certificate chain + private key) presented
    /// when the server requires mutual TLS.
    pub identity: Option<ClientIdentity>,
    /// Optional QUIC transport configuration (idle timeout, RTT, flow control,
    /// ...). Defaults to quinn's settings when `None`.
    pub transport: Option<Arc<quinn::TransportConfig>>,
}

/// Builds a [`quinn::ClientConfig`] from the supplied options.
///
/// TLS 1.3 only, via the ring provider (mirroring the connector's server
/// config). The server is authenticated against `server_root`; when an
/// identity is provided the client presents the certificate chain and key.
pub fn build_client_config(options: &ConnectOptions) -> Result<quinn::ClientConfig> {
    let mut roots = RootCertStore::empty();
    for cert in &options.server_root {
        roots
            .add(cert.clone())
            .map_err(|e| Error::Tls(format!("invalid server root: {e}")))?;
    }

    let provider = Arc::new(default_provider());
    let builder = quinn::rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&TLS13])
        .map_err(|e| Error::Tls(format!("ring provider lacks TLS 1.3: {e}")))?;

    let verifier = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
        .build()
        .map_err(|e| Error::Tls(format!("server verifier: {e}")))?;

    let client = match &options.identity {
        Some(identity) => builder
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(identity.cert_chain.clone(), identity.key.clone_key())
            .map_err(|e| Error::Tls(format!("client auth cert: {e}")))?,
        None => builder
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth(),
    };

    let mut config = quinn::ClientConfig::new(Arc::new(
        QuicClientConfig::try_from(client)
            .map_err(|e| Error::Tls(format!("invalid quic client config: {e}")))?,
    ));
    if let Some(transport) = &options.transport {
        config.transport_config(transport.clone());
    }
    Ok(config)
}

/// Parses a PEM certificate chain into DER certificates.
pub fn certificates_from_pem(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    let mut reader = std::io::BufReader::new(pem);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<std::io::Result<_>>()
        .map_err(|e| Error::Tls(format!("invalid certificate PEM: {e}")))?;
    if certs.is_empty() {
        return Err(Error::Tls(
            "certificate PEM contains no certificates".into(),
        ));
    }
    Ok(certs)
}

/// Parses a PEM private key (PKCS#1, PKCS#8, or SEC1) into DER.
pub fn private_key_from_pem(pem: &[u8]) -> Result<PrivateKeyDer<'static>> {
    let mut reader = std::io::BufReader::new(pem);
    loop {
        match rustls_pemfile::read_one(&mut reader)
            .map_err(|e| Error::Tls(format!("invalid key PEM: {e}")))?
        {
            Some(rustls_pemfile::Item::Pkcs1Key(key)) => {
                return Ok(PrivateKeyDer::Pkcs1(key));
            }
            Some(rustls_pemfile::Item::Pkcs8Key(key)) => {
                return Ok(PrivateKeyDer::Pkcs8(key));
            }
            Some(rustls_pemfile::Item::Sec1Key(key)) => {
                return Ok(PrivateKeyDer::Sec1(key));
            }
            None => return Err(Error::Tls("key PEM contains no private key".into())),
            _ => continue,
        }
    }
}
