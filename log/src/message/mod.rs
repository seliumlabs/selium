mod codec;
mod headers;
mod slice;

use bytes::{Buf, BufMut, Bytes, BytesMut};
pub use codec::LogCodec;
use crc32c::crc32c;
pub use headers::Headers;
pub use slice::MessageSlice;

pub const LEN_MARKER_SIZE: usize = 8;
pub const CRC_SIZE: usize = 4;
pub const HEADERS_SIZE: usize = 24;

#[derive(Debug, Clone)]
pub struct Message {
    headers: Headers,
    records: Bytes,
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

    pub fn decode(src: &mut BytesMut, length: u64) -> Self {
        let headers = Headers::decode(src, length);
        let records_byte_len = length as usize - HEADERS_SIZE - CRC_SIZE;
        let records = src.copy_to_bytes(records_byte_len);
        let _crc = src.get_u32();

        Self {
            headers,
            records,
            _crc,
        }
    }

    pub fn encode(&self, buffer: &mut BytesMut) {
        self.headers.encode(buffer);
        buffer.extend_from_slice(&self.records);
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
