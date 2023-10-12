use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;
use anyhow::{bail, Context, Result};
use rustls::{Certificate, RootCertStore};
use rustls_pemfile::certs;

fn load_certs(cert_file: &PathBuf) -> Result<Vec<Certificate>> {
    certs(&mut BufReader::new(File::open(cert_file)?))
        .with_context(|| format!("Failed to load valid certificates from {cert_file:?}"))
        .map(|mut certs| certs.drain(..).map(Certificate).collect())
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
pub(crate) fn load_root_store(ca_file: &PathBuf) -> Result<RootCertStore> {
    let mut store = RootCertStore::empty();
    let certs = load_certs(ca_file)?;
    store.add_parsable_certificates(&certs);

    if store.is_empty() {
        bail!("No valid certs found in file {ca_file:?}");
    }

    Ok(store)
}
