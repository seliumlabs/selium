use bytes::{Buf, BufMut, Bytes, BytesMut};

pub const SIZE_OF_INDEX_ENTRY: usize =
    std::mem::size_of::<u32>() + std::mem::size_of::<u64>() + std::mem::size_of::<u64>();

/// Represents an entry in a corresponding index file.
#[derive(Debug)]
pub struct IndexEntry {
    relative_offset: u32,
    timestamp: u64,
    physical_position: u64,
}

impl IndexEntry {
    /// Constructs a new IndexEntry instance.
    pub fn new(relative_offset: u32, timestamp: u64, physical_position: u64) -> Self {
        Self {
            relative_offset,
            timestamp,
            physical_position,
        }
    }

    /// Constructs an IndexEntry instance from the provided slice of bytes.
    ///
    /// # Panics
    /// This method will panic if the provided slice is not large enough to
    /// fit the size of an index entry.
    pub fn from_slice(slice: &[u8]) -> Self {
        let mut slice = slice;
        let relative_offset = slice.get_u32();
        let timestamp = slice.get_u64();
        let physical_position = slice.get_u64();
        Self::new(relative_offset, timestamp, physical_position)
    }

    /// Takes ownership of the current IndexEntry, and converts it to an instance of [Bytes].
    pub fn into_slice(self) -> Bytes {
        let mut bytes = BytesMut::with_capacity(SIZE_OF_INDEX_ENTRY);
        bytes.put_u32(self.relative_offset);
        bytes.put_u64(self.timestamp);
        bytes.put_u64(self.physical_position);
        bytes.into()
    }

    /// The relative offset of the entry.
    /// The offset is relative to the base offset of the segment, increasing sequentially from 1.
    pub fn relative_offset(&self) -> u32 {
        self.relative_offset
    }

    /// The physical position of the corresponding record in the log file.
    pub fn physical_position(&self) -> u64 {
        self.physical_position
    }
}
