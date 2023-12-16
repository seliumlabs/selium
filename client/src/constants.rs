//! Commonly used constants.

/// The default `keep_alive` interval for a client connection.
pub const KEEP_ALIVE_DEFAULT: u64 = 5_000;
/// The default `retention_policy` setting for messages.
pub const RETENTION_POLICY_DEFAULT: u64 = 0;

#[cfg(debug_assertions)]
pub(crate) const CLOUD_CA: &[u8; 469] = include_bytes!("../../proxy.debug.der");
#[cfg(debug_assertions)]
pub(crate) const SELIUM_CLOUD_REMOTE_URL: &str = "127.0.0.1:7002";

#[cfg(not(debug_assertions))]
pub(crate) const CLOUD_CA: &[u8; 469] = include_bytes!("../../proxy.prod.der");
#[cfg(not(debug_assertions))]
pub(crate) const SELIUM_CLOUD_REMOTE_URL: &str = "selium.io:7001";
