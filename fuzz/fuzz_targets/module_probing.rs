#![no_main]

//! Fuzz guest-module loading and instantiation.
//!
//! Arbitrary bytes must be rejected by the engine's validation or instantiate
//! without panicking; a malformed module must never abort the host.

use libfuzzer_sys::fuzz_target;
use wasmtiny::WasmApplication;

const MAX_MODULE_BYTES: usize = 256 * 1024;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_MODULE_BYTES {
        return;
    }
    let mut app = WasmApplication::new();
    if let Ok(index) = app.load_module_from_memory(data) {
        let _ = app.instantiate(index);
    }
});
