use crate::protocol::Frame;
use bytes::{Buf, BufMut, BytesMut};
use std::mem::size_of;
use tokio_util::codec::{Decoder, Encoder};

const LEN_MARKER_SIZE: usize = size_of::<u64>();
const TYPE_MARKER_SIZE: usize = size_of::<u8>();
const RESERVED_SIZE: usize = LEN_MARKER_SIZE + TYPE_MARKER_SIZE;

#[derive(Debug, Default)]
pub struct MessageCodec;

impl Encoder<Frame> for MessageCodec {
    type Error = anyhow::Error;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let length = item.get_length()?;
        let message_type = item.get_type();

        dst.reserve(RESERVED_SIZE + length as usize);
        dst.put_u64(length);
        dst.put_u8(message_type);
        item.write_to_bytes(dst)?;

        Ok(())
    }
}

impl Decoder for MessageCodec {
    type Error = anyhow::Error;
    type Item = Frame;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < RESERVED_SIZE {
            return Ok(None);
        }

        let mut length_bytes = [0u8; LEN_MARKER_SIZE];
        length_bytes.copy_from_slice(&src[..LEN_MARKER_SIZE]);

        let length = u64::from_be_bytes(length_bytes);
        let bytes_read = src.len() - RESERVED_SIZE;

        if bytes_read < length as usize {
            src.reserve(bytes_read);
            return Ok(None);
        }

        src.advance(LEN_MARKER_SIZE);

        let message_type = src.get_u8();
        let bytes = src.split_to(length as usize);
        let frame = Frame::try_from((message_type, bytes))?;

        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{PublisherPayload, SubscriberPayload};
    use crate::types::Operation;
    use bytes::Bytes;

    #[test]
    fn encodes_register_subscriber_frame() {
        let frame = Frame::RegisterSubscriber(SubscriberPayload {
            topic: "Some topic".into(),
            retention_policy: 5,
            operations: vec![
                Operation::Map("first/module.wasm".into()),
                Operation::Map("second/module.wasm".into()),
                Operation::Filter("third/module.wasm".into()),
            ],
        });

        let mut codec = MessageCodec;
        let mut buffer = BytesMut::new();
        let expected = Bytes::from("\0\0\0\0\0\0\0z\x01\n\0\0\0\0\0\0\0Some topic\x05\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\0\0\0\0\x11\0\0\0\0\0\0\0first/module.wasm\0\0\0\0\x12\0\0\0\0\0\0\0second/module.wasm\x01\0\0\0\x11\0\0\0\0\0\0\0third/module.wasm");

        codec.encode(frame, &mut buffer).unwrap();

        assert_eq!(buffer, expected);
    }

    #[test]
    fn encodes_register_publisher_frame() {
        let frame = Frame::RegisterPublisher(PublisherPayload {
            topic: "Some topic".into(),
            retention_policy: 5,
            operations: vec![
                Operation::Map("first/module.wasm".into()),
                Operation::Map("second/module.wasm".into()),
                Operation::Filter("third/module.wasm".into()),
            ],
        });

        let mut codec = MessageCodec;
        let mut buffer = BytesMut::new();
        let expected = Bytes::from("\0\0\0\0\0\0\0z\0\n\0\0\0\0\0\0\0Some topic\x05\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\0\0\0\0\x11\0\0\0\0\0\0\0first/module.wasm\0\0\0\0\x12\0\0\0\0\0\0\0second/module.wasm\x01\0\0\0\x11\0\0\0\0\0\0\0third/module.wasm");

        codec.encode(frame, &mut buffer).unwrap();

        assert_eq!(buffer, expected);
    }

    #[test]
    fn encodes_message_frame() {
        let frame = Frame::Message(Bytes::from("Hello world"));

        let mut codec = MessageCodec;
        let mut buffer = BytesMut::new();
        let expected = Bytes::from("\0\0\0\0\0\0\0\x0b\x02Hello world");

        codec.encode(frame, &mut buffer).unwrap();

        assert_eq!(buffer, expected);
    }

    #[test]
    fn encodes_batch_message_frame() {
        let batch = vec![
            Bytes::from("First message"),
            Bytes::from("Second message"),
            Bytes::from("Third message"),
        ];

        let frame = Frame::BatchMessage(batch);

        let mut codec = MessageCodec;
        let mut buffer = BytesMut::new();
        let expected = Bytes::from("\0\0\0\0\0\0\0H\x03\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\rFirst message\0\0\0\0\0\0\0\x0eSecond message\0\0\0\0\0\0\0\rThird message");

        codec.encode(frame, &mut buffer).unwrap();

        assert_eq!(buffer, expected);
    }

    #[test]
    fn decodes_register_subscriber_frame() {
        let mut codec = MessageCodec;
        let mut src = BytesMut::from("\0\0\0\0\0\0\0z\x01\n\0\0\0\0\0\0\0Some topic\x05\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\0\0\0\0\x11\0\0\0\0\0\0\0first/module.wasm\0\0\0\0\x12\0\0\0\0\0\0\0second/module.wasm\x01\0\0\0\x11\0\0\0\0\0\0\0third/module.wasm");

        let expected = Frame::RegisterSubscriber(SubscriberPayload {
            topic: "Some topic".into(),
            retention_policy: 5,
            operations: vec![
                Operation::Map("first/module.wasm".into()),
                Operation::Map("second/module.wasm".into()),
                Operation::Filter("third/module.wasm".into()),
            ],
        });

        let result = codec.decode(&mut src).unwrap().unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn decodes_register_publisher_frame() {
        let mut codec = MessageCodec;
        let mut src = BytesMut::from("\0\0\0\0\0\0\0z\0\n\0\0\0\0\0\0\0Some topic\x05\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\0\0\0\0\x11\0\0\0\0\0\0\0first/module.wasm\0\0\0\0\x12\0\0\0\0\0\0\0second/module.wasm\x01\0\0\0\x11\0\0\0\0\0\0\0third/module.wasm");

        let expected = Frame::RegisterPublisher(PublisherPayload {
            topic: "Some topic".into(),
            retention_policy: 5,
            operations: vec![
                Operation::Map("first/module.wasm".into()),
                Operation::Map("second/module.wasm".into()),
                Operation::Filter("third/module.wasm".into()),
            ],
        });

        let result = codec.decode(&mut src).unwrap().unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn decodes_message_frame() {
        let mut codec = MessageCodec;
        let mut src = BytesMut::from("\0\0\0\0\0\0\0\x0b\x02Hello world");

        let expected = Frame::Message(Bytes::from("Hello world"));
        let result = codec.decode(&mut src).unwrap().unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn decodes_batch_message_frame() {
        let mut codec = MessageCodec;
        let mut src = BytesMut::from("\0\0\0\0\0\0\0H\x03\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\rFirst message\0\0\0\0\0\0\0\x0eSecond message\0\0\0\0\0\0\0\rThird message");

        let expected = Frame::BatchMessage(vec![
            Bytes::from("First message"),
            Bytes::from("Second message"),
            Bytes::from("Third message"),
        ]);

        let result = codec.decode(&mut src).unwrap().unwrap();

        assert_eq!(result, expected);
    }
}
