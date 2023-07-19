use crate::protocol::Frame;
use bytes::{Buf, BufMut, BytesMut};
use std::mem::size_of;
use tokio_util::codec::{Decoder, Encoder};

const LEN_MARKER_SIZE: usize = size_of::<u32>();

#[derive(Debug)]
pub struct MessageCodec {}

impl MessageCodec {
    pub fn new() -> Self {
        Self {}
    }
}

impl Encoder<Frame> for MessageCodec {
    type Error = anyhow::Error;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let serialized = bincode::serialize(&item)?;
        let len = serialized.len();

        dst.reserve(LEN_MARKER_SIZE + len);
        dst.put_u32(len as u32);
        dst.extend_from_slice(&serialized);

        Ok(())
    }
}

impl Decoder for MessageCodec {
    type Error = anyhow::Error;
    type Item = Frame;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < LEN_MARKER_SIZE {
            return Ok(None);
        }

        let mut msg_len_bytes = [0u8; LEN_MARKER_SIZE];
        msg_len_bytes.copy_from_slice(&src[..LEN_MARKER_SIZE]);

        let msg_len = u32::from_be_bytes(msg_len_bytes);
        let frame_len = LEN_MARKER_SIZE + msg_len as usize;

        if src.len() < frame_len {
            src.reserve(frame_len - src.len());
            return Ok(None);
        }

        src.advance(LEN_MARKER_SIZE);

        let frame = bincode::deserialize(&src)?;

        src.advance(msg_len as usize);

        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SubscriberPayload;
    use crate::Operation;
    use bytes::Bytes;

    #[test]
    fn encodes_register_subscriber_frame() {
        let frame = Frame::RegisterSubscriber(SubscriberPayload {
            topic: "Some topic".into(),
            operations: vec![
                Operation::Map("first/module.wasm".into()),
                Operation::Map("second/module.wasm".into()),
                Operation::Filter("third/module.wasm".into()),
            ],
        });

        let mut codec = MessageCodec::new();
        let mut buffer = BytesMut::new();
        let expected = Bytes::from("\0\0\0v\x01\0\0\0\n\0\0\0\0\0\0\0Some topic\x03\0\0\0\0\0\0\0\0\0\0\0\x11\0\0\0\0\0\0\0first/module.wasm\0\0\0\0\x12\0\0\0\0\0\0\0second/module.wasm\x01\0\0\0\x11\0\0\0\0\0\0\0third/module.wasm");

        codec.encode(frame, &mut buffer).unwrap();

        assert_eq!(buffer, expected);
    }

    #[test]
    fn decodes_register_subscriber_frame() {
        let mut codec = MessageCodec::new();
        let mut src = BytesMut::from("\0\0\0v\x01\0\0\0\n\0\0\0\0\0\0\0Some topic\x03\0\0\0\0\0\0\0\0\0\0\0\x11\0\0\0\0\0\0\0first/module.wasm\0\0\0\0\x12\0\0\0\0\0\0\0second/module.wasm\x01\0\0\0\x11\0\0\0\0\0\0\0third/module.wasm");

        let expected = Frame::RegisterSubscriber(SubscriberPayload {
            topic: "Some topic".into(),
            operations: vec![
                Operation::Map("first/module.wasm".into()),
                Operation::Map("second/module.wasm".into()),
                Operation::Filter("third/module.wasm".into()),
            ],
        });

        let result = codec.decode(&mut src).unwrap().unwrap();

        assert_eq!(result, expected);
    }
}
