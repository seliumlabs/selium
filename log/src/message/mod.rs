mod codec;
mod headers;

pub use codec::LogCodec;
pub use headers::Headers;

use bytes::{Buf, BufMut, Bytes, BytesMut};
use std::mem::size_of;

pub const LEN_MARKER_SIZE: usize = 8;
pub const CRC_SIZE: usize = 4;
pub const HEADERS_SIZE: usize = 24;

#[derive(Debug)]
pub struct Message {
    headers: Headers,
    records: Bytes,
    crc: u32,
}

impl Message {
    pub fn new(headers: Headers, records: &[u8]) -> Self {
        let records = Bytes::copy_from_slice(records);

        Self {
            headers,
            records,
            crc: 0,
        }
    }

    pub fn decode(src: &mut BytesMut, length: u64) -> Self {
        let headers = Headers::decode(src, length);
        let records_byte_len = length as usize - HEADERS_SIZE - CRC_SIZE;
        let records = src.copy_to_bytes(records_byte_len);
        let crc = src.get_u32();

        Self {
            headers,
            records,
            crc,
        }
    }

    pub fn encode(&self, buffer: &mut BytesMut) {
        self.headers.encode(buffer);
        buffer.extend_from_slice(&self.records);
        let crc = crc32c::crc32c(buffer);
        buffer.put_u32(crc);
    }

    pub fn records(&self) -> &[u8] {
        &self.records
    }

    pub fn headers(&self) -> &Headers {
        &self.headers
    }
}
