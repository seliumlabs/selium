//! A library containing standard offerings for Selium features, such as client codecs,
//! compression, and more.
//!
//! Selium Standard contains a rich selection of premade client codecs, compression
//! implementations, etc, which have been created by the `Selium Labs` team to make the
//! development experience as effortless as possible while using Selium.
//!
//! # Feature flags
//!
//! Do you only require a subset of features offered by Selium Standard? No worries! All offerings are
//! intended to be entirely optional, and can be included via the respective feature flag. This
//! in-turn will ensure that your binary is kept as slim as possible.
//!
//! For example, if a developer would like to use one of the many compression implementations offered by the
//! library, but uses their own proprietary codecs in-house, they can simply compile `selium-std` with the
//! `compression` feature flag, and then adapt their own proprietary codec to Selium's client codec
//! interface by implementing the respective MessageEncoder and MessageDecoder traits located in the
//! `traits/codec.rs` module.
//!
//! - `compression`: Enables all compression implementations.
//! - `codec`: Enables all client codec implementations.
//! - `traits`: Enables all traits. Enabled by default.
//! - `errors`: Enables all errors. Enabled by default.

#[cfg(feature = "codec")]
pub mod codecs;
#[cfg(feature = "compression")]
pub mod compression;

pub mod errors;
pub mod traits;
