mod headers;
mod slice;

use std::mem::size_of;

use bytes::{BufMut, Bytes};
use crc32c::crc32c;
pub use headers::Headers;
pub use slice::MessageSlice;

pub const LEN_MARKER_SIZE: usize = size_of::<u64>();
pub const CRC_SIZE: usize = size_of::<u32>();
pub const HEADERS_SIZE: usize =
    size_of::<u64>() + size_of::<u32>() + size_of::<u32>() + size_of::<u64>();

#[derive(Debug, Clone)]
pub struct Message {
    headers: Headers,
    records: Bytes,
    // TODO: implement CRC check after replication is implemented.
    _crc: u32,
}

impl Message {
    pub fn new(headers: Headers, records: &[u8]) -> Self {
        let records = Bytes::copy_from_slice(records);

        Self {
            headers,
            records,
            _crc: 0,
        }
    }

    pub fn decode(headers: Headers, records: &[u8], crc: u32) -> Self {
        let records = Bytes::copy_from_slice(records);

        Self {
            headers,
            records,
            _crc: crc,
        }
    }

    pub fn encode(&self, buffer: &mut Vec<u8>) {
        self.headers.encode(buffer);
        buffer.put_slice(&self.records);
        let crc = crc32c(buffer);
        buffer.put_u32(crc);
    }

    pub fn records(&self) -> &[u8] {
        &self.records
    }

    pub fn headers(&self) -> &Headers {
        &self.headers
    }
}
