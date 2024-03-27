use super::Message;
use super::LEN_MARKER_SIZE;
use crate::error::LogError;
use bytes::{Buf, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

#[derive(Debug, Default)]
pub struct LogCodec;

impl Encoder<Message> for LogCodec {
    type Error = LogError;

    fn encode(&mut self, item: Message, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let length = item.headers().length();
        dst.reserve(length as usize);
        item.encode(dst);
        Ok(())
    }
}

impl Decoder for LogCodec {
    type Error = LogError;
    type Item = Message;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < LEN_MARKER_SIZE {
            return Ok(None);
        }

        let length = get_length(src);
        let bytes_read = src.len();

        if bytes_read < length as usize {
            src.reserve(bytes_read);
            return Ok(None);
        }

        src.advance(LEN_MARKER_SIZE);

        Ok(Some(Message::decode(src, length)))
    }
}

fn get_length(src: &mut BytesMut) -> u64 {
    let mut length_bytes = [0u8; LEN_MARKER_SIZE];
    length_bytes.copy_from_slice(&src[..LEN_MARKER_SIZE]);
    u64::from_be_bytes(length_bytes)
}
