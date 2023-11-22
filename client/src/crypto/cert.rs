use rustls::{Certificate, PrivateKey, RootCertStore};
use rustls_pemfile::{certs, pkcs8_private_keys, rsa_private_keys};
use selium_std::errors::{Result, SeliumError};
use std::{fs, io, path::Path};

pub type KeyPair = (Vec<Certificate>, PrivateKey);

fn load_key<T: AsRef<Path>>(path: T) -> Result<PrivateKey> {
    let path = path.as_ref();
    let key = fs::read(path)
        .map_err(|err| SeliumError::InvalidKeys("failed to read private key.", err))?;
    let key = if path.extension().map_or(false, |x| x == "der") {
        PrivateKey(key)
    } else {
        let pkcs8 = pkcs8_private_keys(&mut &*key)
            .map_err(|err| SeliumError::InvalidKeys("malformed PKCS #8 private key.", err))?;

        match pkcs8.into_iter().next() {
            Some(x) => PrivateKey(x),
            None => {
                let rsa = rsa_private_keys(&mut &*key).map_err(|err| {
                    SeliumError::InvalidKeys("malformed PKCS #1 private key.", err)
                })?;
                match rsa.into_iter().next() {
                    Some(x) => PrivateKey(x),
                    None => {
                        let message = "no private keys found in file.";

                        return Err(SeliumError::InvalidKeys(
                            message,
                            io::Error::new(io::ErrorKind::UnexpectedEof, message),
                        ));
                    }
                }
            }
        }
    };

    Ok(key)
}

fn load_certs<T: AsRef<Path>>(path: T) -> Result<Vec<Certificate>> {
    let path = path.as_ref();

    let cert_chain = fs::read(path)
        .map_err(|err| SeliumError::InvalidCerts("failed to read certificate chain.", err))?;

    let cert_chain = if path.extension().map_or(false, |x| x == "der") {
        vec![Certificate(cert_chain)]
    } else {
        certs(&mut &*cert_chain)
            .map_err(|err| SeliumError::InvalidCerts("invalid PEM-encoded certificate.", err))?
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
        return Err(SeliumError::InvalidRootCert);
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
