#![no_main]

//! Fuzz frame header decoding and the canonical ring read path.
//!
//! A hostile header must produce an error, `None`, or a fully validated frame
//! — never a panic, out-of-bounds read, or allocation from an untrusted
//! length.

use libfuzzer_sys::fuzz_target;
use selium_memory::{FrameHeader, RegionMapping};

const CAPACITY: u64 = 4096;

fuzz_target!(|data: &[u8]| {
    let _ = FrameHeader::decode(data);

    let Ok(mapping) = RegionMapping::allocate(CAPACITY) else {
        return;
    };
    let bytes = &data[..data.len().min(CAPACITY as usize)];
    if mapping.write(0, bytes).is_err() {
        return;
    }
    let pos = u64::from(data.first().copied().unwrap_or(0)) % CAPACITY;
    let _ = selium_shm::layout::read_frame(mapping.backend(), pos, CAPACITY - 1, CAPACITY);
});
