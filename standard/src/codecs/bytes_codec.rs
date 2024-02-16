use crate::traits::codec::{MessageDecoder, MessageEncoder};
use anyhow::Result;
use bytes::{Bytes, BytesMut};

/// A basic codec for sending raw bytes.
#[derive(Default, Clone)]
pub struct BytesCodec;

/// Encodes a [Vec<u8>] slice into [Bytes](bytes::Bytes).
///
/// # Errors
///
/// Guaranteed not to error.
impl MessageEncoder for BytesCodec {
    type Item = Vec<u8>;
    fn encode(&self, item: Vec<u8>) -> Result<Bytes> {
        Ok(item.into())
    }
}

/// Decodes a [BytesMut](bytes::BytesMut) payload into an owned [Vec<u8>]
///
/// # Errors
///
/// Guaranteed not to error.
impl MessageDecoder<Vec<u8>> for BytesCodec {
    fn decode(&self, buffer: &mut BytesMut) -> Result<Vec<u8>> {
        Ok(buffer.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_into_string_bytes() {
        let input = b"decoded string".to_vec();
        let expected = BytesMut::from(input.as_slice());

        let codec = BytesCodec;
        let encoded = codec.encode(input.to_owned()).unwrap();

        assert_eq!(expected, encoded);
    }

    #[test]
    fn decodes_string_bytes() {
        let expected = b"encoded string".to_vec();
        let mut buffer = BytesMut::from(expected.as_slice());

        let decoder = BytesCodec;
        let decoded = decoder.decode(&mut buffer).unwrap();

        assert_eq!(decoded, expected);
    }
}
