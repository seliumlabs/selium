//! Much of this code was borrowed with many thanks from the Quinn project:
//! https://github.com/quinn-rs/quinn/blob/main/quinn/examples/server.rs

use std::{fs, path::PathBuf, sync::Arc};

use crate::util::cert::load_root_store;
use anyhow::{bail, Context, Result};
use quinn::{IdleTimeout, ServerConfig};
use rcgen::generate_simple_self_signed;
use rustls::server::AllowAnyAuthenticatedClient;
use rustls::{Certificate, PrivateKey};
use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};

const ALPN_QUIC_HTTP: &[&[u8]] = &[b"hq-29"];

#[derive(Default)]
pub struct ConfigOptions {
    pub keylog: bool,
    pub stateless_retry: bool,
    pub max_idle_timeout: IdleTimeout,
}

pub fn server_config(
    ca: PathBuf,
    certs: Vec<Certificate>,
    key: PrivateKey,
    options: ConfigOptions,
) -> Result<ServerConfig> {
    let root_store = load_root_store(&ca)?;
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

pub fn read_certs(cert_path: PathBuf, key_path: PathBuf) -> Result<(Vec<Certificate>, PrivateKey)> {
    let key = fs::read(key_path.clone()).context("failed to read private key")?;
    let key = if key_path.extension().map_or(false, |x| x == "der") {
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
    let cert_chain = fs::read(cert_path.clone()).context("failed to read certificate chain")?;
    let cert_chain = if cert_path.extension().map_or(false, |x| x == "der") {
        vec![Certificate(cert_chain)]
    } else {
        certs(&mut &*cert_chain)
            .context("invalid PEM-encoded certificate")?
            .into_iter()
            .map(Certificate)
            .collect()
    };

    Ok((cert_chain, key))
}

pub fn generate_self_signed_cert() -> Result<(Vec<Certificate>, PrivateKey)> {
    let cert = generate_simple_self_signed(vec!["localhost".to_string()])?;
    let key = PrivateKey(cert.serialize_private_key_der());

    eprintln!(
        "Warning! Using a self-signed certificate does not protect from
        person-in-the-middle attacks."
    );

    println!(
        "Your self-signed certificate public key is:\n{}",
        cert.serialize_pem()?
    );

    Ok((vec![Certificate(cert.serialize_der()?)], key))
}
