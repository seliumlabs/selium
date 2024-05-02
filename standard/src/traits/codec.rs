use anyhow::Result;
use bytes::{Bytes, BytesMut};

/// Provides an `encode` method for implementors to build their own encoder types.
///
/// See [codecs](crate::codecs) for more information.
pub trait MessageEncoder {
    type Item: Clone;

    fn encode(&self, item: Self::Item) -> Result<Bytes>;
}

/// Provides a `decode` method for implementors to build their own decoder types.
///
/// See [codecs](crate::codecs) for more information.
pub trait MessageDecoder {
    type Item;

    fn decode(&self, buffer: &mut BytesMut) -> Result<Self::Item>;
}
