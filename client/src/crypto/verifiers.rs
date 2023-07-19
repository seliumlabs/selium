use std::sync::Arc;

/// Dummy certificate verifier that treats any certificate as valid.
///
/// NOTE: such verification is vulnerable to MITM attacks, and is to
/// only be used for our prototyping!
///
/// TODO: Remove this hack once we have a stable IO MVP, and replace
/// it with our proposed mTLS implementation.
///
/// Example sourced from:
/// https://github.com/quinn-rs/quinn/blob/main/quinn/examples/insecure_connection.rs
///
pub struct SkipServerVerification;

impl SkipServerVerification {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::client::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}
