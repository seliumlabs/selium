//! Much of this code was borrowed with many thanks from the Quinn project:
//! https://github.com/quinn-rs/quinn/blob/main/quinn/examples/server.rs

use anyhow::{bail, Context, Result};
use quinn::{IdleTimeout, ServerConfig};
use rustls::server::AllowAnyAuthenticatedClient;
use rustls::{Certificate, PrivateKey, RootCertStore};
use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
use std::{fs, path::Path, sync::Arc};

const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

#[derive(Default)]
pub struct ConfigOptions {
    pub keylog: bool,
    pub stateless_retry: bool,
    pub max_idle_timeout: IdleTimeout,
}

pub fn server_config(
    root_store: RootCertStore,
    certs: Vec<Certificate>,
    key: PrivateKey,
    options: ConfigOptions,
) -> Result<ServerConfig> {
    let client_cert_verifier = Arc::new(AllowAnyAuthenticatedClient::new(root_store));

    let mut server_crypto = rustls::ServerConfig::builder()
        .with_safe_defaults()
        .with_client_cert_verifier(client_cert_verifier)
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

fn load_key<T: AsRef<Path>>(path: T) -> Result<PrivateKey> {
    let path = path.as_ref();
    let key = fs::read(path).context("failed to read private key")?;
    let key = if path.extension().map_or(false, |x| x == "der") {
        PrivateKey(key)
    } else {
        let pkcs8 = pkcs8_private_keys(&mut &*key).context("malformed PKCS #8 private key")?;
        match pkcs8.into_iter().next() {
            Some(x) => PrivateKey(x),
            None => {
                let rsa = rsa_private_keys(&mut &*key).context("malformed PKCS #1 private key")?;
                match rsa.into_iter().next() {
                    Some(x) => PrivateKey(x),
                    None => {
                        bail!("no private keys found");
                    }
                }
            }
        }
    };

    Ok(key)
}

fn load_certs<T: AsRef<Path>>(path: T) -> Result<Vec<Certificate>> {
    let path = path.as_ref();
    let cert_chain = fs::read(path).context("failed to read certificate chain")?;

    let cert_chain = if path.extension().map_or(false, |x| x == "der") {
        vec![Certificate(cert_chain)]
    } else {
        certs(&mut &*cert_chain)
            .context("invalid PEM-encoded certificate")?
            .into_iter()
            .map(Certificate)
            .collect()
    };

    Ok(cert_chain)
}

pub fn read_certs<T: AsRef<Path>>(
    cert_path: T,
    key_path: T,
) -> Result<(Vec<Certificate>, PrivateKey)> {
    let certs = load_certs(cert_path)?;
    let key = load_key(key_path)?;
    Ok((certs, key))
}

pub fn load_root_store<T: AsRef<Path>>(ca_file: T) -> Result<RootCertStore> {
    let ca_file = ca_file.as_ref();
    let mut store = RootCertStore::empty();
    let certs = load_certs(ca_file)?;
    store.add_parsable_certificates(&certs);

    if store.is_empty() {
        bail!("No valid certs found in file {ca_file:?}");
    }

    Ok(store)
}
