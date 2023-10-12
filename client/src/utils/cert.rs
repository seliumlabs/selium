use anyhow::Context;
use anyhow::Result;
use rustls::{Certificate, PrivateKey};
use rustls_pemfile::{certs, pkcs8_private_keys};
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

pub type KeyPair = (Vec<Certificate>, PrivateKey);

pub fn load_certs(cert_file: &PathBuf) -> Result<Vec<Certificate>> {
    certs(&mut BufReader::new(File::open(cert_file)?))
        .with_context(|| format!("Failed to load valid certificates from {cert_file:?}"))
        .map(|mut certs| certs.drain(..).map(Certificate).collect())
}

pub fn load_private_keys(key_file: &PathBuf) -> Result<Vec<PrivateKey>> {
    pkcs8_private_keys(&mut BufReader::new(File::open(key_file)?))
        .with_context(|| format!("Failed to load valid keys from {key_file:?}"))
        .map(|mut keys| keys.drain(..).map(PrivateKey).collect())
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
pub fn load_keypair(cert_file: &PathBuf, key_file: &PathBuf) -> Result<KeyPair> {
    let certs = load_certs(cert_file)?;
    let keys = load_private_keys(key_file)?;

    let private_key = keys
        .first()
        .with_context(|| format!("No valid PKCS8 private keys found in the file {key_file:?}"))?
        .to_owned();

    Ok((certs, private_key))
}
