use anyhow::Result;
use bytes::{Bytes, BytesMut};

/// Implicitly implemented by both `Selium` and custom codecs.
///
/// Do not implement this directly on your custom codec types. Derive the [CustomCodec]
/// trait instead.
pub trait Codec {
    fn name(&self) -> Option<String>;
}

pub(crate) trait SeliumCodec {}

/// Derived by custom client codecs. Implicitly implements the [Codec] trait.
///
/// See [codecs](crate::codecs) for more information.
pub trait CustomCodec {}

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
