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
#[derive(Debug, Clone, PartialEq, Eq)]
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
        let table_len = HEADER_ENTRY_OFFSET
            .checked_add(2 * HEADER_ENTRY_SIZE)
            .ok_or(MemoryError::InvalidLayout)?;
        if capacity < table_len {
            return Err(MemoryError::InvalidLayout);
        }
        for (entry_offset, entry_length) in entries {
            let end = entry_offset
                .checked_add(entry_length)
                .ok_or(MemoryError::InvalidLayout)?;
            if entry_offset < table_len || end > capacity {
                return Err(MemoryError::InvalidLayout);
            }
            // Entries are stored as `u32` offset/length pairs; refuse silent
            // truncation of a caller-supplied geometry.
            u32::try_from(entry_offset).map_err(|_error| MemoryError::InvalidLayout)?;
            u32::try_from(entry_length).map_err(|_error| MemoryError::InvalidLayout)?;
        }
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
    /// Validates the magic constant, bounds the entry count by the containing
    /// mapping, and validates the capacity and every entry range (including
    /// overlap with the header table and with other entries) before allocating
    /// the parsed entry table. Malformed input returns [`MemoryError::InvalidLayout`]
    /// without allocating from an untrusted count.
    pub fn parse(backend: &dyn MappingBackend, base_offset: u64) -> Result<Self> {
        let off = base_offset;
        let mapping_size = backend.size();

        let header_end = off
            .checked_add(HEADER_ENTRY_OFFSET)
            .ok_or(MemoryError::InvalidLayout)?;
        if header_end > mapping_size {
            return Err(MemoryError::InvalidLayout);
        }

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

        // Bound the declared count by the bytes actually present in the
        // mapping. This check happens before any allocation proportional to
        // `count`, so a hostile count cannot request a huge allocation.
        let available = mapping_size - header_end;
        let max_entries = available / HEADER_ENTRY_SIZE;
        if u64::from(count) > max_entries {
            return Err(MemoryError::InvalidLayout);
        }

        let table_len = HEADER_ENTRY_OFFSET
            .checked_add(u64::from(count) * HEADER_ENTRY_SIZE)
            .ok_or(MemoryError::InvalidLayout)?;
        let usable = mapping_size - off;
        if capacity < table_len || capacity > usable {
            return Err(MemoryError::InvalidLayout);
        }

        let mut entries = Vec::new();
        entries
            .try_reserve_exact(count as usize)
            .map_err(|_error| MemoryError::InvalidLayout)?;

        for i in 0..count {
            let entry_off = off
                .checked_add(HEADER_ENTRY_OFFSET)
                .and_then(|value| value.checked_add(u64::from(i) * HEADER_ENTRY_SIZE))
                .ok_or(MemoryError::InvalidLayout)?;
            let offset_bytes = backend.read(entry_off, 4)?;
            let length_bytes = backend.read(entry_off + 4, 4)?;
            let entry_offset = u64::from(u32::from_le_bytes(
                offset_bytes
                    .try_into()
                    .map_err(|_invalid_layout| MemoryError::InvalidLayout)?,
            ));
            let entry_length = u64::from(u32::from_le_bytes(
                length_bytes
                    .try_into()
                    .map_err(|_invalid_layout| MemoryError::InvalidLayout)?,
            ));
            let end = entry_offset
                .checked_add(entry_length)
                .ok_or(MemoryError::InvalidLayout)?;
            // Entries are relative to `base_offset`; they must follow the
            // header table and fit within the declared capacity.
            if entry_offset < table_len || end > capacity {
                return Err(MemoryError::InvalidLayout);
            }
            entries.push(MultiMemoryEntry {
                offset: entry_offset,
                length: entry_length,
            });
        }

        // Overlapping sub-memories would alias unrelated ring state.
        let mut spans: Vec<(u64, u64)> = entries
            .iter()
            .filter(|entry| entry.length > 0)
            .map(|entry| (entry.offset, entry.offset + entry.length))
            .collect();
        spans.sort_unstable_by_key(|span| span.0);
        for pair in spans.windows(2) {
            if let [first, second] = pair
                && first.1 > second.0
            {
                return Err(MemoryError::InvalidLayout);
            }
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

    // scenario: ring-protocol-core/single-ring-protocol-definition/multi-memory-header-parsed-identically
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
        let entries = [(128u64, 1024u64), (2048u64, 1024u64)];
        MultiMemoryHeader::write_two_entries(&b, base, 4096, entries).expect("write");

        let header = MultiMemoryHeader::parse(&b, base).expect("parse");
        assert_eq!(header.entries[0].offset, 128);
        assert_eq!(header.entries[1].offset, 2048);
    }

    /// Writes a header with an arbitrary (possibly hostile) count/entry table.
    fn write_raw_header(
        b: &dyn crate::MappingBackend,
        capacity: u64,
        count: u32,
        entries: &[(u32, u32)],
    ) {
        b.write(0, &SHARED_REGION_MAGIC.to_le_bytes())
            .expect("write magic");
        b.write(HEADER_CAPACITY_OFFSET, &capacity.to_le_bytes())
            .expect("write capacity");
        b.write(HEADER_COUNT_OFFSET, &count.to_le_bytes())
            .expect("write count");
        for (i, (offset, length)) in entries.iter().enumerate() {
            let entry_off = HEADER_ENTRY_OFFSET + (i as u64) * HEADER_ENTRY_SIZE;
            b.write(entry_off, &offset.to_le_bytes())
                .expect("write entry offset");
            b.write(entry_off + 4, &length.to_le_bytes())
                .expect("write entry length");
        }
    }

    // scenario: guest-shared-memory/multi-memory-layout-is-validated-before-allocation/hostile-entry-count
    #[test]
    fn parse_rejects_hostile_entry_count_without_allocating_from_it() {
        let b = backend(64);
        write_raw_header(&b, 64, u32::MAX, &[]);
        assert_eq!(
            MultiMemoryHeader::parse(&b, 0),
            Err(MemoryError::InvalidLayout)
        );
    }

    #[test]
    fn parse_rejects_entry_table_larger_than_mapping() {
        // A 64-byte mapping can hold at most (64 - 24) / 8 = 5 entries.
        let b = backend(64);
        write_raw_header(&b, 64, 6, &[]);
        assert_eq!(
            MultiMemoryHeader::parse(&b, 0),
            Err(MemoryError::InvalidLayout)
        );
    }

    #[test]
    fn parse_rejects_capacity_beyond_mapping() {
        let b = backend(4096);
        write_raw_header(&b, u64::MAX, 0, &[]);
        assert_eq!(
            MultiMemoryHeader::parse(&b, 0),
            Err(MemoryError::InvalidLayout)
        );
    }

    // scenario: guest-shared-memory/multi-memory-layout-is-validated-before-allocation/invalid-entry-geometry
    #[test]
    fn parse_rejects_entry_out_of_range() {
        let b = backend(8192);
        write_raw_header(&b, 4096, 2, &[(128, 1024), (3000, 2000)]);
        assert_eq!(
            MultiMemoryHeader::parse(&b, 0),
            Err(MemoryError::InvalidLayout)
        );
    }

    #[test]
    fn parse_rejects_entry_overlapping_header() {
        let b = backend(8192);
        write_raw_header(&b, 4096, 2, &[(0, 1024), (2048, 1024)]);
        assert_eq!(
            MultiMemoryHeader::parse(&b, 0),
            Err(MemoryError::InvalidLayout)
        );
    }

    #[test]
    fn parse_rejects_overlapping_entries() {
        let b = backend(8192);
        write_raw_header(&b, 4096, 2, &[(128, 1024), (512, 1024)]);
        assert_eq!(
            MultiMemoryHeader::parse(&b, 0),
            Err(MemoryError::InvalidLayout)
        );
    }

    #[test]
    fn parse_rejects_overflowing_entry_range() {
        let b = backend(8192);
        write_raw_header(&b, 8192, 1, &[(u32::MAX, u32::MAX)]);
        assert_eq!(
            MultiMemoryHeader::parse(&b, 0),
            Err(MemoryError::InvalidLayout)
        );
    }

    #[test]
    fn parse_accepts_adjacent_and_zero_length_entries() {
        let b = backend(8192);
        write_raw_header(&b, 4096, 3, &[(128, 1024), (1152, 1024), (3000, 0)]);
        let header = MultiMemoryHeader::parse(&b, 0).expect("adjacent entries are valid");
        assert_eq!(header.entries.len(), 3);
    }

    #[test]
    fn write_rejects_entry_geometry_that_would_truncate_or_overflow() {
        let b = backend(8192);
        assert_eq!(
            MultiMemoryHeader::write_two_entries(
                &b,
                0,
                4096,
                [(u64::from(u32::MAX) + 1, 8), (128, 1024)],
            ),
            Err(MemoryError::InvalidLayout)
        );
        assert_eq!(
            MultiMemoryHeader::write_two_entries(&b, 0, 4096, [(128, 4096), (4096, 0)]),
            Err(MemoryError::InvalidLayout)
        );
        assert_eq!(
            MultiMemoryHeader::write_two_entries(&b, 0, 4096, [(0, 128), (128, 128)]),
            Err(MemoryError::InvalidLayout)
        );
    }
}
