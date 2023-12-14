//! Much of this code was borrowed with many thanks from the Quinn project:
//! https://github.com/quinn-rs/quinn/blob/main/quinn/examples/server.rs

use anyhow::{bail, Context, Result};
use openssl::x509::X509;
use quinn::{Connection, IdleTimeout, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{pki_types::PrivateKeyDer, RootCertStore};
use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
use selium_std::errors::CryptoError;
use std::{fs, path::Path, sync::Arc};

const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

#[derive(Default)]
pub struct ConfigOptions {
    pub keylog: bool,
    pub stateless_retry: bool,
    pub max_idle_timeout: IdleTimeout,
}

pub fn server_config<'a>(
    root_store: RootCertStore,
    certs: Vec<CertificateDer<'a>>,
    key: PrivateKeyDer<'a>,
    options: ConfigOptions,
) -> Result<ServerConfig> {
    let client_verifier = WebPkiClientVerifier::builder(root_store.into()).build()?;

    let mut server_crypto = rustls::ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
        .with_single_cert(certs, key)?;

    server_crypto.alpn_protocols = ALPN_QUIC_HTTP.iter().map(|&x| x.into()).collect();
    if options.keylog {
        server_crypto.key_log = Arc::new(rustls::KeyLogFile::new());
    }

    let mut server_config = ServerConfig::with_crypto(Arc::new(server_crypto));
    let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
    transport_config.max_concurrent_uni_streams(0_u8.into());
    transport_config.max_idle_timeout(Some(options.max_idle_timeout));
    if options.stateless_retry {
        server_config.use_retry(true);
    }

    Ok(server_config)
}

fn load_key<'a, T: AsRef<Path>>(path: T) -> Result<PrivateKeyDer<'a>> {
    let path = path.as_ref();
    let key = fs::read(path).context("failed to read private key")?;
    let key = if path.extension().map_or(false, |x| x == "der") {
        PrivatePkcs8KeyDer::from(key).into()
    } else {
        // let pkcs8 = ;.context("malformed PKCS #8 private key")?;
        match pkcs8_private_keys(&mut &*key).into_iter().next() {
            Some(Ok(x)) => x.into(),
            Some(Err(e)) => return Err(CryptoError::MalformedPKCS8PrivateKey(e).into()),
            None => rsa_private_keys(&mut &*key)
                .into_iter()
                .next()
                .ok_or(CryptoError::NoPrivateKeysFound)?
                .map(|k| PrivateKeyDer::Pkcs1(k))
                .map_err(|e| CryptoError::MalformedPKCS1PrivateKey(e))?,
        }
    };

    Ok(key)
}

fn load_certs<'a, T: AsRef<Path>>(path: T) -> Result<Vec<CertificateDer<'a>>> {
    let path = path.as_ref();
    let cert_chain = fs::read(path).context("failed to read certificate chain")?;

    let cert_chain = if path.extension().map_or(false, |x| x == "der") {
        vec![cert_chain.into()]
    } else {
        let mut chain = Vec::new();
        for res in certs(&mut &*cert_chain).into_iter() {
            chain.push(res?);
        }
        chain
    };

    Ok(cert_chain)
}

pub fn read_certs<'a, T: AsRef<Path>>(
    cert_path: T,
    key_path: T,
) -> Result<(Vec<CertificateDer<'a>>, PrivateKeyDer<'a>)> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;
    Ok((certs, key))
}

pub fn load_root_store<T: AsRef<Path>>(ca_file: T) -> Result<RootCertStore> {
    let ca_file = ca_file.as_ref();
    let mut store = RootCertStore::empty();
    let certs = load_certs(ca_file)?;
    store.add_parsable_certificates(certs);

    if store.is_empty() {
        bail!("No valid certs found in file {ca_file:?}");
    }

    Ok(store)
}

pub fn get_pubkey_from_connection<'a>(connection: &Connection) -> Result<String> {
    let peer_identity = connection
        .peer_identity()
        .context("Unable to read peer identity")?;

    let certs = peer_identity
        .downcast_ref::<Vec<CertificateDer<'a>>>()
        .context("Unable to read cert")?;

    let pub_key = extract_public_key(certs)?;

    Ok(pub_key)
}

fn extract_public_key<'a>(certs: &[CertificateDer<'a>]) -> Result<String> {
    let first_cert = certs.get(0).context("Failed to get first certificate.")?;
    let cert = X509::from_der(first_cert.as_ref())?;
    let pub_key = cert.public_key()?;
    let pem = pub_key.public_key_to_pem()?;
    let result = String::from_utf8(pem)?;

    Ok(result)
}
