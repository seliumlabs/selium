/// Represents an entry in a corresponding index file.
#[derive(Debug)]
pub struct IndexEntry {
    /// The relative offset of the entry.
    /// The offset is relative to the base offset of the segment, increasing
    /// sequentially from 1.
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

    pub fn relative_offset(&self) -> u32 {
        self.relative_offset
    }

    pub fn timestamp(&self) -> u64 {
        self.timestamp
    }

    pub fn physical_position(&self) -> u64 {
        self.physical_position
    }
}
