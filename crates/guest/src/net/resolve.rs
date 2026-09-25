//! Typed name resolution via the DNS connector.
//!
//! Resolving is a thin RPC client to the connector's self-registered
//! `dns/resolve` route: discovery lookup, one unary [`DnsQuery`] →
//! [`DnsResponse`] round-trip, and a typed outcome mapped to an address list
//! or error. No guest code touches the DNS wire format.

use std::net::IpAddr;

use selium_proto_dns::{DnsOutcome, DnsQuery, DnsRecordType, DnsResponse, RESOLVE_URI};
use selium_shm::rpc;

use crate::{Context, GuestError, Result, resource::ResourceSender};

/// Resolves `name` to IP addresses via the DNS connector.
///
/// # Errors
///
/// Returns an error when the connector is unavailable, discovery denies the
/// attach, or the outcome is NXDOMAIN, timeout, or truncation.
pub async fn resolve(ctx: &mut Context, name: &str) -> Result<Vec<IpAddr>> {
    let target = ctx.lookup(RESOLVE_URI).await?.ok_or_else(|| {
        GuestError::Host(format!("dns connector not registered at {RESOLVE_URI}"))
    })?;

    let sender = ResourceSender::attach(target.resource_id)?;

    let mut client = rpc::connect::<DnsQuery, DnsResponse, _>(sender, 0, 0)
        .await
        .map_err(|e| GuestError::Host(format!("connect to dns connector: {e}")))?;

    let response = client
        .request(DnsQuery::from_str(name, DnsRecordType::A))
        .await
        .map_err(|e| GuestError::Host(format!("dns request: {e}")))?;

    match response.outcome {
        DnsOutcome::Ok => parse_addresses(&response.addresses),
        DnsOutcome::NxDomain => Err(GuestError::Host(format!("name not found: {name}"))),
        DnsOutcome::Timeout => Err(GuestError::Host(format!("dns timeout for: {name}"))),
        DnsOutcome::Truncated => Err(GuestError::Host(format!(
            "dns response truncated for: {name}"
        ))),
        DnsOutcome::ServFail => Err(GuestError::Host(format!(
            "dns upstream server failure for: {name}"
        ))),
        DnsOutcome::Refused => Err(GuestError::Host(format!("dns query refused for: {name}"))),
        DnsOutcome::Upstream => Err(GuestError::Host(format!(
            "dns query could not be completed for: {name}"
        ))),
    }
}

fn parse_addresses(addresses: &[String]) -> Result<Vec<IpAddr>> {
    addresses
        .iter()
        .map(|address| {
            address.parse::<IpAddr>().map_err(|_error| {
                GuestError::Host(format!("dns connector returned invalid address: {address}"))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ipv4_and_ipv6_literals() {
        let addresses = vec![
            "93.184.216.34".to_string(),
            "2606:2800:220:1:248:1893:25c8:1946".to_string(),
        ];
        assert_eq!(
            parse_addresses(&addresses).expect("parse"),
            vec![
                "93.184.216.34".parse::<IpAddr>().unwrap(),
                "2606:2800:220:1:248:1893:25c8:1946"
                    .parse::<IpAddr>()
                    .unwrap(),
            ]
        );
    }

    #[test]
    fn rejects_invalid_address() {
        parse_addresses(&["not-an-ip".to_string()]).unwrap_err();
    }
}
