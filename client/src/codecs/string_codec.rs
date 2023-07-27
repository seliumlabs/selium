use crate::traits::{MessageDecoder, MessageEncoder};
use anyhow::Result;
use bytes::{Bytes, BytesMut};

#[derive(Default, Clone)]
pub struct StringCodec;

impl MessageEncoder<&str> for StringCodec {
    fn encode(&self, item: &str) -> Result<Bytes> {
        Ok(item.to_owned().into())
    }
}

impl MessageDecoder<String> for StringCodec {
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
