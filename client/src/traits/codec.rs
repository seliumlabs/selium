use anyhow::Result;
use bytes::{Bytes, BytesMut};

pub(crate) trait SeliumCodec {}

/// Provides an `encode` method for implementors to build their own encoder types.
///
/// See [codecs](crate::codecs) for more information.
pub trait MessageEncoder<Item> {
    fn encode(&self, item: Item) -> Result<Bytes>;
}

/// Provides a `decode` method for implementors to build their own decoder types.
///
/// See [codecs](crate::codecs) for more information.
pub trait MessageDecoder<T> {
    fn decode(&self, buffer: &mut BytesMut) -> Result<T>;
}
