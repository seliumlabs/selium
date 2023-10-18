use anyhow::{bail, Context, Result};
use rustls::{Certificate, PrivateKey, RootCertStore};
use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
use std::{fs, path::Path};

pub type KeyPair = (Vec<Certificate>, PrivateKey);

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

/// Creates and returns a RootCertStore via certificates parsed from the provided
/// filepath pointing to a Certificate Authority file.
///
/// This function will fail if no certificates can be successfully parsed from
/// the CA input file.
///
/// # Arguments
///
/// * `ca_file` - The filepath to the CA file.
///
pub(crate) fn load_root_store<T: AsRef<Path>>(ca_file: T) -> Result<RootCertStore> {
    let ca_file = ca_file.as_ref();
    let mut store = RootCertStore::empty();
    let certs = load_certs(ca_file)?;
    store.add_parsable_certificates(&certs);

    if store.is_empty() {
        bail!("No valid certs found in file {:?}", ca_file);
    }

    Ok(store)
}

/// Extracts a public/private key pair from the provided filepaths.
///
/// This function will fail if no valid certificates or private key can be
/// successfully parsed from the input files.
///
/// # Arguments
///
/// * `cert_file` - The filepath to the certificate file.
/// * `key_file` - The filepath to the private key file.
///
pub fn load_keypair<T: AsRef<Path>>(cert_file: T, key_file: T) -> Result<KeyPair> {
    let certs = load_certs(cert_file)?;
    let private_key = load_key(key_file)?;
    Ok((certs, private_key))
}
