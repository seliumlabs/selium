//! Guest-facing helpers for working with Selium ABI metadata.
//!
//! # Examples
//! ```no_run
//! use selium_userland::abi::{GuestDecodeError, buffer_from_parts};
//!
//! fn main() -> Result<(), GuestDecodeError> {
//!     // A `(ptr, len)` of `(0, 0)` is treated as an empty slice.
//!     let bytes = unsafe { buffer_from_parts(0, 0)? };
//!     assert!(bytes.is_empty());
//!     Ok(())
//! }
//! ```

/// Re-export of Selium's shared ABI types for guest crates.
pub use selium_abi::*;

use core::{slice, str};
use thiserror::Error;

/// Errors surfaced when decoding pointers provided by the host.
#[derive(Debug, Error)]
pub enum GuestDecodeError {
    /// The host supplied an invalid or null pointer.
    #[error("invalid pointer provided by host")]
    InvalidPointer,
    /// The host supplied bytes that are not valid UTF-8.
    #[error("invalid UTF-8 data")]
    InvalidUtf8,
}

/// Convert a `(ptr, len)` pair (as produced by [`AbiParam::Buffer`]) into a byte slice.
///
/// # Safety
/// The caller must ensure that the lifetime of the returned slice does not outlive the
/// original allocation, and that the host-provided pointer is valid for `len` bytes.
pub unsafe fn buffer_from_parts<'a>(ptr: u32, len: u32) -> Result<&'a [u8], GuestDecodeError> {
    if ptr == 0 || len == 0 {
        return Ok(&[]);
    }

    let ptr = ptr as *const u8;
    if ptr.is_null() {
        return Err(GuestDecodeError::InvalidPointer);
    }

    Ok(unsafe { slice::from_raw_parts(ptr, len as usize) })
}

/// Convert a `(ptr, len)` pair into a UTF-8 string slice.
///
/// # Safety
/// Same requirements as [`buffer_from_parts`].
pub unsafe fn utf8_from_parts<'a>(ptr: u32, len: u32) -> Result<&'a str, GuestDecodeError> {
    let buf = unsafe { buffer_from_parts(ptr, len)? };
    str::from_utf8(buf).map_err(|_| GuestDecodeError::InvalidUtf8)
}
