use crate::constants::CLOUD_CA;
use crate::crypto::cert::load_root_store;
use crate::ClientCommon;
use rustls::{Certificate, PrivateKey, RootCertStore};

#[doc(hidden)]
pub struct CloudWantsCertAndKey {
    pub(crate) common: ClientCommon,
    pub(crate) root_store: RootCertStore,
}

impl Default for CloudWantsCertAndKey {
    fn default() -> Self {
        let certs = vec![Certificate(CLOUD_CA.to_vec())];
        // Safe to unwrap, as the cloud CA is baked into the library.
        let root_store = load_root_store(&certs).unwrap();

        Self {
            common: ClientCommon::default(),
            root_store,
        }
    }
}

#[doc(hidden)]
pub struct CloudWantsConnect {
    pub(crate) common: ClientCommon,
    pub(crate) root_store: RootCertStore,
    pub(crate) certs: Vec<Certificate>,
    pub(crate) key: PrivateKey,
}

impl CloudWantsConnect {
    pub fn new(prev: CloudWantsCertAndKey, certs: &[Certificate], key: PrivateKey) -> Self {
        Self {
            common: prev.common,
            root_store: prev.root_store,
            certs: certs.to_owned(),
            key,
        }
    }
}
