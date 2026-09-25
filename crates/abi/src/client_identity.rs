//! Wire format for authenticated client identities carried as `HostQueueSend`
//! handoff metadata.
//!
//! The QUIC connector attaches this payload to every stream handoff; the
//! bridge-server decodes it to resolve a client's capability grants. The shape
//! is shared here so both guests own one contract rather than a duplicated one.

/// Length in bytes of the key fingerprint (SHA-256).
pub const FINGERPRINT_LEN: usize = 32;

/// An authenticated client identity: tenant scope + leaf SPKI fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    /// Tenant implied by the trust anchor that verified the client.
    pub tenant: String,
    /// SHA-256 of the client leaf certificate's SPKI.
    pub fingerprint: [u8; FINGERPRINT_LEN],
}

impl ClientIdentity {
    /// Encodes the identity as a self-describing payload:
    /// `u32 LE tenant length || tenant bytes || 32-byte fingerprint`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.tenant.len() + FINGERPRINT_LEN);
        out.extend_from_slice(&(self.tenant.len() as u32).to_le_bytes());
        out.extend_from_slice(self.tenant.as_bytes());
        out.extend_from_slice(&self.fingerprint);
        out
    }

    /// Decodes a payload produced by [`ClientIdentity::encode`].
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let tenant_len = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
        let tenant_start = 4usize;
        let tenant_end = tenant_start.checked_add(tenant_len)?;
        let fingerprint_end = tenant_end.checked_add(FINGERPRINT_LEN)?;
        let tenant = std::str::from_utf8(bytes.get(tenant_start..tenant_end)?)
            .ok()?
            .to_string();
        let fingerprint: [u8; FINGERPRINT_LEN] =
            bytes.get(tenant_end..fingerprint_end)?.try_into().ok()?;
        Some(Self {
            tenant,
            fingerprint,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_round_trips() {
        let identity = ClientIdentity {
            tenant: "acme".to_string(),
            fingerprint: [0xAB; FINGERPRINT_LEN],
        };
        let decoded = ClientIdentity::decode(&identity.encode()).expect("decode");
        assert_eq!(decoded, identity);
    }

    #[test]
    fn decode_rejects_truncated_payloads() {
        let identity = ClientIdentity {
            tenant: "acme".to_string(),
            fingerprint: [0xAB; FINGERPRINT_LEN],
        };
        let bytes = identity.encode();
        assert!(ClientIdentity::decode(&bytes[..3]).is_none());
        assert!(ClientIdentity::decode(&bytes[..bytes.len() - 1]).is_none());
    }
}
