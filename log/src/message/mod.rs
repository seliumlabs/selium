//! Message envelope/frame for a set of records.

mod headers;
mod slice;

use bytes::{BufMut, Bytes};
use crc32c::crc32c;
pub use headers::Headers;
pub use slice::MessageSlice;
use std::mem::size_of;

/// The byte length of the [Headers] length marker
pub const LEN_MARKER_SIZE: usize = size_of::<u64>();

/// The byte length of the CRC.
pub const CRC_SIZE: usize = size_of::<u32>();

/// The combined byte length of the message headers.
pub const HEADERS_SIZE: usize =
    size_of::<u64>() + size_of::<u32>() + size_of::<u32>() + size_of::<u64>();

/// The Message frame contains information required to parse the message, a calculated CRC used to
/// verify message integrity, and the encoded records.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    headers: Headers,
    records: Bytes,
    // TODO: implement CRC check after replication is implemented.
    _crc: u32,
}

impl Message {
    /// Constructs a Message instance with the provided [Headers], records
    /// batch and CRC.
    pub fn new(headers: Headers, records: &[u8], crc: u32) -> Self {
        let records = Bytes::copy_from_slice(records);

        Self {
            headers,
            records,
            _crc: crc,
        }
    }

    /// Shorthand method for constructing a Message instance with a batch
    /// size of 1.
    pub fn single(records: &[u8], version: u32) -> Self {
        let headers = Headers::new(records.len(), 1, version);
        Self::new(headers, records, 0)
    }

    /// Shorthand method for constructing a Message instance with a provided
    /// batch size.
    pub fn batch(records: &[u8], batch_size: u32, version: u32) -> Self {
        let headers = Headers::new(records.len(), batch_size, version);
        Self::new(headers, records, 0)
    }

    /// Encodes this Message instance into the provided buffer.
    pub fn encode(&self, buffer: &mut Vec<u8>) {
        self.headers.encode(buffer);
        buffer.put_slice(&self.records);
        let crc = crc32c(buffer);
        buffer.put_u32(crc);
    }

    /// The message headers containing information about the records batch.
    pub fn headers(&self) -> &Headers {
        &self.headers
    }

    /// The encoded records batch.
    pub fn records(&self) -> &[u8] {
        &self.records
    }
}
