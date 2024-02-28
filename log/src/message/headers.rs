use super::{CRC_SIZE, HEADERS_SIZE};
use bytes::{Buf, BufMut, BytesMut};
use chrono::Utc;

#[derive(Debug)]
pub struct Headers {
    length: u64,
    version: u32,
    batch_size: u32,
    timestamp: u64,
}

impl Headers {
    pub fn new(batch_len: usize, version: u32, batch_size: u32) -> Self {
        let length = (batch_len + HEADERS_SIZE + CRC_SIZE) as u64;
        let timestamp = Utc::now().timestamp() as u64;

        Self {
            length,
            version,
            batch_size,
            timestamp,
        }
    }

    pub fn decode(src: &mut BytesMut, length: u64) -> Self {
        let version = src.get_u32();
        let batch_size = src.get_u32();
        let timestamp = src.get_u64();

        Self {
            length,
            version,
            batch_size,
            timestamp,
        }
    }

    pub fn encode(&self, buffer: &mut BytesMut) {
        buffer.put_u64(self.length);
        buffer.put_u32(self.version);
        buffer.put_u32(self.batch_size);
        buffer.put_u64(self.timestamp);
    }

    pub fn length(&self) -> u64 {
        self.length
    }

    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }

    pub fn batch_size(&self) -> u32 {
        self.batch_size
    }
}
