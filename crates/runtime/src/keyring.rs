//! Host-held PKI keyring backing the certificate-signing hostcalls.
//!
//! The keyring is the host side of the dumb-host/smart-guest crypto split:
//! the identity guest owns *policy* (who mints what, when to rotate or
//! revoke) while the host owns the *keys*. A depth-3 hierarchy is held here:
//!
//! - an **offline root** (`root`), retained for operator bootstrap only —
//!   no guest-facing hostcall references it;
//! - an **online intermediate** (`intermediate`), which signs tenant CAs;
//! - **per-tenant CA keys** (`tenant_cas`), which sign user leaf certificates.
//!
//! No read path exposes private-key material: the only private-key accessors
//! are the signing methods themselves, which reconstruct a [`rcgen::KeyPair`]
//! transiently from the stored PKCS#8 DER and discard it after signing.
//!
//! Tenant CA keys live behind a pluggable [`CaStore`] backing. The in-process
//! implementation is the day-1 default; a file or HSM backing replaces it
//! without changing the hostcall surface.

use std::collections::HashMap;

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
    SubjectPublicKeyInfo,
};
use rustls_pki_types::CertificateDer;
use thiserror::Error;
use time::{Duration, OffsetDateTime};

/// Default lifetime of a user leaf certificate, honouring the design's
/// "short-TTL leaf" requirement. Tokens degrade to expiry-driven revocation
/// alone after this window unless grants are removed first.
const DEFAULT_LEAF_TTL: Duration = Duration::days(1);

/// Pluggable backing for tenant CA keys.
pub trait CaStore: Send + Sync {
    /// Stores a tenant CA key.
    fn insert(&mut self, tenant: &str, ca: TenantCa) -> Result<(), KeyringError>;
    /// Returns a tenant CA key, if present.
    fn get(&self, tenant: &str) -> Option<&TenantCa>;
    /// Removes and returns a tenant CA key, if present.
    fn remove(&mut self, tenant: &str) -> Option<TenantCa>;
}

#[derive(Debug, Error)]
pub enum KeyringError {
    #[error("no intermediate key material configured")]
    IntermediateMissing,
    #[error("invalid certificate material: {0}")]
    InvalidMaterial(String),
    #[error("signing failed: {0}")]
    Signing(String),
    #[error("tenant CA not found: {tenant}")]
    TenantCaNotFound { tenant: String },
    #[error("unsupported subject key algorithm")]
    UnsupportedSubjectKey,
}

/// Host-side PKI key material: a DER certificate plus its PKCS#8 private key.
#[derive(Debug, Clone)]
pub struct KeyMaterial {
    /// DER-encoded X.509 certificate (public).
    pub cert_der: Vec<u8>,
    /// DER-encoded PKCS#8 private key (never leaves the host).
    pub key_der: Vec<u8>,
}

/// Material needed to initialise the keyring at startup.
#[derive(Debug, Clone, Default)]
pub struct KeyringBootstrap {
    /// Optional offline root, retained for operator bootstrap only.
    pub root: Option<KeyMaterial>,
    /// The online intermediate. `None` means the keyring cannot be
    /// initialised (the host would be unable to sign tenant CAs).
    pub intermediate: Option<KeyMaterial>,
}

/// Retained material for a tenant CA: public anchor certificate + private key.
/// `cert_der` is the only observable half; `key_der` is accessed only for
/// signing.
#[derive(Debug, Clone)]
pub struct TenantCa {
    pub(crate) cert_der: Vec<u8>,
    pub(crate) key_der: Vec<u8>,
}

/// The in-process backing implementation.
#[derive(Debug, Default)]
pub struct InMemoryCaStore {
    by_tenant: HashMap<String, TenantCa>,
}

/// The host-held keyring.
pub struct Keyring {
    intermediate: KeyMaterial,
    root: Option<KeyMaterial>,
    tenant_cas: Box<dyn CaStore>,
}

impl CaStore for InMemoryCaStore {
    fn insert(&mut self, tenant: &str, ca: TenantCa) -> Result<(), KeyringError> {
        self.by_tenant.insert(tenant.to_string(), ca);
        Ok(())
    }

    fn get(&self, tenant: &str) -> Option<&TenantCa> {
        self.by_tenant.get(tenant)
    }

    fn remove(&mut self, tenant: &str) -> Option<TenantCa> {
        self.by_tenant.remove(tenant)
    }
}

impl Keyring {
    /// Initialises a keyring from bootstrap material, failing loudly when the
    /// intermediate is missing or invalid.
    pub fn initialize(
        tenant_cas: Box<dyn CaStore>,
        bootstrap: KeyringBootstrap,
    ) -> Result<Self, KeyringError> {
        let intermediate = bootstrap
            .intermediate
            .ok_or(KeyringError::IntermediateMissing)?;
        validate_material(&intermediate)?;
        if let Some(root) = &bootstrap.root {
            validate_material(root)?;
        }
        Ok(Self {
            intermediate,
            root: bootstrap.root,
            tenant_cas,
        })
    }

    /// Generates a fresh root + intermediate hierarchy. The offline root is
    /// retained for operator bootstrap and never referenced by a hostcall.
    pub fn generate() -> Result<Self, KeyringError> {
        let root_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .map_err(|error| KeyringError::Signing(error.to_string()))?;
        let root_params = ca_params("Selium Root CA");
        let root_cert = root_params
            .self_signed(&root_key)
            .map_err(|error| KeyringError::Signing(error.to_string()))?;
        let root_issuer = Issuer::from_params(&root_params, &root_key);

        let intermediate_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .map_err(|error| KeyringError::Signing(error.to_string()))?;
        let intermediate_params = ca_params("Selium Intermediate CA");
        let intermediate_cert = intermediate_params
            .signed_by(&intermediate_key, &root_issuer)
            .map_err(|error| KeyringError::Signing(error.to_string()))?;

        Ok(Self {
            intermediate: material_from(&intermediate_cert, &intermediate_key),
            root: Some(material_from(&root_cert, &root_key)),
            tenant_cas: Box::new(InMemoryCaStore::default()),
        })
    }

    /// Swaps the tenant CA backing for a custom store (e.g. a file or HSM
    /// backing).
    pub fn with_tenant_store(mut self, store: Box<dyn CaStore>) -> Self {
        self.tenant_cas = store;
        self
    }

    /// Returns the intermediate certificate (public DER only).
    pub fn intermediate_cert(&self) -> &[u8] {
        &self.intermediate.cert_der
    }

    /// Returns the root certificate (public DER only), if retained.
    pub fn root_cert(&self) -> Option<&[u8]> {
        self.root
            .as_ref()
            .map(|material| material.cert_der.as_slice())
    }

    /// Mints a tenant CA: generates a host-held tenant key pair, signs it via
    /// the online intermediate, retains the key, and returns the DER-encoded
    /// tenant CA certificate.
    pub fn sign_tenant_ca(&mut self, tenant: &str) -> Result<Vec<u8>, KeyringError> {
        let issuer_key = KeyPair::try_from(self.intermediate.key_der.as_slice()).map_err(|e| {
            KeyringError::InvalidMaterial(format!("intermediate key parse failed: {e}"))
        })?;
        let issuer_cert = CertificateDer::from(self.intermediate.cert_der.clone());
        let issuer = Issuer::from_ca_cert_der(&issuer_cert, issuer_key).map_err(|e| {
            KeyringError::InvalidMaterial(format!("intermediate cert parse failed: {e}"))
        })?;

        let tenant_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
            .map_err(|error| KeyringError::Signing(error.to_string()))?;
        let params = ca_params(&format!("Selium Tenant CA {tenant}"));
        let cert = params
            .signed_by(&tenant_key, &issuer)
            .map_err(|error| KeyringError::Signing(error.to_string()))?;
        let cert_der = cert.der().to_vec();
        self.tenant_cas.insert(
            tenant,
            TenantCa {
                cert_der: cert_der.clone(),
                key_der: tenant_key.serialize_der(),
            },
        )?;
        Ok(cert_der)
    }

    /// Signs a client-supplied SPKI via the tenant's CA key, returning the
    /// DER-encoded short-TTL leaf certificate. The client's private key never
    /// leaves the client; only its public key crosses the boundary.
    pub fn sign_user_cert(&self, tenant: &str, spki_der: &[u8]) -> Result<Vec<u8>, KeyringError> {
        let ca = self
            .tenant_cas
            .get(tenant)
            .ok_or_else(|| KeyringError::TenantCaNotFound {
                tenant: tenant.to_string(),
            })?;
        let ca_key = KeyPair::try_from(ca.key_der.as_slice()).map_err(|e| {
            KeyringError::InvalidMaterial(format!("tenant CA key parse failed: {e}"))
        })?;
        let ca_cert = CertificateDer::from(ca.cert_der.clone());
        let issuer = Issuer::from_ca_cert_der(&ca_cert, ca_key).map_err(|e| {
            KeyringError::InvalidMaterial(format!("tenant CA cert parse failed: {e}"))
        })?;
        let subject = SubjectPublicKeyInfo::from_der(spki_der)
            .map_err(|_error| KeyringError::UnsupportedSubjectKey)?;

        let mut params = leaf_params(tenant);
        let not_before = OffsetDateTime::now_utc() - Duration::minutes(5);
        params.not_before = not_before;
        params.not_after = not_before + DEFAULT_LEAF_TTL;

        let cert = params
            .signed_by(&subject, &issuer)
            .map_err(|error| KeyringError::Signing(error.to_string()))?;
        Ok(cert.der().to_vec())
    }

    /// Deletes a tenant CA key from the keyring.
    pub fn revoke_ca(&mut self, tenant: &str) -> Result<(), KeyringError> {
        if self.tenant_cas.remove(tenant).is_none() {
            return Err(KeyringError::TenantCaNotFound {
                tenant: tenant.to_string(),
            });
        }
        Ok(())
    }
}

/// Parameters for a CA certificate: CA basic constraints plus key-cert-sign.
fn ca_params(common_name: &str) -> CertificateParams {
    let mut params = CertificateParams::default();
    params.distinguished_name = distinguished_name(common_name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    params.use_authority_key_identifier_extension = true;
    params
}

fn distinguished_name(common_name: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, common_name);
    dn
}

/// Parameters for a user leaf: no CA bit, digital signature + client auth.
fn leaf_params(tenant: &str) -> CertificateParams {
    let mut params = CertificateParams::default();
    params.distinguished_name = distinguished_name(&format!("Selium Client {tenant}"));
    params.is_ca = IsCa::NoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.use_authority_key_identifier_extension = true;
    params
}

fn material_from(cert: &Certificate, key: &KeyPair) -> KeyMaterial {
    KeyMaterial {
        cert_der: cert.der().to_vec(),
        key_der: key.serialize_der(),
    }
}

/// Validates that stored material round-trips through a signing handle.
fn validate_material(material: &KeyMaterial) -> Result<(), KeyringError> {
    let key = KeyPair::try_from(material.key_der.as_slice()).map_err(|error| {
        KeyringError::InvalidMaterial(format!("private key parse failed: {error}"))
    })?;
    let cert = CertificateDer::from(material.cert_der.clone());
    let _issuer = Issuer::from_ca_cert_der(&cert, key).map_err(|error| {
        KeyringError::InvalidMaterial(format!("certificate parse failed: {error}"))
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::PublicKeyData;
    use x509_parser::prelude::FromDer;

    #[test]
    fn generate_produces_intermediate_and_root() {
        let keyring = Keyring::generate().expect("generate keyring");
        assert!(!keyring.intermediate_cert().is_empty());
        assert!(keyring.root_cert().is_some());
    }

    #[test]
    fn initialize_fails_loudly_without_intermediate() {
        let result = Keyring::initialize(
            Box::new(InMemoryCaStore::default()),
            KeyringBootstrap {
                root: None,
                intermediate: None,
            },
        );
        assert!(matches!(result, Err(KeyringError::IntermediateMissing)));
    }

    #[test]
    fn initialize_rejects_invalid_intermediate() {
        let result = Keyring::initialize(
            Box::new(InMemoryCaStore::default()),
            KeyringBootstrap {
                root: None,
                intermediate: Some(KeyMaterial {
                    cert_der: b"not a certificate".to_vec(),
                    key_der: b"not a key".to_vec(),
                }),
            },
        );
        assert!(matches!(result, Err(KeyringError::InvalidMaterial(_))));
    }

    #[test]
    fn sign_tenant_ca_retains_key_and_returns_certificate() {
        let mut keyring = Keyring::generate().expect("generate");
        let cert_der = keyring.sign_tenant_ca("acme").expect("sign tenant ca");

        // The anchor is a CA certificate.
        let (_, parsed) = x509_parser::parse_x509_certificate(&cert_der).expect("parse anchor");
        assert!(parsed.tbs_certificate.is_ca(), "tenant CA must be a CA");

        // The key was retained: a leaf can be signed under it.
        let leaf = keyring
            .sign_user_cert("acme", &test_spki())
            .expect("sign user cert");
        assert!(!leaf.is_empty());
    }

    #[test]
    fn sign_user_cert_embeds_the_client_spki() {
        let mut keyring = Keyring::generate().expect("generate");
        keyring.sign_tenant_ca("acme").expect("sign tenant ca");

        let spki = test_spki();
        let leaf = keyring.sign_user_cert("acme", &spki).expect("sign leaf");

        let (_, input) =
            x509_parser::x509::SubjectPublicKeyInfo::from_der(&spki).expect("parse input spki");
        let (_, parsed) = x509_parser::parse_x509_certificate(&leaf).expect("parse leaf");
        let leaf_pki = parsed.tbs_certificate.subject_pki;

        // The leaf carries the client's subject public key, not a host key.
        assert_eq!(leaf_pki.algorithm, input.algorithm);
        assert_eq!(
            leaf_pki.subject_public_key.as_ref(),
            input.subject_public_key.as_ref(),
        );
    }

    #[test]
    fn sign_user_cert_leaf_expires_in_bounded_window() {
        let mut keyring = Keyring::generate().expect("generate");
        keyring.sign_tenant_ca("acme").expect("sign tenant ca");

        let leaf = keyring
            .sign_user_cert("acme", &test_spki())
            .expect("sign leaf");
        let (_, parsed) = x509_parser::parse_x509_certificate(&leaf).expect("parse leaf");
        let validity = parsed.validity();
        let now = OffsetDateTime::now_utc();

        // The leaf is currently valid and expires within the short-TTL window.
        assert!(validity.not_before.to_datetime() <= now);
        assert!(validity.not_after.to_datetime() > now);
        assert!(validity.not_after.to_datetime() < now + DEFAULT_LEAF_TTL);
    }

    #[test]
    fn sign_user_cert_unknown_tenant_fails() {
        let keyring = Keyring::generate().expect("generate");
        let result = keyring.sign_user_cert("missing", &test_spki());
        assert!(matches!(result, Err(KeyringError::TenantCaNotFound { .. })));
    }

    #[test]
    fn revoke_ca_removes_key() {
        let mut keyring = Keyring::generate().expect("generate");
        keyring.sign_tenant_ca("acme").expect("sign tenant ca");
        keyring.revoke_ca("acme").expect("revoke ca");

        // Signing under the revoked CA key fails, and double-revoke is not
        // mistaken for success.
        let _ = keyring.sign_user_cert("acme", &test_spki()).unwrap_err();
        assert!(matches!(
            keyring.revoke_ca("acme"),
            Err(KeyringError::TenantCaNotFound { .. })
        ));
    }

    /// Generates a client SPKI via rcgen, mirroring the client-side key
    /// generation whose public half the identity guest forwards to the host.
    fn test_spki() -> Vec<u8> {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("keygen");
        key.subject_public_key_info()
    }
}
