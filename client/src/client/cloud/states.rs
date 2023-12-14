use crate::constants::CLOUD_CA;
use crate::crypto::cert::load_root_store;
use crate::ClientCommon;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::RootCertStore;

#[doc(hidden)]
pub struct CloudWantsCertAndKey {
    pub(crate) common: ClientCommon,
    pub(crate) root_store: RootCertStore,
}

impl Default for CloudWantsCertAndKey {
    fn default() -> Self {
        let certs = vec![CertificateDer::from(CLOUD_CA.to_vec())];
        // Safe to unwrap, as the cloud CA is baked into the library.
        let root_store = load_root_store(certs).unwrap();

        Self {
            common: ClientCommon::default(),
            root_store,
        }
    }
}

#[doc(hidden)]
pub struct CloudWantsConnect<'a> {
    pub(crate) common: ClientCommon,
    pub(crate) root_store: RootCertStore,
    pub(crate) certs: Vec<CertificateDer<'a>>,
    pub(crate) key: PrivateKeyDer<'a>,
}

impl<'a> CloudWantsConnect<'a> {
    pub fn new(
        prev: CloudWantsCertAndKey,
        certs: &[CertificateDer<'a>],
        key: PrivateKeyDer<'a>,
    ) -> Self {
        Self {
            common: prev.common,
            root_store: prev.root_store,
            certs: certs.to_owned(),
            key,
        }
    }
}
