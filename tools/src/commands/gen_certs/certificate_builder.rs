use super::validity_range::ValidityRange;
use anyhow::Result;
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DnType, DnValue, ExtendedKeyUsagePurpose,
    IsCa, KeyUsagePurpose, SanType,
};

pub struct CertificateBuilder {
    params: CertificateParams,
}

impl CertificateBuilder {
    pub fn ca() -> Self {
        let mut params = CertificateParams::new(vec![]);

        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages.push(KeyUsagePurpose::DigitalSignature);
        params.key_usages.push(KeyUsagePurpose::KeyCertSign);
        params.key_usages.push(KeyUsagePurpose::CrlSign);

        Self { params }
    }

    pub fn server() -> Self {
        CertificateBuilder::entity(ExtendedKeyUsagePurpose::ServerAuth)
    }

    pub fn client() -> Self {
        CertificateBuilder::entity(ExtendedKeyUsagePurpose::ClientAuth)
    }

    pub fn country_name(mut self, name: &str) -> Self {
        self.params.distinguished_name.push(
            DnType::CountryName,
            DnValue::PrintableString(name.to_owned()),
        );
        self
    }

    pub fn organization_name(mut self, name: &str) -> Self {
        self.params
            .distinguished_name
            .push(DnType::OrganizationName, name);
        self
    }

    pub fn common_name(mut self, name: &str) -> Self {
        self.params
            .distinguished_name
            .push(DnType::CommonName, name);
        self
    }

    pub fn valid_for_days(mut self, days: i64) -> Self {
        let range = ValidityRange::new(days);
        self.params.not_before = range.start;
        self.params.not_after = range.end;
        self
    }

    pub fn build(self) -> Result<Certificate> {
        let cert = Certificate::from_params(self.params)?;
        Ok(cert)
    }

    fn entity(purpose: ExtendedKeyUsagePurpose) -> Self {
        let mut params = CertificateParams::new(vec![]);

        params
            .subject_alt_names
            .push(SanType::DnsName("localhost".to_owned()));

        params.key_usages.push(KeyUsagePurpose::DigitalSignature);
        params.use_authority_key_identifier_extension = true;
        params.extended_key_usages.push(purpose);

        Self { params }
    }
}
