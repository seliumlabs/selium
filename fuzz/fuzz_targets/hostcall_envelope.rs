#![no_main]

//! Fuzz the hostcall envelope decoder with untrusted bytes.
//!
//! The host bounds the declared length before decoding; this target exercises
//! the decoder itself over every declared payload the host would accept. Any
//! panic, unbounded allocation, or undefined behaviour is a defect.

use libfuzzer_sys::fuzz_target;
use selium_abi::{HOSTCALL_ENVELOPE_MAX_BYTES, HostcallEnvelope};

fuzz_target!(|data: &[u8]| {
    if data.len() > HOSTCALL_ENVELOPE_MAX_BYTES {
        return;
    }
    let _ = selium_abi::decode_rkyv::<HostcallEnvelope>(data);
});
