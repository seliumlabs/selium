use super::certificate_builder::CertificateBuilder;
use anyhow::Result;
use rcgen::Certificate;

pub struct KeyPair(pub Vec<u8>, pub Vec<u8>);

impl KeyPair {
    pub fn client(ca: &Certificate) -> Result<Self> {
        let cert_builder = CertificateBuilder::client();
        Self::build(cert_builder, ca)
    }

    pub fn server(ca: &Certificate) -> Result<Self> {
        let cert_builder = CertificateBuilder::server();
        Self::build(cert_builder, ca)
    }

    fn build(builder: CertificateBuilder, ca: &Certificate) -> Result<Self> {
        let cert = builder
            .common_name("selium.com")
            .valid_for_days(5)
            .build()?;

        let signed_cert = cert.serialize_der_with_signer(ca)?;
        let key = cert.serialize_private_key_der();

        Ok(Self(signed_cert, key))
    }
}
