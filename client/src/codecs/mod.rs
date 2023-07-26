#[cfg(feature = "bincode")]
mod bincode_codec;
mod string_codec;

#[cfg(feature = "bincode")]
pub use bincode_codec::*;

pub use string_codec::*;
