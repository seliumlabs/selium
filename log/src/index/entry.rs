use bytes::{Buf, BufMut, Bytes, BytesMut};

pub const SIZE_OF_INDEX_ENTRY: usize = 20;

/// Represents an entry in a corresponding index file.
#[derive(Debug)]
pub struct IndexEntry {
    /// The relative offset of the entry.
    /// The offset is relative to the base offset of the segment, increasing
    /// sequentially from 0.
    relative_offset: u32,

    /// A UNIX timestamp corresponding to the time the entry was appended to the log.
    timestamp: u64,

    /// The physical position of the corresponding record in the log file.
    physical_position: u64,
}

impl IndexEntry {
    pub fn new(relative_offset: u32, timestamp: u64, physical_position: u64) -> Self {
        Self {
            relative_offset,
            timestamp,
            physical_position,
        }
    }

    pub fn from_slice(slice: &[u8]) -> Self {
        let mut slice = slice;
        let relative_offset = slice.get_u32();
        let timestamp = slice.get_u64();
        let physical_position = slice.get_u64();

        Self {
            relative_offset,
            timestamp,
            physical_position,
        }
    }

    pub fn into_slice(self) -> Bytes {
        let mut bytes = BytesMut::with_capacity(SIZE_OF_INDEX_ENTRY);
        bytes.put_u32(self.relative_offset);
        bytes.put_u64(self.timestamp);
        bytes.put_u64(self.physical_position);
        bytes.into()
    }

    pub fn relative_offset(&self) -> u32 {
        self.relative_offset
    }

    pub fn physical_position(&self) -> u64 {
        self.physical_position
    }
}
