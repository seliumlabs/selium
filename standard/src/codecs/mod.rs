//! Client codec implementations for commonly used serialization formats, including UTF-8 encoded
//! strings, and various binary formats, such as bincode.
//!
//! In `Selium`, messages are sent over the wire in a binary format, and thus, the server has no
//! indication of, or any desire to make sense of the data. This is perfectly suitable for the
//! server, but not very convenient for client users.
//!
//! To alleviate this issue, the `Selium` library makes use of codecs to encode produced messages
//! into bytes that the server can work with, and decode messages into a format that is useful to
//! consumers.
//!
//! Codecs are almost never used directly, but are provided as a configuration option while constructing
//! a `Subscriber` or `Publisher` stream. The `Subscriber` or `Publisher` will then call the underlying
//! [encode](crate::traits::codec::MessageEncoder::encode) or [decode](crate::traits::codec::MessageDecoder::decode)
//! method in their respective `Sink` or `Stream` implementations when producing or consuming messages.
//!
//! # Encoder
//!
//! An `Encoder` is used to encode an input into a sequence of bytes before being transmitted
//! over the wire.
//!
//! ### The MessageEncoder Trait
//!
//! The [MessageEncoder](crate::traits::codec::MessageEncoder) trait is responsible for specifying the process of
//! receiving a generic input of type `Item`, and condensing it down into a [BytesMut](bytes::BytesMut)
//! value.
//!
//! [MessageEncoder](crate::traits::codec::MessageEncoder) exposes a single method to implementors,
//! [encode](crate::traits::codec::MessageEncoder::encode).
//!
//! # Decoder
//!
//! A `Decoder` is used to decode a sequence of bytes received over the wire into the target `Item`
//! type.
//!
//! ### The MessageDecoder Trait
//!
//! The [MessageDecoder](crate::traits::codec::MessageDecoder) trait is responsible for specifying the
//! process of converting a [BytesMut](bytes::BytesMut) value into the target `Item` type.
//!
//! [MessageDecoder](crate::traits::codec::MessageDecoder) exposes a single method to implementors,
//! [decode](crate::traits::codec::MessageDecoder::decode).
//!
//! # Custom Codecs
//!
//! `Selium` aims to provide a suitable collection of codecs for various message payload formats,
//! including UTF-8 [String] encoding/decoding, various [serde] binary serialization formats, such
//! as [bincode], and many others.
//!
//! However, when the provided codecs are either not suitable for your needs, or lack support for
//! a specific serialization format, it is trivial to create a custom codec via the
//! [MessageEncoder](crate::traits::codec::MessageEncoder) and
//! [MessageDecoder](crate::traits::codec::MessageDecoder) traits.
//!
//! ## Example
//!
//! To give a contrived example, let's create a codec called `ColorCodec`, which will encode and
//! decode a [tuple] containing three [u8] values to describe a color.
//!
//! To begin, we'll create a struct called `ColorCodec`, and derive the [Default] and [Clone]
//! traits.
//!
//! ```
//! #[derive(Default, Clone)]
//! pub struct ColorCodec;
//! ```
//!
//! Next, we will implement the [MessageEncoder](crate::traits::codec::MessageEncoder) and
//! [MessageDecoder](crate::traits::codec::MessageDecoder) traits for our `ColorCodec` struct,
//! Let's leave them unimplemented for now.
//!
//! For the sake of convenience, a type alias `Color` has been defined to give meaning to the
//! unnamed [tuple].
//!
//! ```
//! # #[derive(Default, Clone)]
//! # pub struct ColorCodec;
//! use anyhow::Result;
//! use selium_std::traits::codec::{MessageEncoder, MessageDecoder};
//! use bytes::{Bytes, BytesMut};
//!
//! type Color = (u8, u8, u8);
//!
//! impl MessageEncoder for ColorCodec {
//!     type Item = Color;
//!
//!     fn encode(&self, item: Color) -> Result<Bytes> {
//!         unimplemented!()
//!     }
//! }
//!
//! impl MessageDecoder for ColorCodec {
//!     type Item = Color;
//!
//!     fn decode(&self, buffer: &mut BytesMut) -> Result<Self::Item> {
//!         unimplemented!()
//!     }
//! }
//! ```
//!
//! Now we can finish the implementations of the [encode](crate::traits::codec::MessageEncoder::encode) and
//! [decode](crate::traits::codec::MessageEncoder::encode) methods.
//!
//! Starting with the [encode](crate::traits::codec::MessageEncoder::encode) method, we can push each element in
//! the tuple onto a [BytesMut](bytes::BytesMut) buffer. Be sure to reserve enough space prior to this operation.
//!
//! ```
//! # use anyhow::Result;
//! # use selium_std::traits::codec::MessageEncoder;
//! # use bytes::{Bytes, BytesMut, BufMut};
//! # #[derive(Default, Clone)]
//! # pub struct ColorCodec;
//! # type Color = (u8, u8, u8);
//! impl MessageEncoder for ColorCodec {
//!     type Item = Color;
//!
//!     fn encode(&self, (r, g, b): Color) -> Result<Bytes> {
//!         let mut buffer = BytesMut::with_capacity(3);
//!
//!         buffer.put_u8(r);
//!         buffer.put_u8(g);
//!         buffer.put_u8(b);
//!
//!         Ok(buffer.into())
//!     }
//! }
//! ```
//!
//! Finally, we can complete the [decode](crate::traits::codec::MessageDecoder::decode) method by popping three
//! [u8] values from the [BytesMut](bytes::BytesMut) buffer, and reconstructing the tuple.
//!
//! ```
//! # use anyhow::Result;
//! # use selium_std::traits::codec::MessageDecoder;
//! # use bytes::{Buf, BytesMut};
//! # #[derive(Default, Clone)]
//! # pub struct ColorCodec;
//! # type Color = (u8, u8, u8);
//! impl MessageDecoder for ColorCodec {
//!     type Item = Color;
//!
//!     fn decode(&self, buffer: &mut BytesMut) -> Result<Self::Item> {
//!         let r = buffer.get_u8();
//!         let g = buffer.get_u8();
//!         let b = buffer.get_u8();
//!
//!         Ok((r, g, b))
//!     }
//! }
//! ```
//!
//! Putting it all together, we have the following code, showing just how simple it is to create a
//! customized codec to suit your specific message format.
//!
//! ```
//! use anyhow::Result;
//! use selium_std::traits::codec::{MessageEncoder, MessageDecoder};
//! use bytes::{Bytes, BytesMut, Buf, BufMut};
//!
//! #[derive(Default, Clone)]
//! pub struct ColorCodec;
//!
//! type Color = (u8, u8, u8);
//!
//! impl MessageEncoder for ColorCodec {
//!     type Item = Color;
//!
//!     fn encode(&self, (r, g, b): Color) -> Result<Bytes> {
//!         let mut buffer = BytesMut::with_capacity(3);
//!
//!         buffer.put_u8(r);
//!         buffer.put_u8(g);
//!         buffer.put_u8(b);
//!
//!         Ok(buffer.into())
//!     }
//! }
//!
//! impl MessageDecoder for ColorCodec {
//!     type Item = Color;
//!
//!     fn decode(&self, buffer: &mut BytesMut) -> Result<Self::Item> {
//!         let r = buffer.get_u8();
//!         let g = buffer.get_u8();
//!         let b = buffer.get_u8();
//!
//!         Ok((r, g, b))
//!     }
//! }
//! ```

mod bincode_codec;
mod bytes_codec;
mod string_codec;

pub use bincode_codec::*;
pub use bytes_codec::*;
pub use string_codec::*;
