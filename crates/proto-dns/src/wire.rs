//! DNS wire-format codec (RFC 1035).
//!
//! Bridges the schema types to real DNS messages exchanged over UDP/53:
//! [`encode_query`] turns a [`DnsQuery`] into a wire DNS question, and
//! [`parse_response`] turns an upstream DNS answer back into typed
//! addresses and a typed outcome. The DNS connector owns all wire traffic;
//! no other guest code touches this format.

// Parser offsets are bounds-checked against the buffer before slicing: every
// slice range here is verified present by `require` or an explicit length
// check first.
#![expect(
    clippy::indexing_slicing,
    reason = "parser offsets are bounds-checked before slicing"
)]

use std::{net::Ipv4Addr, net::Ipv6Addr};

use thiserror::Error;

use crate::{DnsOutcome, DnsQuery, DnsRecordType};

/// DNS class code for the Internet class.
const CLASS_IN: u16 = 1;
const FLAG_RD: u16 = 0x0100;
/// DNS flags bit fields.
const FLAG_TC: u16 = 0x0200;
/// Byte length of the fixed DNS message header.
const HEADER_LEN: usize = 12;
/// DNS response codes (IANA).
const RCODE_NOERROR: u8 = 0;
const RCODE_NXDOMAIN: u8 = 3;
const RCODE_REFUSED: u8 = 5;
const RCODE_SERVFAIL: u8 = 2;
/// DNS resource record type codes (IANA).
const TYPE_A: u16 = 1;
const TYPE_AAAA: u16 = 28;
const TYPE_CNAME: u16 = 5;

/// Errors produced by the wire codec.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum WireError {
    /// The message is shorter than its header, a record, or a length field.
    #[error("DNS message is shorter than expected")]
    Short,
    /// A name label exceeds 63 bytes, or the encoded name exceeds 255.
    #[error("DNS name label or name is too long")]
    NameTooLong,
    /// A name contains an empty label or unsupported label type.
    #[error("malformed DNS name")]
    Malformed,
    /// Name decompression exceeded the pointer-hopping guard.
    #[error("DNS name compression pointer loop")]
    CompressionLoop,
}

/// A parsed upstream DNS response, ready to be mapped to a [`crate::DnsResponse`].
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedResponse {
    /// The transaction id echoed by the resolver (used for correlation).
    pub txid: u16,
    /// The typed outcome derived from the header.
    pub outcome: DnsOutcome,
    /// A/AAAA addresses carried by the answer section.
    pub addresses: Vec<String>,
}

impl DnsRecordType {
    /// Maps a record type to its IANA RR TYPE code.
    pub fn as_wire(self) -> u16 {
        match self {
            DnsRecordType::A => TYPE_A,
            DnsRecordType::Cname => TYPE_CNAME,
            DnsRecordType::Aaaa => TYPE_AAAA,
        }
    }
}

/// Encodes a [`DnsQuery`] into a complete wire DNS query message.
///
/// The query is built with the recursion-desired flag set, a single
/// question, and no answer/authority/additional records.
pub fn encode_query(query: &DnsQuery, txid: u16) -> Result<Vec<u8>, WireError> {
    let mut out = Vec::with_capacity(HEADER_LEN + 1 + query.name.len() + 6);

    // Header: id, flags (RD), qdcount = 1, ancount/ns/ar = 0.
    out.extend_from_slice(&txid.to_be_bytes());
    out.extend_from_slice(&FLAG_RD.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // ancount, nscount, arcount

    // Question section.
    encode_name(&query.name, &mut out)?;
    out.extend_from_slice(&query.record_type.as_wire().to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());

    Ok(out)
}

/// Parses an upstream DNS response message.
pub fn parse_response(bytes: &[u8]) -> Result<ParsedResponse, WireError> {
    if bytes.len() < HEADER_LEN {
        return Err(WireError::Short);
    }

    let txid = u16::from_be_bytes([bytes[0], bytes[1]]);
    let flags = u16::from_be_bytes([bytes[2], bytes[3]]);
    let qdcount = u16::from_be_bytes([bytes[4], bytes[5]]) as usize;
    let ancount = u16::from_be_bytes([bytes[6], bytes[7]]) as usize;

    let outcome = if flags & FLAG_TC != 0 {
        DnsOutcome::Truncated
    } else {
        match rcode(flags) {
            RCODE_NOERROR => DnsOutcome::Ok,
            RCODE_SERVFAIL => DnsOutcome::ServFail,
            RCODE_NXDOMAIN => DnsOutcome::NxDomain,
            RCODE_REFUSED => DnsOutcome::Refused,
            // FORMERR, NOTIMP, and any other code: an upstream error the
            // type system does not model explicitly.
            _ => DnsOutcome::Upstream,
        }
    };

    // Skip the question section.
    let mut pos = HEADER_LEN;
    for _ in 0..qdcount {
        let (_name, end) = parse_name(bytes, pos)?;
        pos = end.checked_add(4).ok_or(WireError::Short)?;
        if pos > bytes.len() {
            return Err(WireError::Short);
        }
    }

    // Parse the answer section, collecting A/AAAA literals.
    let mut addresses = Vec::new();
    for _ in 0..ancount {
        let (_name, end) = parse_name(bytes, pos)?;
        pos = end;
        pos = require(bytes, pos, 10)?; // type(2) + class(2) + ttl(4) + rdlength(2)
        let rtype = u16::from_be_bytes([bytes[pos - 10], bytes[pos - 9]]);
        let rdlength = u16::from_be_bytes([bytes[pos - 2], bytes[pos - 1]]) as usize;
        let rdata_start = pos;
        let rdata_end = pos.checked_add(rdlength).ok_or(WireError::Short)?;
        if rdata_end > bytes.len() {
            return Err(WireError::Short);
        }

        match rtype {
            TYPE_A if rdlength == 4 => {
                let ip = Ipv4Addr::new(
                    bytes[rdata_start],
                    bytes[rdata_start + 1],
                    bytes[rdata_start + 2],
                    bytes[rdata_start + 3],
                );
                addresses.push(ip.to_string());
            }
            TYPE_AAAA if rdlength == 16 => {
                let mut octets = [0u8; 16];
                octets.copy_from_slice(&bytes[rdata_start..rdata_start + 16]);
                addresses.push(Ipv6Addr::from(octets).to_string());
            }
            _ => {}
        }

        pos = rdata_end;
    }

    Ok(ParsedResponse {
        txid,
        outcome,
        addresses,
    })
}

/// Encodes a domain name as a sequence of length-prefixed labels followed
/// by a zero terminator.
fn encode_name(name: &str, out: &mut Vec<u8>) -> Result<(), WireError> {
    let name = name.trim_end_matches('.');
    if name.is_empty() {
        out.push(0);
        return Ok(());
    }

    let mut written = 0usize;
    for label in name.split('.') {
        if label.is_empty() {
            return Err(WireError::Malformed);
        }
        if label.len() > 63 {
            return Err(WireError::NameTooLong);
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
        written += 1 + label.len();
    }
    if written >= 255 {
        return Err(WireError::NameTooLong);
    }
    out.push(0);
    Ok(())
}

/// Decodes a domain name, following compression pointers.
///
/// Returns the decoded name and the position just past the name encoding in
/// the original message (the pointer itself when a compression pointer is
/// followed).
fn parse_name(bytes: &[u8], start: usize) -> Result<(String, usize), WireError> {
    let mut labels: Vec<String> = Vec::new();
    let mut pos = start;
    let mut end = start;
    let mut jumped = false;
    let mut jumps = 0usize;

    loop {
        if pos >= bytes.len() {
            return Err(WireError::Short);
        }
        let length = bytes[pos];
        match length {
            0 => {
                if !jumped {
                    end = pos + 1;
                }
                break;
            }
            0b1100_0000..=0b1111_1111 => {
                // Compression pointer.
                let next = *bytes.get(pos + 1).ok_or(WireError::Short)?;
                let pointer = ((length as usize & 0x3F) << 8) | next as usize;
                if !jumped {
                    end = pos + 2;
                }
                pos = pointer;
                jumped = true;
                jumps += 1;
                if jumps > 32 {
                    return Err(WireError::CompressionLoop);
                }
            }
            0b0100_0000..=0b1011_1111 => {
                return Err(WireError::Malformed);
            }
            _ => {
                let label_len = length as usize;
                let label_start = pos + 1;
                let label_end = require(bytes, label_start, label_len)?;
                let label = std::str::from_utf8(&bytes[label_start..label_end])
                    .map_err(|_error| WireError::Malformed)?;
                labels.push(label.to_string());
                pos = label_end;
            }
        }
    }

    Ok((labels.join("."), end))
}

/// Extracts the 4-bit response code from the flags field.
fn rcode(flags: u16) -> u8 {
    (flags & 0x000F) as u8
}

/// Ensures `len` more bytes are available at `pos`, returning the advanced
/// position.
fn require(bytes: &[u8], pos: usize, len: usize) -> Result<usize, WireError> {
    let end = pos.checked_add(len).ok_or(WireError::Short)?;
    if end > bytes.len() {
        return Err(WireError::Short);
    }
    Ok(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A known-good DNS query for `example.com` A with id 0x1234.
    ///
    /// Header: id=0x1234, flags=RD, qdcount=1, counts=0, followed by the
    /// question `example.com IN A`.
    const EXAMPLE_COM_A_QUERY: &[u8] = &[
        0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, b'e', b'x',
        b'a', b'm', b'p', b'l', b'e', 0x03, b'c', b'o', b'm', 0x00, 0x00, 0x01, 0x00, 0x01,
    ];

    /// A known-good DNS response for `example.com` A = 93.184.216.34.
    ///
    /// id=0x1234, flags=QR|RD|RA, rcode=0, qdcount=1, ancount=1. The answer
    /// name is a compression pointer to the question name, with ttl 60.
    const EXAMPLE_COM_A_RESPONSE: &[u8] = &[
        0x12, 0x34, 0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x07, b'e', b'x',
        b'a', b'm', b'p', b'l', b'e', 0x03, b'c', b'o', b'm', 0x00, 0x00, 0x01, 0x00, 0x01, 0xC0,
        0x0C, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3C, 0x00, 0x04, 0x5D, 0xB8, 0xD8, 0x22,
    ];

    fn question_section() -> &'static [u8] {
        &EXAMPLE_COM_A_QUERY[12..]
    }

    #[test]
    fn encode_query_matches_known_good_packet() {
        let query = DnsQuery::from_str("example.com", DnsRecordType::A);
        let encoded = encode_query(&query, 0x1234).expect("encode query");
        assert_eq!(encoded, EXAMPLE_COM_A_QUERY);
    }

    #[test]
    fn encode_query_aaaa_selects_record_type() {
        let query = DnsQuery::from_str("example.com", DnsRecordType::Aaaa);
        let encoded = encode_query(&query, 0x0001).expect("encode query");
        // The qtype field (two bytes before the end) is AAAA = 28.
        let len = encoded.len();
        assert_eq!(&encoded[len - 4..len - 2], &TYPE_AAAA.to_be_bytes());
        assert_eq!(&encoded[len - 2..], &CLASS_IN.to_be_bytes());
    }

    #[test]
    fn parse_known_good_response() {
        let parsed = parse_response(EXAMPLE_COM_A_RESPONSE).expect("parse response");
        assert_eq!(parsed.txid, 0x1234);
        assert_eq!(parsed.outcome, DnsOutcome::Ok);
        assert_eq!(parsed.addresses, vec!["93.184.216.34".to_string()]);
    }

    #[test]
    fn parse_nxdomain_response() {
        // Same header/question, but rcode=3 (NXDOMAIN) and no answers.
        let mut packet = EXAMPLE_COM_A_QUERY.to_vec();
        packet[2] = 0x81; // QR|RD
        packet[3] = 0x83; // RA + rcode 3
        packet[7] = 0x00; // ancount = 0

        let parsed = parse_response(&packet).expect("parse response");
        assert_eq!(parsed.outcome, DnsOutcome::NxDomain);
        assert!(parsed.addresses.is_empty());
    }

    #[test]
    fn parse_truncated_response() {
        // TC bit set → Truncated outcome regardless of answers.
        let mut packet = EXAMPLE_COM_A_QUERY.to_vec();
        packet[2] = 0x83; // QR|RD|TC
        packet[3] = 0x80; // RA

        let parsed = parse_response(&packet).expect("parse response");
        assert_eq!(parsed.outcome, DnsOutcome::Truncated);
    }

    #[test]
    fn parse_servfail_response() {
        // rcode=2 (SERVFAIL) → distinct typed outcome, not Ok.
        let mut packet = EXAMPLE_COM_A_QUERY.to_vec();
        packet[2] = 0x81; // QR|RD
        packet[3] = 0x82; // RA + rcode 2
        packet[7] = 0x00; // ancount = 0

        let parsed = parse_response(&packet).expect("parse response");
        assert_eq!(parsed.outcome, DnsOutcome::ServFail);
    }

    #[test]
    fn parse_refused_response() {
        // rcode=5 (REFUSED) → distinct typed outcome, not Ok.
        let mut packet = EXAMPLE_COM_A_QUERY.to_vec();
        packet[2] = 0x81; // QR|RD
        packet[3] = 0x85; // RA + rcode 5
        packet[7] = 0x00; // ancount = 0

        let parsed = parse_response(&packet).expect("parse response");
        assert_eq!(parsed.outcome, DnsOutcome::Refused);
    }

    #[test]
    fn parse_unhandled_rcode_maps_to_upstream() {
        // rcode=4 (NOTIMP) is not modelled explicitly → Upstream, never Ok.
        let mut packet = EXAMPLE_COM_A_QUERY.to_vec();
        packet[2] = 0x81; // QR|RD
        packet[3] = 0x84; // RA + rcode 4
        packet[7] = 0x00; // ancount = 0

        let parsed = parse_response(&packet).expect("parse response");
        assert_eq!(parsed.outcome, DnsOutcome::Upstream);
    }

    #[test]
    fn parse_aaaa_response() {
        // id=0x0001, QR, ancount=1, answer is a literal AAAA 2001:db8::1.
        let mut packet = vec![
            0x00, 0x01, 0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        ];
        packet.extend_from_slice(question_section());
        packet.extend_from_slice(&[0xC0, 0x0C]); // name pointer
        packet.extend_from_slice(&TYPE_AAAA.to_be_bytes());
        packet.extend_from_slice(&CLASS_IN.to_be_bytes());
        packet.extend_from_slice(&[0x00, 0x00, 0x00, 0x3C]); // ttl
        packet.extend_from_slice(&16u16.to_be_bytes()); // rdlength
        packet.extend_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

        let parsed = parse_response(&packet).expect("parse response");
        assert_eq!(parsed.outcome, DnsOutcome::Ok);
        assert_eq!(parsed.addresses, vec!["2001:db8::1".to_string()]);
    }

    #[test]
    fn parse_short_packet_is_error() {
        assert!(matches!(
            parse_response(&[0x12, 0x34, 0x81]),
            Err(WireError::Short)
        ));
    }

    #[test]
    fn encode_query_rejects_oversized_label() {
        let long_label = "a".repeat(64);
        let query = DnsQuery::from_str(format!("{long_label}.com"), DnsRecordType::A);
        assert!(matches!(
            encode_query(&query, 1),
            Err(WireError::NameTooLong)
        ));
    }

    #[test]
    fn round_trip_through_typed_surface() {
        // The wire codec feeds the typed surface: parse bytes, then construct
        // the schema type the connector publishes on its channel.
        let parsed = parse_response(EXAMPLE_COM_A_RESPONSE).expect("parse");
        let response = match parsed.outcome {
            DnsOutcome::Ok => crate::DnsResponse::ok(parsed.addresses),
            other => crate::DnsResponse::failure(other),
        };
        assert_eq!(response.outcome, DnsOutcome::Ok);
        assert_eq!(response.addresses, vec!["93.184.216.34".to_string()]);
    }
}
