//! Minimal in-tree vendoring of [`web-time`](https://crates.io/crates/web-time).
//!
//! Vendored and trimmed from upstream `web-time` (daxpedda/web-time, MIT OR
//! Apache-2.0). The upstream `wasm-bindgen` (JS) backend, `serde` support and
//! Web-atomics synchronization are omitted; `selium` only needs the
//! custom-time-source path.
//!
//! This crate keeps the `web-time` *name* on purpose: quinn and quinn-proto
//! `pub(crate) use web_time::{...}` directly on `wasm32-unknown-unknown`, so
//! the package name is part of their public ABI there and the code cannot be
//! absorbed into another crate (e.g. `selium-guest`).
//!
//! - On non-Wasm targets this crate is a pure [`std::time`] re-export, so
//!   native builds behave identically to upstream.
//! - On `wasm32-unknown-unknown` it provides [`Instant`], [`SystemTime`] and
//!   [`UNIX_EPOCH`] backed by a source registered with
//!   [`set_custom_time_source`] — the connector guests drive these from the
//!   Selium host time hostcalls.

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
pub use self::time::*;
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
pub use std::time::*;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
mod time;
