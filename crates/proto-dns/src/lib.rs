//! Selium DNS protocol wire types.
//!
//! Schema-backed FlatBuffers types for typed name resolution: a `DnsQuery`
//! carries a name and record type, a `DnsResponse` carries a typed outcome
//! and the resolved IP literals. The DNS connector uses these on its
//! well-known channel; the [`wire`] codec bridges them to real DNS messages
//! over UDP/53.

use selium_guest_macros::schema;

#[allow(warnings)]
#[rustfmt::skip]
pub mod fbs;
pub mod wire;

/// Well-known discovery URI registered by the DNS connector at boot, under
/// the root (empty) tenant.
///
/// Resolving guests attach by looking this URI up through discovery; a
/// channel grant on this URI is the capability that expresses "may resolve".
pub const RESOLVE_URI: &str = "sel:///dns/resolve";

/// DNS resource record types carried by typed queries and responses.
///
/// Values follow the IANA DNS RR TYPE registry (A = 1, CNAME = 5, AAAA = 28).
#[schema(
    path = "schemas/dns.fbs",
    ty = "selium.dns.DnsRecordType",
    binding = "fbs::selium::dns::DnsRecordType"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsRecordType {
    /// IPv4 address record.
    A,
    /// Canonical name alias record.
    Cname,
    /// IPv6 address record.
    Aaaa,
}

/// Honest resolution outcomes.
///
/// Upstream failure modes surface as distinct typed outcomes: the connector
/// never fabricates answers and never silently retries forever.
#[schema(
    path = "schemas/dns.fbs",
    ty = "selium.dns.DnsOutcome",
    binding = "fbs::selium::dns::DnsOutcome"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsOutcome {
    /// The name resolved; [`DnsResponse::addresses`] carries the answers.
    Ok,
    /// The upstream resolver answered NXDOMAIN.
    NxDomain,
    /// The upstream resolver did not answer within the timeout.
    Timeout,
    /// The upstream resolver answered with a truncated (TC) message.
    Truncated,
    /// The upstream resolver answered SERVFAIL.
    ServFail,
    /// The upstream resolver refused the query.
    Refused,
    /// The query could not be completed: the connector failed to forward it,
    /// the reply was undecodable, or the upstream returned an unhandled
    /// error code.
    Upstream,
}

/// A typed name-resolution query.
#[schema(
    path = "schemas/dns.fbs",
    ty = "selium.dns.DnsQuery",
    binding = "fbs::selium::dns::DnsQuery"
)]
#[derive(Debug, Clone, PartialEq)]
pub struct DnsQuery {
    /// The name to resolve.
    pub name: String,
    /// The record type to query for.
    pub record_type: DnsRecordType,
}

/// A typed name-resolution response carrying resolved IP literals.
#[schema(
    path = "schemas/dns.fbs",
    ty = "selium.dns.DnsResponse",
    binding = "fbs::selium::dns::DnsResponse"
)]
#[derive(Debug, Clone, PartialEq)]
pub struct DnsResponse {
    /// The resolution outcome.
    pub outcome: DnsOutcome,
    /// Resolved IP literals (empty for non-`Ok` outcomes).
    pub addresses: Vec<String>,
}

impl DnsQuery {
    /// Convenience constructor accepting `impl Into<String>` for the name.
    pub fn from_str(name: impl Into<String>, record_type: DnsRecordType) -> Self {
        Self::new(name.into(), record_type)
    }
}

impl DnsResponse {
    /// Builds a successful response from resolved IP literals.
    pub fn ok(addresses: Vec<String>) -> Self {
        Self::new(DnsOutcome::Ok, addresses)
    }

    /// Builds a failure response for a non-`Ok` outcome.
    pub fn failure(outcome: DnsOutcome) -> Self {
        Self::new(outcome, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_service::{FlatMsg, HasSchema};

    fn round_trip<T: FlatMsg + Clone + PartialEq + std::fmt::Debug>(value: &T) {
        let encoded = T::encode(value);
        let decoded = T::decode(&encoded).expect("decode should succeed");
        assert_eq!(value, &decoded, "round-trip mismatch");
    }

    #[test]
    fn dns_query_round_trip() {
        round_trip(&DnsQuery::from_str("example.com", DnsRecordType::A));
    }

    #[test]
    fn dns_query_aaaa_round_trip() {
        round_trip(&DnsQuery::from_str("example.com", DnsRecordType::Aaaa));
    }

    #[test]
    fn dns_response_ok_round_trip() {
        round_trip(&DnsResponse::ok(vec![
            "93.184.216.34".to_string(),
            "2606:2800:220:1:248:1893:25c8:1946".to_string(),
        ]));
    }

    #[test]
    fn dns_response_empty_round_trip() {
        round_trip(&DnsResponse::ok(vec![]));
    }

    #[test]
    fn dns_response_failures_round_trip() {
        for outcome in [
            DnsOutcome::NxDomain,
            DnsOutcome::Timeout,
            DnsOutcome::Truncated,
            DnsOutcome::ServFail,
            DnsOutcome::Refused,
            DnsOutcome::Upstream,
        ] {
            round_trip(&DnsResponse::failure(outcome));
        }
    }

    #[test]
    fn enum_from_flatbuffer_round_trips() {
        let mut builder = flatbuffers::FlatBufferBuilder::new();
        for record_type in [DnsRecordType::A, DnsRecordType::Cname, DnsRecordType::Aaaa] {
            let wire = record_type.write_flatbuffer(&mut builder);
            assert_eq!(DnsRecordType::from_flatbuffer(wire), record_type);
        }
    }

    #[test]
    fn has_schema_verification() {
        assert_eq!(DnsQuery::SCHEMA.fqname, "selium.dns.DnsQuery");
        assert_eq!(DnsResponse::SCHEMA.fqname, "selium.dns.DnsResponse");
        assert_ne!(DnsQuery::SCHEMA.hash, [0u8; 16]);
    }
}
