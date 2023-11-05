mod client;
mod streams;

pub mod batching;
pub(crate) mod crypto;
pub mod prelude;
pub mod traits;
pub(crate) mod utils;

pub use client::*;
pub use streams::*;

#[cfg(any(
    feature = "std-compression",
    feature = "std-codec",
    feature = "std-traits"
))]
pub mod std {
    //! Re-exports [selium_std] modules.
    pub use selium_std::*;
}
