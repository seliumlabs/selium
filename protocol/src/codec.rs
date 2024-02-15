use crate::Frame;
use anyhow::Result;
use bytes::{Buf, BufMut, BytesMut};
use selium_std::errors::{ProtocolError, SeliumError};
use std::mem::size_of;
use tokio_util::codec::{Decoder, Encoder};

const MAX_MESSAGE_SIZE: u64 = 1024 * 1024;
const LEN_MARKER_SIZE: usize = size_of::<u64>();
const TYPE_MARKER_SIZE: usize = size_of::<u8>();
const RESERVED_SIZE: usize = LEN_MARKER_SIZE + TYPE_MARKER_SIZE;

#[derive(Debug, Default)]
pub struct MessageCodec;

impl Encoder<Frame> for MessageCodec {
    type Error = SeliumError;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let length = item.get_length()?;
        validate_payload_length(length)?;

        let message_type = item.get_type();

        dst.reserve(RESERVED_SIZE + length as usize);
        dst.put_u64(length);
        dst.put_u8(message_type);
        item.write_to_bytes(dst)?;

        Ok(())
    }
}

impl Decoder for MessageCodec {
    type Error = SeliumError;
    type Item = Frame;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < RESERVED_SIZE {
            return Ok(None);
        }

        let mut length_bytes = [0u8; LEN_MARKER_SIZE];
        length_bytes.copy_from_slice(&src[..LEN_MARKER_SIZE]);

        let length = u64::from_be_bytes(length_bytes);
        validate_payload_length(length)?;

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

fn validate_payload_length(length: u64) -> Result<(), SeliumError> {
    if length > MAX_MESSAGE_SIZE {
        Err(ProtocolError::PayloadTooLarge(length, MAX_MESSAGE_SIZE))?
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::error_codes::UNKNOWN_ERROR;
    use crate::utils::encode_message_batch;
    use crate::{
        ErrorPayload, MessagePayload, Operation, PublisherPayload, SubscriberPayload, TopicName,
    };
    use bytes::Bytes;

    #[test]
    fn encodes_register_subscriber_frame() {
        let topic = TopicName::try_from("/namespace/topic").unwrap();

        let frame = Frame::RegisterSubscriber(SubscriberPayload {
            topic,
            retention_policy: 5,
            operations: vec![
                Operation::Map("first/module.wasm".into()),
                Operation::Map("second/module.wasm".into()),
                Operation::Filter("third/module.wasm".into()),
            ],
        });

        let mut codec = MessageCodec;
        let mut buffer = BytesMut::new();
        let expected = Bytes::from_static(b"\0\0\0\0\0\0\0\x86\x01\t\0\0\0\0\0\0\0namespace\x05\0\0\0\0\0\0\0topic\x05\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\0\0\0\0\x11\0\0\0\0\0\0\0first/module.wasm\0\0\0\0\x12\0\0\0\0\0\0\0second/module.wasm\x01\0\0\0\x11\0\0\0\0\0\0\0third/module.wasm");

        codec.encode(frame, &mut buffer).unwrap();

        assert_eq!(buffer, expected);
    }

    #[test]
    fn encodes_register_publisher_frame() {
        let topic = TopicName::try_from("/namespace/topic").unwrap();

        let frame = Frame::RegisterPublisher(PublisherPayload {
            topic,
            retention_policy: 5,
            operations: vec![
                Operation::Map("first/module.wasm".into()),
                Operation::Map("second/module.wasm".into()),
                Operation::Filter("third/module.wasm".into()),
            ],
        });

        let mut codec = MessageCodec;
        let mut buffer = BytesMut::new();
        let expected = Bytes::from_static(b"\0\0\0\0\0\0\0\x86\0\t\0\0\0\0\0\0\0namespace\x05\0\0\0\0\0\0\0topic\x05\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\0\0\0\0\x11\0\0\0\0\0\0\0first/module.wasm\0\0\0\0\x12\0\0\0\0\0\0\0second/module.wasm\x01\0\0\0\x11\0\0\0\0\0\0\0third/module.wasm");

        codec.encode(frame, &mut buffer).unwrap();

        assert_eq!(buffer, expected);
    }

    #[test]
    fn encodes_message_frame_with_header() {
        let mut h = HashMap::new();
        h.insert("test".to_owned(), "header".to_owned());

        let frame = Frame::Message(MessagePayload {
            headers: Some(h),
            message: Bytes::from("Hello world"),
        });

        let mut codec = MessageCodec;
        let mut buffer = BytesMut::new();
        let expected = Bytes::from_static(b"\0\0\0\0\0\0\06\x04\x01\x01\0\0\0\0\0\0\0\x04\0\0\0\0\0\0\0test\x06\0\0\0\0\0\0\0header\x0b\0\0\0\0\0\0\0Hello world");

        codec.encode(frame, &mut buffer).unwrap();

        assert_eq!(buffer, expected);
    }

    #[test]
    fn encodes_message_frame_without_header() {
        let frame = Frame::Message(MessagePayload {
            headers: None,
            message: Bytes::from("Hello world"),
        });

        let mut codec = MessageCodec;
        let mut buffer = BytesMut::new();
        let expected = Bytes::from("\0\0\0\0\0\0\0\x14\x04\0\x0b\0\0\0\0\0\0\0Hello world");

        codec.encode(frame, &mut buffer).unwrap();

        assert_eq!(buffer, expected);
    }

    #[test]
    fn encodes_batch_message_frame() {
        let batch = encode_message_batch(vec![
            Bytes::from("First message"),
            Bytes::from("Second message"),
            Bytes::from("Third message"),
        ]);

        let frame = Frame::BatchMessage(batch);

        let mut codec = MessageCodec;
        let mut buffer = BytesMut::new();
        let expected = Bytes::from("\0\0\0\0\0\0\0H\x05\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\rFirst message\0\0\0\0\0\0\0\x0eSecond message\0\0\0\0\0\0\0\rThird message");

        codec.encode(frame, &mut buffer).unwrap();

        assert_eq!(buffer, expected);
    }

    #[test]
    fn encodes_error_frame() {
        let frame = Frame::Error(ErrorPayload {
            code: UNKNOWN_ERROR,
            message: "This is an error".into(),
        });

        let mut codec = MessageCodec;
        let mut buffer = BytesMut::new();
        let expected =
            Bytes::from_static(b"\0\0\0\0\0\0\0\x1c\x06\0\0\0\0\x10\0\0\0\0\0\0\0This is an error");

        codec.encode(frame, &mut buffer).unwrap();

        assert_eq!(buffer, expected);
    }

    #[test]
    fn encodes_ok_frame() {
        let frame = Frame::Ok;

        let mut codec = MessageCodec;
        let mut buffer = BytesMut::new();
        let expected = Bytes::from_static(b"\0\0\0\0\0\0\0\0\x07");

        codec.encode(frame, &mut buffer).unwrap();

        assert_eq!(buffer, expected);
    }

    #[test]
    fn fails_to_encode_if_payload_too_large() {
        const PAYLOAD: [u8; MAX_MESSAGE_SIZE as usize + 1] = [0u8; MAX_MESSAGE_SIZE as usize + 1];

        let frame = Frame::Message(MessagePayload {
            headers: None,
            message: Bytes::from_static(&PAYLOAD),
        });
        let mut codec = MessageCodec;
        let mut buffer = BytesMut::new();

        assert!(codec.encode(frame, &mut buffer).is_err());
    }

    #[test]
    fn decodes_register_subscriber_frame() {
        let mut codec = MessageCodec;
        let mut src = BytesMut::from(&b"\0\0\0\0\0\0\0\x86\x01\t\0\0\0\0\0\0\0namespace\x05\0\0\0\0\0\0\0topic\x05\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\0\0\0\0\x11\0\0\0\0\0\0\0first/module.wasm\0\0\0\0\x12\0\0\0\0\0\0\0second/module.wasm\x01\0\0\0\x11\0\0\0\0\0\0\0third/module.wasm"[..]);
        let topic = TopicName::try_from("/namespace/topic").unwrap();

        let expected = Frame::RegisterSubscriber(SubscriberPayload {
            topic,
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
        let mut src = BytesMut::from(&b"\0\0\0\0\0\0\0\x86\0\t\0\0\0\0\0\0\0namespace\x05\0\0\0\0\0\0\0topic\x05\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\0\0\0\0\x11\0\0\0\0\0\0\0first/module.wasm\0\0\0\0\x12\0\0\0\0\0\0\0second/module.wasm\x01\0\0\0\x11\0\0\0\0\0\0\0third/module.wasm"[..]);
        let topic = TopicName::try_from("/namespace/topic").unwrap();

        let expected = Frame::RegisterPublisher(PublisherPayload {
            topic,
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
    fn decodes_message_frame_with_header() {
        let mut codec = MessageCodec;
        let mut src = BytesMut::from("\0\0\0\0\0\0\06\x04\x01\x01\0\0\0\0\0\0\0\x04\0\0\0\0\0\0\0test\x06\0\0\0\0\0\0\0header\x0b\0\0\0\0\0\0\0Hello world");

        let mut h = HashMap::new();
        h.insert("test".to_owned(), "header".to_owned());

        let expected = Frame::Message(MessagePayload {
            headers: Some(h),
            message: Bytes::from("Hello world"),
        });
        let result = codec.decode(&mut src).unwrap().unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn decodes_message_frame_without_header() {
        let mut codec = MessageCodec;
        let mut src = BytesMut::from("\0\0\0\0\0\0\0\x14\x04\0\x0b\0\0\0\0\0\0\0Hello world");

        let expected = Frame::Message(MessagePayload {
            headers: None,
            message: Bytes::from("Hello world"),
        });
        let result = codec.decode(&mut src).unwrap().unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn decodes_batch_message_frame() {
        let mut codec = MessageCodec;
        let mut src = BytesMut::from("\0\0\0\0\0\0\0H\x05\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\rFirst message\0\0\0\0\0\0\0\x0eSecond message\0\0\0\0\0\0\0\rThird message");

        let batch = encode_message_batch(vec![
            Bytes::from("First message"),
            Bytes::from("Second message"),
            Bytes::from("Third message"),
        ]);

        let expected = Frame::BatchMessage(batch);
        let result = codec.decode(&mut src).unwrap().unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn decodes_error_frame() {
        let mut codec = MessageCodec;
        let mut src =
            BytesMut::from("\0\0\0\0\0\0\0\x1c\x06\0\0\0\0\x10\0\0\0\0\0\0\0This is an error");

        let expected = Frame::Error(ErrorPayload {
            code: UNKNOWN_ERROR,
            message: "This is an error".into(),
        });

        let result = codec.decode(&mut src).unwrap().unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn decodes_ok_frame() {
        let mut codec = MessageCodec;
        let mut src = BytesMut::from("\0\0\0\0\0\0\0\0\x07");

        let expected = Frame::Ok;

        let result = codec.decode(&mut src).unwrap().unwrap();

        assert_eq!(result, expected);
    }

    #[test]
    fn fails_to_decode_if_payload_too_large() {
        const PAYLOAD: [u8; MAX_MESSAGE_SIZE as usize + 1] = [0u8; MAX_MESSAGE_SIZE as usize + 1];

        let mut codec = MessageCodec;
        let mut src = BytesMut::from(&PAYLOAD[..]);

        assert!(codec.decode(&mut src).is_err());
    }
}
