use super::certificate_builder::CertificateBuilder;
use anyhow::Result;
use rcgen::Certificate;

const COMMON_NAME: &str = "selium.io";

pub struct KeyPair(pub Vec<u8>, pub Vec<u8>);

impl KeyPair {
    pub fn client(ca: &Certificate, no_expiry: bool) -> Result<Self> {
        let cert_builder = CertificateBuilder::client();
        Self::build(cert_builder, ca, no_expiry)
    }

    pub fn server(ca: &Certificate, no_expiry: bool) -> Result<Self> {
        let cert_builder = CertificateBuilder::server();
        Self::build(cert_builder, ca, no_expiry)
    }

    fn build(builder: CertificateBuilder, ca: &Certificate, no_expiry: bool) -> Result<Self> {
        let mut builder = builder.common_name(COMMON_NAME);

        if !no_expiry {
            builder = builder.valid_for_days(5);
        }

        let cert = builder.build()?;
        let signed_cert = cert.serialize_der_with_signer(ca)?;
        let key = cert.serialize_private_key_der();

        Ok(Self(signed_cert, key))
    }
}
