use crate::traits::{MessageDecoder, MessageEncoder};
use anyhow::Result;
use bytes::{Buf, Bytes, BytesMut};
use serde::{de::DeserializeOwned, Serialize};
use std::marker::PhantomData;
use crate::traits::{Codec, SeliumCodec};

/// A basic codec that uses [bincode] to serialize and deserialize
/// binary message payloads.
#[derive(Debug, SeliumCodec)]
pub struct BincodeCodec<Item> {
    _marker: PhantomData<Item>,
}

impl<Item> Clone for BincodeCodec<Item> {
    fn clone(&self) -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

impl<T> Default for BincodeCodec<T> {
    fn default() -> Self {
        Self {
            _marker: PhantomData,
        }
    }
}

/// Encodes any `Item` implementing [Serialize](serde::Serialize) into a binary format via
/// [bincode].
///
/// # Errors
///
/// Returns [Err] if `item` fails to serialize.
impl<Item: Serialize> MessageEncoder<Item> for BincodeCodec<Item> {
    fn encode(&self, item: Item) -> Result<Bytes> {
        Ok(bincode::serialize(&item)?.into())
    }
}

/// Decodes a [BytesMut](bytes::BytesMut) payload into any `Item` implementing
/// [DeserializeOwned](serde::de::DeserializeOwned).
///
/// # Errors
///
/// Returns [Err] if the [BytesMut](bytes::BytesMut) payload fails to deserialize into `Item`.
impl<Item: DeserializeOwned> MessageDecoder<Item> for BincodeCodec<Item> {
    fn decode(&self, buffer: &mut BytesMut) -> Result<Item> {
        Ok(bincode::deserialize_from(buffer.reader())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Dummy {
        foo: String,
        bar: u64,
    }

    #[test]
    fn encodes_to_bincode_bytes() {
        let input = Dummy {
            foo: "foo".to_owned(),
            bar: 42,
        };

        let codec = BincodeCodec::default();
        let bytes = codec.encode(&input).unwrap();
        let expected = Bytes::from("\x03\0\0\0\0\0\0\0foo*\0\0\0\0\0\0\0");

        assert_eq!(expected, bytes);
    }

    #[test]
    fn decodes_bincode_bytes() {
        let mut buffer = BytesMut::from("\x03\0\0\0\0\0\0\0foo*\0\0\0\0\0\0\0");
        let decoder = BincodeCodec::<Dummy>::default();

        let expected = Dummy {
            foo: "foo".to_owned(),
            bar: 42,
        };

        let decoded = decoder.decode(&mut buffer).unwrap();

        assert_eq!(decoded, expected);
    }
}
