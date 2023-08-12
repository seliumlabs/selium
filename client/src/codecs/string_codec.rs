use crate::traits::{MessageDecoder, MessageEncoder, SeliumCodec};
use anyhow::Result;
use bytes::{Bytes, BytesMut};

/// A basic codec for encoding/decoding UTF-8 [String] message payloads.
#[derive(Default, Clone)]
pub struct StringCodec;

/// Encodes a [&str] slice into [Bytes](bytes::Bytes).
impl MessageEncoder<&str> for StringCodec {
    fn encode(&self, item: &str) -> Result<Bytes> {
        Ok(item.to_owned().into())
    }
}

/// Decodes a [BytesMut](bytes::BytesMut) payload into an owned [String]
///
/// # Errors
///
/// Returns [Err] if a valid UTF-8 [String] cannot be constructed from the
/// [BytesMut](bytes::BytesMut) slice.
impl MessageDecoder<String> for StringCodec {
    fn decode(&self, buffer: &mut BytesMut) -> Result<String> {
        Ok(String::from_utf8(buffer[..].into())?)
    }
}

impl SeliumCodec for StringCodec {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_into_string_bytes() {
        let input = "decoded string";
        let expected = BytesMut::from(input);

        let codec = StringCodec;
        let encoded = codec.encode(input).unwrap();

        assert_eq!(expected, encoded);
    }

    #[test]
    fn decodes_string_bytes() {
        let expected = "encoded string";
        let mut buffer = BytesMut::from(expected);

        let decoder = StringCodec;
        let decoded = decoder.decode(&mut buffer).unwrap();

        assert_eq!(decoded, expected);
    }
}
