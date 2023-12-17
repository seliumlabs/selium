use crate::ClientCommon;
use rustls::{Certificate, PrivateKey, RootCertStore};

#[doc(hidden)]
#[derive(Debug, Default)]
pub struct CustomWantsEndpoint {
    pub(crate) common: ClientCommon,
}

#[doc(hidden)]
#[derive(Debug)]
pub struct CustomWantsRootCert {
    pub(crate) common: ClientCommon,
    pub(crate) endpoint: String,
}

impl CustomWantsRootCert {
    pub fn new(prev: CustomWantsEndpoint, endpoint: &str) -> Self {
        Self {
            common: prev.common,
            endpoint: endpoint.to_owned(),
        }
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub struct CustomWantsCertAndKey {
    pub(crate) common: ClientCommon,
    pub(crate) endpoint: String,
    pub(crate) root_store: RootCertStore,
}

impl CustomWantsCertAndKey {
    pub fn new(prev: CustomWantsRootCert, root_store: RootCertStore) -> Self {
        Self {
            common: prev.common,
            endpoint: prev.endpoint,
            root_store,
        }
    }
}

#[doc(hidden)]
#[derive(Debug)]
pub struct CustomWantsConnect {
    pub(crate) common: ClientCommon,
    pub(crate) endpoint: String,
    pub(crate) root_store: RootCertStore,
    pub(crate) certs: Vec<Certificate>,
    pub(crate) key: PrivateKey,
}

impl CustomWantsConnect {
    pub fn new(prev: CustomWantsCertAndKey, certs: &[Certificate], key: PrivateKey) -> Self {
        Self {
            common: prev.common,
            endpoint: prev.endpoint,
            root_store: prev.root_store,
            certs: certs.to_owned(),
            key,
        }
    }
}
