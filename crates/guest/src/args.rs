//! Entrypoint pointer-argument readers.
//!
//! A pointer argument is declared in an entrypoint as a `(u64, u64)` tuple
//! carrying `(address, length)`. The runtime wrote those bytes into this
//! guest's linear memory before invoking the entrypoint. These helpers
//! reconstruct a slice view over them so guest entrypoints do not repeat
//! unsafe slice construction.

/// Reconstructs the bytes of a pointer argument.
///
/// # Safety
/// `ptr` and `len` must describe bytes the runtime wrote into this guest's
/// linear memory for the current entrypoint invocation. Passing arbitrary
/// address/length pairs is undefined behaviour.
pub unsafe fn bytes(ptr: u64, len: u64) -> &'static [u8] {
    // SAFETY: upheld by the caller per the contract above.
    unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) }
}

/// Reconstructs a pointer argument as UTF-8, returning `None` for invalid
/// bytes.
///
/// # Safety
/// See [`bytes`].
pub unsafe fn str(ptr: u64, len: u64) -> Option<&'static str> {
    // SAFETY: delegated to `bytes`, whose contract the caller upholds.
    core::str::from_utf8(unsafe { bytes(ptr, len) }).ok()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    #[test]
    fn bytes_reconstructs_a_slice() {
        let payload = b"udp://127.0.0.1:53";
        let (ptr, len) = (payload.as_ptr() as u64, payload.len() as u64);
        // SAFETY: pointer/length describe valid, live bytes in this test.
        let slice = unsafe { super::bytes(ptr, len) };
        assert_eq!(slice, payload);
    }

    #[test]
    fn str_decodes_utf8() {
        let payload = "udp://127.0.0.1:53";
        let (ptr, len) = (payload.as_ptr() as u64, payload.len() as u64);
        // SAFETY: pointer/length describe valid, live bytes in this test.
        assert_eq!(unsafe { super::str(ptr, len) }, Some(payload));
    }
}
