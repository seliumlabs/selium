//! Guest-side helpers for building Selium userland services.
//!
//! Selium guests communicate exclusively via channels and typed hostcalls.
//!
//! # Examples
//! ```
//! use futures::{SinkExt, StreamExt};
//! use selium_userland::{entrypoint, io::DriverError};
//!
//! // Services can return results.
//! #[entrypoint]
//! pub async fn result_service() -> Result<(), DriverError> {
//!     Ok(())
//! }
//!
//! // You can also define services that do not have return values.
//! #[entrypoint]
//! pub async fn void_service() {
//!     ()
//! }
//! ```

// Appease #[schema] macro that can't find `selium_userland::` without it.
extern crate self as selium_userland;

macro_rules! driver_module {
    ($mod_name:ident, $import:ident, $import_module:literal) => {
        mod $mod_name {
            use selium_abi::{GuestInt, GuestUint};

            use crate::driver::DriverModule;

            #[allow(dead_code)]
            pub struct Module;

            #[cfg(target_arch = "wasm32")]
            #[link(wasm_import_module = $import_module)]
            unsafe extern "C" {
                pub fn create(args_ptr: GuestInt, args_len: GuestUint) -> GuestUint;
                pub fn poll(
                    handle: GuestUint,
                    task_id: GuestUint,
                    result_ptr: GuestInt,
                    result_len: GuestUint,
                ) -> GuestUint;
                pub fn drop(
                    handle: GuestUint,
                    result_ptr: GuestInt,
                    result_len: GuestUint,
                ) -> GuestUint;
            }

            #[allow(dead_code)]
            #[cfg(all(not(target_arch = "wasm32"), test))]
            unsafe fn create(args_ptr: GuestInt, args_len: GuestUint) -> GuestUint {
                crate::driver::test_driver::create(
                    selium_abi::hostcall_name!($import),
                    args_ptr,
                    args_len,
                )
            }

            #[allow(dead_code)]
            #[cfg(all(not(target_arch = "wasm32"), not(test)))]
            unsafe fn create(_args_ptr: GuestInt, _args_len: GuestUint) -> GuestUint {
                selium_abi::driver_encode_error(2)
            }

            #[allow(dead_code)]
            #[cfg(all(not(target_arch = "wasm32"), test))]
            unsafe fn poll(
                handle: GuestUint,
                task_id: GuestUint,
                result_ptr: GuestInt,
                result_len: GuestUint,
            ) -> GuestUint {
                crate::driver::test_driver::poll(
                    selium_abi::hostcall_name!($import),
                    handle,
                    task_id,
                    result_ptr,
                    result_len,
                )
            }

            #[allow(dead_code)]
            #[cfg(all(not(target_arch = "wasm32"), not(test)))]
            unsafe fn poll(
                _handle: GuestUint,
                _task_id: GuestUint,
                _result_ptr: GuestInt,
                _result_len: GuestUint,
            ) -> GuestUint {
                selium_abi::driver_encode_error(2)
            }

            #[allow(dead_code)]
            #[cfg(all(not(target_arch = "wasm32"), test))]
            unsafe fn drop(
                handle: GuestUint,
                result_ptr: GuestInt,
                result_len: GuestUint,
            ) -> GuestUint {
                crate::driver::test_driver::drop(
                    selium_abi::hostcall_name!($import),
                    handle,
                    result_ptr,
                    result_len,
                )
            }

            #[allow(dead_code)]
            #[cfg(all(not(target_arch = "wasm32"), not(test)))]
            unsafe fn drop(
                _handle: GuestUint,
                _result_ptr: GuestInt,
                _result_len: GuestUint,
            ) -> GuestUint {
                0
            }

            impl DriverModule for Module {
                unsafe fn create(args_ptr: GuestInt, args_len: GuestUint) -> GuestUint {
                    unsafe { create(args_ptr, args_len) }
                }

                unsafe fn poll(
                    handle: GuestUint,
                    task_id: GuestUint,
                    result_ptr: GuestInt,
                    result_len: GuestUint,
                ) -> GuestUint {
                    unsafe { poll(handle, task_id, result_ptr, result_len) }
                }

                unsafe fn drop(
                    handle: GuestUint,
                    result_ptr: GuestInt,
                    result_len: GuestUint,
                ) -> GuestUint {
                    unsafe { drop(handle, result_ptr, result_len) }
                }
            }
        }
    };
}

pub mod abi;
mod r#async;
pub mod context;
mod driver;
pub mod encoding;
/// Generated Flatbuffers schema bindings.
///
/// The types in this module are generated from Selium `.fbs` schema files and are primarily used
/// by higher-level helpers (for example, [`crate::encoding`]).
///
/// # Examples
/// ```
/// use selium_userland::encoding::{FlatMsg, FlatResult};
///
/// let bytes = FlatMsg::encode(&FlatResult::ok(1, Vec::new()));
/// assert!(selium_userland::fbs::selium::result::flat_result_buffer_has_identifier(&bytes));
/// ```
#[allow(missing_docs)]
#[allow(warnings)]
#[rustfmt::skip]
pub mod fbs;
pub mod io;
pub mod logging;
pub mod net;
pub mod process;
pub mod singleton;
pub mod time;

/// Re-export of the `rkyv` crate used for internal Selium serialisation.
pub use rkyv;

pub use r#async::{block_on, spawn, yield_now};
pub use context::{Context, Dependency, DependencyDescriptor};
/// Re-export of Selium's derive and attribute macros for guest crates.
pub use selium_userland_macros::*;

/// Re-export of singleton dependency identifiers.
pub use selium_abi::DependencyId;

pub trait FromHandle: Sized {
    /// Construct a typed wrapper from raw handle(s).
    ///
    /// # Safety
    /// The provided handles must have been minted by Selium for the current guest and must match
    /// the expected capability type. Supplying forged or stale handles may be rejected by the host
    /// kernel or result in undefined guest behaviour.
    unsafe fn from_handle(handles: Self::Handles) -> Self;
    /// The raw handle type(s) required by this wrapper.
    type Handles;
}
