use crate::traits::codec::{MessageDecoder, MessageEncoder};
use anyhow::Result;
use bytes::{Bytes, BytesMut};

/// A basic codec for encoding/decoding UTF-8 [String] message payloads.
#[derive(Default, Clone)]
pub struct StringCodec;

/// Encodes a [&str] slice into [Bytes](bytes::Bytes).
impl MessageEncoder for StringCodec {
    type Item = String;

    fn encode(&self, item: String) -> Result<Bytes> {
        Ok(item.into())
    }
}

/// Decodes a [BytesMut](bytes::BytesMut) payload into an owned [String]
///
/// # Errors
///
/// Returns [Err] if a valid UTF-8 [String] cannot be constructed from the
/// [BytesMut](bytes::BytesMut) slice.
impl MessageDecoder for StringCodec {
    type Item = String;

    fn decode(&self, buffer: &mut BytesMut) -> Result<String> {
        Ok(String::from_utf8(buffer[..].into())?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_into_string_bytes() {
        let input = "decoded string";
        let expected = BytesMut::from(input);

        let codec = StringCodec;
        let encoded = codec.encode(input.to_owned()).unwrap();

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
