#![no_main]

//! Fuzz multi-memory header parsing.
//!
//! `parse` must bound the entry count, validate table and entry geometry, and
//! reject malformed input with `InvalidLayout` without allocating from the
//! declared count or creating sub-mappings.

use libfuzzer_sys::fuzz_target;
use selium_memory::{MultiMemoryHeader, RegionMapping};

const MAX_HEADER_BYTES: usize = 4096;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_HEADER_BYTES {
        return;
    }
    let size = (data.len() as u64).max(24);
    let Ok(mapping) = RegionMapping::allocate(size) else {
        return;
    };
    if mapping.write(0, data).is_err() {
        return;
    }
    let _ = MultiMemoryHeader::parse(mapping.backend(), 0);
});
