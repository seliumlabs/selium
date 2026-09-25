//! Multi-memory shared region header.
//!
//! A multi-memory region is a single shared memory allocation that contains
//! a small header followed by one or more sub-memories (e.g. RPC request and
//! reply rings). The header layout is:
//!
//! ```text
//! offset 0   8     16    24        24 + n*8
//!  ┌─────────┬─────────┬──────────┬──────────────┐
//!  │ magic   │ capacity │ count    │ entries[]   │
//!  │ u64     │ u64      │ u32      │ (u32, u32)* │
//!  └─────────┴─────────┴──────────┴──────────────┘
//! ```
//!
//! Each entry is `(offset: u32, length: u32)` packed into 8 bytes.

use crate::{MappingBackend, MemoryError, Result, SHARED_REGION_MAGIC};

/// Byte offset of the total capacity field (u64).
pub const HEADER_CAPACITY_OFFSET: u64 = 8;
/// Byte offset of the entry count field (u32).
pub const HEADER_COUNT_OFFSET: u64 = 16;
/// Byte offset where the entry table begins.
pub const HEADER_ENTRY_OFFSET: u64 = 24;
/// Size of a single entry in the entry table (offset: u32 + length: u32 = 8 bytes).
pub const HEADER_ENTRY_SIZE: u64 = 8;
/// Header size for two entries: magic + capacity + count + 2 entries (8 + 8 + 4 + 4 + 16 = 40).
pub const HEADER_SIZE_TWO_ENTRIES: u64 = HEADER_ENTRY_OFFSET + 2 * HEADER_ENTRY_SIZE;

/// A single entry in the multi-memory header: offset and length of a sub-memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultiMemoryEntry {
    /// Byte offset of the sub-memory within the parent region.
    pub offset: u64,
    /// Length of the sub-memory in bytes.
    pub length: u64,
}

/// Parsed multi-memory region header.
#[derive(Debug, Clone)]
pub struct MultiMemoryHeader {
    /// Total region capacity.
    pub capacity: u64,
    /// Number of sub-memory entries.
    pub count: u32,
    /// Sub-memory entries (offset, length pairs).
    pub entries: Vec<MultiMemoryEntry>,
}

impl MultiMemoryHeader {
    /// Writes a two-entry multi-memory header to the given backend at offset 0.
    ///
    /// The header is written starting at `base_offset` within the backend's
    /// address space.
    pub fn write_two_entries(
        backend: &dyn MappingBackend,
        base_offset: u64,
        capacity: u64,
        entries: [(u64, u64); 2],
    ) -> Result<()> {
        let off = base_offset;
        backend.write(off, &SHARED_REGION_MAGIC.to_le_bytes())?;
        backend.write(off + HEADER_CAPACITY_OFFSET, &capacity.to_le_bytes())?;
        backend.write(off + HEADER_COUNT_OFFSET, &2u32.to_le_bytes())?;
        for (i, entry) in entries.iter().enumerate() {
            let entry_off = off + HEADER_ENTRY_OFFSET + (i as u64) * HEADER_ENTRY_SIZE;
            backend.write(entry_off, &(entry.0 as u32).to_le_bytes())?;
            backend.write(entry_off + 4, &(entry.1 as u32).to_le_bytes())?;
        }
        Ok(())
    }

    /// Parses a multi-memory header from the given backend at offset 0.
    ///
    /// Validates the magic constant and entry count. Returns an error for
    /// invalid magic or malformed entries.
    pub fn parse(backend: &dyn MappingBackend, base_offset: u64) -> Result<Self> {
        let off = base_offset;

        let magic_bytes = backend.read(off, 8)?;
        let magic = u64::from_le_bytes(
            magic_bytes
                .try_into()
                .map_err(|_invalid_layout| MemoryError::InvalidLayout)?,
        );
        if magic != SHARED_REGION_MAGIC {
            return Err(MemoryError::InvalidLayout);
        }

        let cap_bytes = backend.read(off + HEADER_CAPACITY_OFFSET, 8)?;
        let capacity = u64::from_le_bytes(
            cap_bytes
                .try_into()
                .map_err(|_invalid_layout| MemoryError::InvalidLayout)?,
        );

        let count_bytes = backend.read(off + HEADER_COUNT_OFFSET, 4)?;
        let count = u32::from_le_bytes(
            count_bytes
                .try_into()
                .map_err(|_invalid_layout| MemoryError::InvalidLayout)?,
        );

        let mut entries = Vec::with_capacity(count as usize);
        for i in 0..count {
            let entry_off = off + HEADER_ENTRY_OFFSET + (i as u64) * HEADER_ENTRY_SIZE;
            let offset_bytes = backend.read(entry_off, 4)?;
            let length_bytes = backend.read(entry_off + 4, 4)?;
            let entry_offset = u32::from_le_bytes(
                offset_bytes
                    .try_into()
                    .map_err(|_invalid_layout| MemoryError::InvalidLayout)?,
            ) as u64;
            let entry_length = u32::from_le_bytes(
                length_bytes
                    .try_into()
                    .map_err(|_invalid_layout| MemoryError::InvalidLayout)?,
            ) as u64;
            entries.push(MultiMemoryEntry {
                offset: entry_offset,
                length: entry_length,
            });
        }

        Ok(Self {
            capacity,
            count,
            entries,
        })
    }

    /// Returns the sub-memory entry at the given index.
    pub fn entry(&self, index: u32) -> Result<&MultiMemoryEntry> {
        self.entries
            .get(index as usize)
            .ok_or(MemoryError::InvalidLayout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PointerBackend;

    fn backend(size: u64) -> PointerBackend {
        PointerBackend::allocate(size).expect("allocate")
    }

    #[test]
    fn write_and_parse_two_entries() {
        let b = backend(8192);
        let entries = [(1024u64, 2048u64), (3072u64, 2048u64)];
        MultiMemoryHeader::write_two_entries(&b, 0, 8192, entries).expect("write");

        let header = MultiMemoryHeader::parse(&b, 0).expect("parse");
        assert_eq!(header.capacity, 8192);
        assert_eq!(header.count, 2);
        assert_eq!(header.entries.len(), 2);
        assert_eq!(header.entries[0].offset, 1024);
        assert_eq!(header.entries[0].length, 2048);
        assert_eq!(header.entries[1].offset, 3072);
        assert_eq!(header.entries[1].length, 2048);
    }

    #[test]
    #[expect(
        clippy::assertions_on_result_states,
        reason = "unwrap_used lint conflicts with clippy's suggested fix"
    )]
    fn parse_rejects_bad_magic() {
        let b = backend(64);
        b.write(0, &[0u8; 8]).expect("write");
        assert!(MultiMemoryHeader::parse(&b, 0).is_err());
    }

    #[test]
    fn write_and_parse_at_offset() {
        let b = backend(8192);
        let base = 4096u64;
        let entries = [(128u64, 2048u64), (2176u64, 2048u64)];
        MultiMemoryHeader::write_two_entries(&b, base, 4096, entries).expect("write");

        let header = MultiMemoryHeader::parse(&b, base).expect("parse");
        assert_eq!(header.entries[0].offset, 128);
        assert_eq!(header.entries[1].offset, 2176);
    }
}
