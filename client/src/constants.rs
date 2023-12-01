/// The default `keep_alive` interval for a client connection.
pub const KEEP_ALIVE_DEFAULT: u64 = 5_000;

#[cfg(debug_assertions)]
pub(crate) const CLOUD_CA: &[u8; 455] = include_bytes!("../cloud.debug.der");
#[cfg(debug_assertions)]
pub(crate) const SELIUM_CLOUD_REMOTE_URL: &str = "127.0.0.1:4001";

#[cfg(not(debug_assertions))]
pub(crate) const CLOUD_CA: &[u8; 455] = include_bytes!("../cloud.prod.der");
#[cfg(not(debug_assertions))]
pub(crate) const SELIUM_CLOUD_REMOTE_URL: &str = "selium.com";
