//! TLS configuration helpers for the Hyper driver.

use std::{net::IpAddr, sync::Arc};

use rustls::server::danger::ClientCertVerifier;
use rustls::{
    ClientConfig, RootCertStore, ServerConfig,
    crypto::ring::default_provider,
    pki_types::{CertificateDer, PrivateKeyDer},
    server::{ClientHello, ResolvesServerCert, WebPkiClientVerifier},
    sign,
};
use rustls_pki_types::{PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, pem::SliceIter};
use selium_abi::NetProtocol;
use selium_kernel::drivers::net::{TlsClientConfig, TlsServerConfig};
use webpki_roots::TLS_SERVER_ROOTS;

use crate::driver::HyperError;

#[derive(Debug)]
struct StaticResolver {
    certified_key: Arc<sign::CertifiedKey>,
}

impl StaticResolver {
    fn new(certified_key: Arc<sign::CertifiedKey>) -> Self {
        Self { certified_key }
    }
}

impl ResolvesServerCert for StaticResolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<sign::CertifiedKey>> {
        Some(Arc::clone(&self.certified_key))
    }
}

pub(crate) fn build_client_config(
    protocol: NetProtocol,
    tls: Option<&TlsClientConfig>,
) -> Result<Arc<ClientConfig>, HyperError> {
    let provider = default_provider();
    let tls_builder = ClientConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(HyperError::Rustls)?;
    if tls.and_then(|cfg| cfg.client_key_pem.as_ref()).is_some()
        && tls.and_then(|cfg| cfg.client_cert_pem.as_ref()).is_none()
    {
        return Err(HyperError::ClientKeyMissing);
    }
    let roots = match tls.and_then(|cfg| cfg.ca_bundle_pem.as_ref()) {
        Some(pem) => build_root_store(pem)?,
        None => RootCertStore::from_iter(TLS_SERVER_ROOTS.iter().cloned()),
    };
    let mut config = match tls.and_then(|cfg| cfg.client_cert_pem.as_ref()) {
        Some(client_cert_pem) => {
            let client_key_pem = tls
                .and_then(|cfg| cfg.client_key_pem.as_ref())
                .ok_or(HyperError::ClientKeyMissing)?;
            let cert_chain = parse_certificates(client_cert_pem)?;
            let key = parse_private_key(client_key_pem)?;
            tls_builder
                .with_root_certificates(roots)
                .with_client_auth_cert(cert_chain, key)
                .map_err(HyperError::Rustls)?
        }
        None => tls_builder
            .with_root_certificates(roots)
            .with_no_client_auth(),
    };
    config.alpn_protocols = resolve_alpn(protocol, tls.and_then(|cfg| cfg.alpn.as_ref()));
    Ok(Arc::new(config))
}

pub(crate) fn build_server_config(
    certified_key: Arc<sign::CertifiedKey>,
    alpn: Vec<Vec<u8>>,
    client_verifier: Arc<dyn ClientCertVerifier>,
) -> Result<Arc<ServerConfig>, HyperError> {
    let provider = default_provider();
    let tls_builder = ServerConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(HyperError::Rustls)?
        .with_client_cert_verifier(client_verifier);
    let resolver = Arc::new(StaticResolver::new(certified_key));
    let mut config = tls_builder.with_cert_resolver(resolver);
    config.alpn_protocols = alpn;
    Ok(Arc::new(config))
}

pub(crate) fn certified_key_from_config(
    config: &TlsServerConfig,
) -> Result<(Arc<sign::CertifiedKey>, Vec<Vec<u8>>), HyperError> {
    let certificates = parse_certificates(&config.cert_chain_pem)?;
    let cert_chain_bytes = certificates
        .iter()
        .map(|cert| cert.as_ref().to_vec())
        .collect::<Vec<_>>();
    let private_key = parse_private_key(&config.private_key_pem)?;
    let provider = default_provider();
    let certified_key = sign::CertifiedKey::from_der(certificates, private_key, &provider)
        .map_err(HyperError::Rustls)?;
    Ok((Arc::new(certified_key), cert_chain_bytes))
}

pub(crate) fn build_client_verifier(
    client_ca_pem: Option<&Vec<u8>>,
    require_client_auth: bool,
) -> Result<Arc<dyn ClientCertVerifier>, HyperError> {
    match (client_ca_pem, require_client_auth) {
        (None, false) => Ok(WebPkiClientVerifier::no_client_auth()),
        (None, true) => Err(HyperError::ClientAuthMissing),
        (Some(pem), require_client_auth) => {
            let roots = build_root_store(pem)?;
            let builder = WebPkiClientVerifier::builder(Arc::new(roots));
            let builder = if require_client_auth {
                builder
            } else {
                builder.allow_unauthenticated()
            };
            builder
                .build()
                .map_err(|err| HyperError::ClientAuth(err.to_string()))
        }
    }
}

pub(crate) fn resolve_alpn(
    protocol: NetProtocol,
    override_alpn: Option<&Vec<String>>,
) -> Vec<Vec<u8>> {
    match override_alpn {
        Some(values) => values
            .iter()
            .map(|value| value.as_bytes().to_vec())
            .collect(),
        None => match protocol {
            NetProtocol::Https => vec![b"h2".to_vec()],
            _ => Vec::new(),
        },
    }
}

pub(crate) fn server_name(
    domain: &str,
) -> Result<rustls::pki_types::ServerName<'static>, HyperError> {
    let trimmed = domain.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return Ok(rustls::pki_types::ServerName::IpAddress(ip.into()));
    }

    rustls::pki_types::ServerName::try_from(trimmed.to_string())
        .map_err(|_| HyperError::HostMismatch)
}

fn build_root_store(pem: &[u8]) -> Result<RootCertStore, HyperError> {
    let mut store = RootCertStore::empty();
    for cert in parse_certificates(pem)? {
        store
            .add(cert)
            .map_err(|err| HyperError::Certificate(err.to_string()))?;
    }
    Ok(store)
}

fn parse_certificates(bytes: &[u8]) -> Result<Vec<CertificateDer<'static>>, HyperError> {
    let parsed = SliceIter::new(bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| HyperError::Certificate(err.to_string()))?;
    if !parsed.is_empty() {
        return Ok(parsed);
    }

    Ok(vec![CertificateDer::from(bytes.to_vec())])
}

fn parse_private_key(bytes: &[u8]) -> Result<PrivateKeyDer<'static>, HyperError> {
    let pkcs8 = SliceIter::new(bytes)
        .collect::<Result<Vec<PrivatePkcs8KeyDer>, _>>()
        .map_err(|err| HyperError::PrivateKey(err.to_string()))?;
    if let Some(key) = pkcs8.into_iter().next() {
        return Ok(key.into());
    }

    let rsa = SliceIter::new(bytes)
        .collect::<Result<Vec<PrivatePkcs1KeyDer>, _>>()
        .map_err(|err| HyperError::PrivateKey(err.to_string()))?;
    if let Some(key) = rsa.into_iter().next() {
        return Ok(key.into());
    }

    PrivateKeyDer::try_from(bytes.to_vec())
        .map_err(|_| HyperError::PrivateKey("no private key found in provided material".into()))
}
