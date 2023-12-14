use rustls::{
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
    RootCertStore,
};
use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
use selium_std::errors::{CryptoError, Result};
use std::{fs, path::Path};

pub type KeyPair<'a> = (Vec<CertificateDer<'a>>, PrivateKeyDer<'a>);

fn load_key<'a, T: AsRef<Path>>(path: T) -> Result<PrivateKeyDer<'a>> {
    let path = path.as_ref();
    let key = fs::read(path).map_err(CryptoError::OpenKeyFileError)?;
    let key = if path.extension().map_or(false, |x| x == "der") {
        PrivatePkcs8KeyDer::from(key).into()
    } else {
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

pub fn load_certs<'a, T: AsRef<Path>>(path: T) -> Result<Vec<CertificateDer<'a>>> {
    let path = path.as_ref();
    let cert_chain = fs::read(path).map_err(CryptoError::OpenCertFileError)?;
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
pub(crate) fn load_root_store<'a>(
    certs: impl IntoIterator<Item = CertificateDer<'a>>,
) -> Result<RootCertStore> {
    let mut store = RootCertStore::empty();
    store.add_parsable_certificates(certs);

    if store.is_empty() {
        return Err(CryptoError::InvalidRootCert.into());
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
pub fn load_keypair<'a, T: AsRef<Path>>(cert_file: T, key_file: T) -> Result<KeyPair<'a>> {
    let certs = load_certs(cert_file)?;
    let private_key = load_key(key_file)?;
    Ok((certs, private_key))
}
