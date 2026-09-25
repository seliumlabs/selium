//! Per-connection client identity derivation for the mTLS connector.
//!
//! QUIC handshakes enforce client authentication at the endpoint via a union
//! verifier over all configured tenant trust anchors. This module derives the
//! *authenticated identity* of an accepted connection for handoff metadata:
//!
//! - **tenant scope** — the tenant whose trust anchor verifies the presented
//!   client certificate (each tenant's anchor is tried in turn, mirroring the
//!   endpoint-global union verifier);
//! - **key fingerprint** — the SHA-256 of the leaf certificate's
//!   SubjectPublicKeyInfo (SPKI), so "same key = same client" survives
//!   certificate renewal while remaining stable across rotation.
//!
//! The identity is encoded as an opaque, self-describing byte payload and
//! attached to every stream handoff as `HostQueueSend` metadata.

use std::sync::Arc;

use parking_lot::Mutex;
use quinn::{
    ServerConfig, TransportConfig,
    crypto::rustls::QuicServerConfig,
    rustls::{
        DigitallySignedStruct, DistinguishedName, Error as RustlsError, RootCertStore,
        SignatureScheme,
        client::danger::HandshakeSignatureValid,
        crypto::ring::default_provider,
        pki_types::{CertificateDer, PrivateKeyDer, UnixTime},
        server::{WebPkiClientVerifier, danger::ClientCertVerified, danger::ClientCertVerifier},
        version,
    },
};
use selium_abi::client_identity::{ClientIdentity, FINGERPRINT_LEN};
use sha2::{Digest, Sha256};

use crate::TlsError;

/// A single tenant's client-certification trust anchor + its verifier.
pub struct ClientAnchor {
    tenant: String,
    verifier: Arc<dyn ClientCertVerifier>,
}

/// Mandatory-client-auth verifier that refuses every certificate. The anchor
/// set's empty state: before identity publishes its first tenant anchor the
/// connector must refuse, never silently accept.
#[derive(Debug)]
struct RefuseAllClientVerifier;

/// The set of client trust anchors, rebuilt live from the identity guest's
/// published anchor table.
///
/// The set itself is a rustls [`ClientCertVerifier`], delegating every
/// handshake to the *current* union verifier. Rebuilding the set (on tenant
/// onboarding or revocation) therefore changes the anchoring for the next
/// handshake without disturbing connections already authenticated.
///
/// Building a set with no anchors via [`ClientAnchorSet::new`] is a hard
/// error (mTLS is endpoint-global, so a connector with no anchors must refuse
/// to serve). [`ClientAnchorSet::empty`] is the live-table flow's starting
/// state: it serves as a verifier that refuses every client until the first
/// anchor state arrives.
#[derive(Clone)]
pub struct ClientAnchorSet {
    inner: Arc<Mutex<AnchorInner>>,
}

/// The mutable inner state of the anchor set: the per-tenant anchors and the
/// endpoint-global union verifier built from them.
struct AnchorInner {
    anchors: Vec<ClientAnchor>,
    union: Arc<dyn ClientCertVerifier>,
}

impl ClientAnchor {
    /// Builds a tenant anchor from a single CA certificate (the trust root
    /// for that tenant's client certificates), given its verifier.
    fn with_verifier(tenant: String, verifier: Arc<dyn ClientCertVerifier>) -> Self {
        Self { tenant, verifier }
    }

    /// Returns this anchor's tenant scope.
    pub fn tenant(&self) -> &str {
        &self.tenant
    }

    /// Returns whether this anchor verifies the presented client certificate.
    pub fn verifies(
        &self,
        leaf: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
    ) -> bool {
        let now = UnixTime::now();
        self.verifier
            .verify_client_cert(leaf, intermediates, now)
            .is_ok()
    }
}

impl ClientCertVerifier for RefuseAllClientVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, RustlsError> {
        Err(RustlsError::General(
            "no client trust anchors configured".to_string(),
        ))
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Err(RustlsError::General(
            "no client trust anchors configured".to_string(),
        ))
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Err(RustlsError::General(
            "no client trust anchors configured".to_string(),
        ))
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ECDSA_NISTP256_SHA256]
    }
}

impl ClientAnchorSet {
    /// Builds the anchor set from `(tenant, CA certificate)` pairs.
    ///
    /// Fails loudly when no anchors are provided or a certificate is invalid.
    pub fn new(anchors: Vec<(String, CertificateDer<'static>)>) -> Result<Self, TlsError> {
        if anchors.is_empty() {
            return Err(TlsError::MissingClientAnchors);
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(AnchorInner::build(anchors)?)),
        })
    }

    /// Builds an empty (refusing) anchor set: the start state for the
    /// live-table flow before identity has published any tenant anchor.
    pub fn empty() -> Result<Self, TlsError> {
        Ok(Self {
            inner: Arc::new(Mutex::new(AnchorInner::build(Vec::new())?)),
        })
    }

    /// Rebuilds the anchor set from `(tenant, CA certificate)` pairs. An empty
    /// set is admitted (the connector keeps serving but refuses every client
    /// certificate). The change takes effect for the next handshake.
    pub fn replace(&self, anchors: Vec<(String, CertificateDer<'static>)>) -> Result<(), TlsError> {
        let inner = AnchorInner::build(anchors)?;
        *self.inner.lock() = inner;
        Ok(())
    }

    /// Returns the current endpoint-global union client verifier.
    fn current_union(&self) -> Arc<dyn ClientCertVerifier> {
        self.inner.lock().union.clone()
    }

    /// Derives the authenticated identity for a `quinn::Connection`.
    pub fn identity_for(&self, connection: &quinn::Connection) -> Option<ClientIdentity> {
        let chain = connection
            .peer_identity()?
            .downcast::<Vec<CertificateDer<'static>>>()
            .ok()?;
        self.identity_from_chain(&chain)
    }

    /// Derives the authenticated identity from a presented certificate chain.
    pub fn identity_from_chain(&self, chain: &[CertificateDer<'static>]) -> Option<ClientIdentity> {
        let leaf = chain.first()?;
        let fingerprint = spki_fingerprint(leaf)?;
        let intermediates = chain.get(1..).unwrap_or_default();
        let tenant = self
            .inner
            .lock()
            .anchors
            .iter()
            .find(|anchor| anchor.verifies(leaf, intermediates))
            .map(|anchor| anchor.tenant().to_string())?;
        Some(ClientIdentity {
            tenant,
            fingerprint,
        })
    }
}

impl ClientCertVerifier for ClientAnchorSet {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        now: UnixTime,
    ) -> Result<ClientCertVerified, RustlsError> {
        self.current_union()
            .verify_client_cert(end_entity, intermediates, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.current_union()
            .verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        self.current_union()
            .verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.current_union().supported_verify_schemes()
    }
}

impl std::fmt::Debug for ClientAnchorSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientAnchorSet").finish_non_exhaustive()
    }
}

impl AnchorInner {
    /// Builds anchors from `(tenant, CA certificate)` pairs. An empty set
    /// produces a deny-all verifier: the connector offers mandatory client
    /// authentication but refuses every certificate until identity publishes.
    fn build(anchors: Vec<(String, CertificateDer<'static>)>) -> Result<Self, TlsError> {
        let mut union_roots = RootCertStore::empty();
        let mut built = Vec::with_capacity(anchors.len());
        for (tenant, cert) in anchors {
            union_roots.add(cert.clone()).map_err(|e| {
                tracing::error!("quic-connector: invalid client anchor for {tenant}: {e}");
                TlsError::InvalidClientAnchor
            })?;
            let roots = RootCertStore::empty();
            let verifier = per_tenant_verifier(roots, &cert, &tenant)?;
            built.push(ClientAnchor::with_verifier(tenant, verifier));
        }

        let union: Arc<dyn ClientCertVerifier> = if built.is_empty() {
            Arc::new(RefuseAllClientVerifier)
        } else {
            WebPkiClientVerifier::builder(Arc::new(union_roots))
                .build()
                .map_err(|e| {
                    tracing::error!("quic-connector: client union verifier build failed: {e}");
                    TlsError::InvalidClientAnchor
                })?
        };

        Ok(Self {
            anchors: built,
            union,
        })
    }
}

/// Builds the QUIC server configuration: TLS 1.3, with **opt-in** mandatory
/// client authentication.
///
/// - `Some(anchors)`: every connection must present a client certificate
///   verifying against the union of the configured tenant anchors (mTLS).
/// - `None`: no client authentication (mTLS opt-out; routes that require it,
///   like the bridge, must not be served without anchors).
///
/// TLS 1.3 0-RTT early data stays **disabled** (the rustls default): early
/// data is replayable across connections, and the connector relays stream
/// bytes into the fabric under the authenticated client identity. TLS 1.3
/// session resumption stays **disabled** too: rustls skips client
/// authentication on PSK-resumed handshakes, so a resumption ticket would
/// bypass both mandatory client authentication and anchor revocation — every
/// connection runs the full certificate verification.
pub fn build_server_config(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    anchors: Option<&ClientAnchorSet>,
) -> Result<ServerConfig, TlsError> {
    let provider = default_provider();
    let builder = quinn::rustls::ServerConfig::builder_with_provider(Arc::new(provider))
        .with_protocol_versions(&[&version::TLS13])
        .map_err(|e| {
            tracing::error!("quic-connector: TLS provider missing TLS 1.3: {e}");
            TlsError::ConfigError
        })?;
    let rustls_config = match anchors {
        Some(anchors) => builder
            .with_client_cert_verifier(Arc::new(anchors.clone()))
            .with_single_cert(certs, key)
            .map_err(|e| {
                tracing::error!("quic-connector: failed to build TLS config: {e}");
                TlsError::ConfigError
            })?,
        None => builder
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| {
                tracing::error!("quic-connector: failed to build TLS config: {e}");
                TlsError::ConfigError
            })?,
    };

    // Session resumption stays disabled: rustls skips client authentication
    // on PSK-resumed TLS 1.3 handshakes, so a resumption ticket issued by a
    // prior connection would let a client bypass certificate verification
    // entirely — defeating both mandatory mTLS and anchor revocation. Every
    // connection therefore runs the full certificate verification: the
    // connector issues no TLS 1.3 tickets and stores no resumable sessions.
    let mut rustls_config = rustls_config;
    rustls_config.send_tls13_tickets = 0;
    rustls_config.session_storage = Arc::new(quinn::rustls::server::NoServerSessionStorage {});

    let quic_crypto = QuicServerConfig::try_from(rustls_config).map_err(|e| {
        tracing::error!("quic-connector: QUIC crypto config rejected: {e}");
        TlsError::ConfigError
    })?;
    let mut config = ServerConfig::with_crypto(Arc::new(quic_crypto));

    // Cap concurrent bidirectional streams per connection: quinn advertises
    // the limit as the connection's MAX_STREAMS, so a client cannot open
    // streams beyond the cap — no per-stream region is ever allocated for
    // them (see `MAX_CONCURRENT_BIDI_STREAMS`).
    let mut transport = TransportConfig::default();
    transport.max_concurrent_bidi_streams(crate::MAX_CONCURRENT_BIDI_STREAMS.into());
    config.transport_config(Arc::new(transport));

    Ok(config)
}

/// Builds a per-tenant client verifier from a single trust anchor.
fn per_tenant_verifier(
    mut roots: RootCertStore,
    cert: &CertificateDer<'static>,
    tenant: &str,
) -> Result<Arc<dyn ClientCertVerifier>, TlsError> {
    roots.add(cert.clone()).map_err(|e| {
        tracing::error!("quic-connector: invalid client anchor for {tenant}: {e}");
        TlsError::InvalidClientAnchor
    })?;
    WebPkiClientVerifier::builder(Arc::new(roots))
        .build()
        .map_err(|e| {
            tracing::error!("quic-connector: client verifier build failed for {tenant}: {e}");
            TlsError::InvalidClientAnchor
        })
}

/// SHA-256 of a certificate's SPKI.
fn spki_fingerprint(cert: &CertificateDer<'_>) -> Option<[u8; FINGERPRINT_LEN]> {
    let parsed = quinn::rustls::server::ParsedCertificate::try_from(cert).ok()?;
    let spki = parsed.subject_public_key_info();
    let digest = Sha256::digest(spki.as_ref());
    let mut out = [0u8; FINGERPRINT_LEN];
    out.copy_from_slice(&digest);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT_CERT_DER: &[u8] = include_bytes!("../tests/fixtures/client_cert.der");
    const SERVER_CERT_DER: &[u8] = include_bytes!("../tests/fixtures/cert.der");

    fn client_anchor() -> ClientAnchorSet {
        let cert = CertificateDer::from(CLIENT_CERT_DER.to_vec());
        ClientAnchorSet::new(vec![("acme".to_string(), cert)]).expect("anchor set")
    }

    #[test]
    fn absent_anchors_fails_loudly() {
        assert!(matches!(
            ClientAnchorSet::new(Vec::new()),
            Err(TlsError::MissingClientAnchors)
        ));
    }

    #[test]
    fn identity_is_derived_from_fixture_chain() {
        let anchors = client_anchor();
        let cert = CertificateDer::from(CLIENT_CERT_DER.to_vec());

        let identity = anchors
            .identity_from_chain(std::slice::from_ref(&cert))
            .expect("self-signed anchor verifies its own chain");

        assert_eq!(identity.tenant, "acme");
        let parsed = quinn::rustls::server::ParsedCertificate::try_from(&cert).expect("parse");
        let expected = Sha256::digest(parsed.subject_public_key_info().as_ref());
        assert_eq!(identity.fingerprint.as_slice(), expected.as_slice());
    }

    #[test]
    fn identity_is_rejected_for_unknown_chain() {
        // A chain issued outside the configured anchors (here: the server's
        // own self-signed certificate) must not derive an identity.
        let anchors = client_anchor();
        let server_cert = CertificateDer::from(SERVER_CERT_DER.to_vec());
        assert!(anchors.identity_from_chain(&[server_cert]).is_none());
    }

    #[test]
    fn empty_set_refuses_valid_client_certificate() {
        // The live-table flow's start state: before identity's initial
        // anchor state arrives, the connector refuses every client
        // certificate rather than silently disabling client authentication.
        let set = ClientAnchorSet::empty().expect("empty anchor set");
        let cert = CertificateDer::from(CLIENT_CERT_DER.to_vec());
        let refused = set
            .verify_client_cert(&cert, &[], UnixTime::now())
            .expect_err("the empty anchor set must refuse a certificate a configured set accepts");
        assert!(
            refused.to_string().contains("no client trust anchors"),
            "the refusal names the missing anchor state: {refused}"
        );
    }

    /// Generates a tenant CA and a client leaf signed by it, as the identity
    /// guest's signing oracle would (client key generated "client-side";
    /// only the public half reaches the CA).
    fn generated_tenant(
        ca_cn: &str,
        leaf_cn: &str,
    ) -> (CertificateDer<'static>, CertificateDer<'static>) {
        use rcgen::{
            BasicConstraints, CertificateParams, DistinguishedName, DnType,
            ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
            PKCS_ECDSA_P256_SHA256,
        };

        let ca_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("ca key");
        let mut ca_params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, ca_cn);
        ca_params.distinguished_name = dn;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca_cert = ca_params.self_signed(&ca_key).expect("self-signed ca");
        let issuer = Issuer::from_params(&ca_params, &ca_key);

        let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("leaf key");
        let mut leaf_params = CertificateParams::default();
        let mut leaf_dn = DistinguishedName::new();
        leaf_dn.push(DnType::CommonName, leaf_cn);
        leaf_params.distinguished_name = leaf_dn;
        leaf_params.is_ca = IsCa::NoCa;
        leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let leaf = leaf_params.signed_by(&leaf_key, &issuer).expect("leaf");

        (
            CertificateDer::from(ca_cert.der().to_vec()),
            CertificateDer::from(leaf.der().to_vec()),
        )
    }

    #[test]
    fn replace_propagates_anchor_addition_and_revocation() {
        // New tenant anchor propagates: a rebuilt set accepts certificates
        // chained to the new anchor.
        let (ca_a, leaf_a) = generated_tenant("Tenant CA acme", "Client acme");
        let (ca_b, leaf_b) = generated_tenant("Tenant CA beta", "Client beta");
        let set = ClientAnchorSet::new(vec![("acme".to_string(), ca_a.clone())])
            .expect("anchor set with acme");

        assert_eq!(
            set.identity_from_chain(std::slice::from_ref(&leaf_a))
                .expect("acme leaf")
                .tenant,
            "acme"
        );
        set.verify_client_cert(&leaf_a, &[], UnixTime::now())
            .expect("the union verifier accepts the acme leaf");

        // Revoked tenant anchor propagates: after a rebuild without acme,
        // its leaf is refused by both the union verifier and identity
        // derivation, while a concurrently added tenant is accepted.
        set.replace(vec![("beta".to_string(), ca_b)])
            .expect("replace with beta only");
        set.verify_client_cert(&leaf_a, &[], UnixTime::now())
            .expect_err("the next handshake chained to the removed anchor is refused");
        assert!(
            set.identity_from_chain(std::slice::from_ref(&leaf_a))
                .is_none()
        );
        assert_eq!(
            set.identity_from_chain(std::slice::from_ref(&leaf_b))
                .expect("beta leaf")
                .tenant,
            "beta"
        );
        set.verify_client_cert(&leaf_b, &[], UnixTime::now())
            .expect("the union verifier accepts the concurrently added beta leaf");

        // Full revocation (every anchor removed) refuses every client.
        set.replace(Vec::new()).expect("replace with none");
        set.verify_client_cert(&leaf_b, &[], UnixTime::now())
            .expect_err("the fully revoked set refuses every client");
        assert!(
            set.identity_from_chain(std::slice::from_ref(&leaf_b))
                .is_none()
        );
    }
}
