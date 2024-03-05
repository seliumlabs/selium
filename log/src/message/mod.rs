mod codec;
mod headers;
mod slice;

use bytes::{Buf, Bytes, BytesMut};
pub use codec::LogCodec;
pub use headers::Headers;
pub use slice::MessageSlice;

pub const LEN_MARKER_SIZE: usize = 8;
pub const HEADERS_SIZE: usize = 24;

#[derive(Debug, Clone)]
pub struct Message {
    headers: Headers,
    records: Bytes,
}

impl Message {
    pub fn new(headers: Headers, records: &[u8]) -> Self {
        let records = Bytes::copy_from_slice(records);
        Self { headers, records }
    }

    pub fn decode(src: &mut BytesMut, length: u64) -> Self {
        let headers = Headers::decode(src, length);
        let records_byte_len = length as usize - HEADERS_SIZE;
        let records = src.copy_to_bytes(records_byte_len);
        Self { headers, records }
    }

    pub fn encode(&self, buffer: &mut BytesMut) {
        self.headers.encode(buffer);
        buffer.extend_from_slice(&self.records);
    }

    pub fn records(&self) -> &[u8] {
        &self.records
    }

    pub fn headers(&self) -> &Headers {
        &self.headers
    }
}
