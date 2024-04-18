use crate::{Offset, Operation, TopicName};
use bytes::{BufMut, Bytes, BytesMut};
use selium_std::errors::{ProtocolError, Result, SeliumError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

type Headers = Option<HashMap<String, String>>;

const REGISTER_PUBLISHER: u8 = 0x0;
const REGISTER_SUBSCRIBER: u8 = 0x1;
const REGISTER_REPLIER: u8 = 0x2;
const REGISTER_REQUESTOR: u8 = 0x3;
const MESSAGE: u8 = 0x4;
const BATCH_MESSAGE: u8 = 0x5;
const ERROR: u8 = 0x6;
const OK: u8 = 0x7;

#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    RegisterPublisher(PublisherPayload),
    RegisterSubscriber(SubscriberPayload),
    RegisterReplier(ReplierPayload),
    RegisterRequestor(RequestorPayload),
    Message(MessagePayload),
    BatchMessage(BatchPayload),
    Error(ErrorPayload),
    Ok,
}

impl Frame {
    pub fn get_length(&self) -> Result<u64> {
        Ok(match self {
            Self::RegisterPublisher(payload) => {
                bincode::serialized_size(payload).map_err(ProtocolError::SerdeError)?
            }
            Self::RegisterSubscriber(payload) => {
                bincode::serialized_size(payload).map_err(ProtocolError::SerdeError)?
            }
            Self::RegisterReplier(payload) => {
                bincode::serialized_size(payload).map_err(ProtocolError::SerdeError)?
            }
            Self::RegisterRequestor(payload) => {
                bincode::serialized_size(payload).map_err(ProtocolError::SerdeError)?
            }
            Self::Message(payload) => {
                bincode::serialized_size(payload).map_err(ProtocolError::SerdeError)?
            }
            Self::BatchMessage(payload) => {
                bincode::serialized_size(payload).map_err(ProtocolError::SerdeError)?
            }
            Self::Error(payload) => {
                bincode::serialized_size(payload).map_err(ProtocolError::SerdeError)?
            }
            Self::Ok => 0,
        })
    }

    pub fn get_type(&self) -> u8 {
        match self {
            Self::RegisterPublisher(_) => REGISTER_PUBLISHER,
            Self::RegisterSubscriber(_) => REGISTER_SUBSCRIBER,
            Self::RegisterReplier(_) => REGISTER_REPLIER,
            Self::RegisterRequestor(_) => REGISTER_REQUESTOR,
            Self::Message(_) => MESSAGE,
            Self::BatchMessage(_) => BATCH_MESSAGE,
            Self::Error(_) => ERROR,
            Self::Ok => OK,
        }
    }

    pub fn get_topic(&self) -> Option<&TopicName> {
        match self {
            Self::RegisterPublisher(p) => Some(&p.topic),
            Self::RegisterSubscriber(s) => Some(&s.topic),
            Self::RegisterReplier(s) => Some(&s.topic),
            Self::RegisterRequestor(c) => Some(&c.topic),
            Self::Message(_) => None,
            Self::BatchMessage(_) => None,
            Self::Error(_) => None,
            Self::Ok => None,
        }
    }

    pub fn write_to_bytes(self, dst: &mut BytesMut) -> Result<()> {
        match self {
            Frame::RegisterPublisher(payload) => bincode::serialize_into(dst.writer(), &payload)
                .map_err(ProtocolError::SerdeError)?,
            Frame::RegisterSubscriber(payload) => bincode::serialize_into(dst.writer(), &payload)
                .map_err(ProtocolError::SerdeError)?,
            Frame::RegisterReplier(payload) => bincode::serialize_into(dst.writer(), &payload)
                .map_err(ProtocolError::SerdeError)?,
            Frame::RegisterRequestor(payload) => bincode::serialize_into(dst.writer(), &payload)
                .map_err(ProtocolError::SerdeError)?,
            Frame::Message(payload) => bincode::serialize_into(dst.writer(), &payload)
                .map_err(ProtocolError::SerdeError)?,
            Frame::Error(payload) => bincode::serialize_into(dst.writer(), &payload)
                .map_err(ProtocolError::SerdeError)?,
            Frame::BatchMessage(payload) => bincode::serialize_into(dst.writer(), &payload)
                .map_err(ProtocolError::SerdeError)?,
            Frame::Ok => (),
        }

        Ok(())
    }

    pub fn unwrap_retention_policy(&self) -> u64 {
        match self {
            Self::RegisterPublisher(payload) => payload.retention_policy,
            Self::RegisterSubscriber(payload) => payload.retention_policy,
            _ => panic!("Attempted to unwrap non-register Frame variant"),
        }
    }

    pub fn unwrap_batch_size(&self) -> u32 {
        match self {
            Self::Message(_) => 1,
            Self::BatchMessage(payload) => payload.size,
            _ => panic!("Attempted to unwrap non-Message Frame variant"),
        }
    }

    pub fn unwrap_payload(self) -> Bytes {
        match self {
            Self::Message(payload) => payload.message,
            Self::BatchMessage(payload) => payload.message,
            _ => panic!("Attempted to unwrap non-Message Frame variant"),
        }
    }

    pub fn unwrap_message(self) -> MessagePayload {
        match self {
            Self::Message(p) => p,
            _ => panic!("Attempted to unwrap non-Message Frame variant"),
        }
    }
}

impl TryFrom<(u8, BytesMut)> for Frame {
    type Error = SeliumError;

    fn try_from(
        (message_type, bytes): (u8, BytesMut),
    ) -> Result<Self, <Frame as TryFrom<(u8, BytesMut)>>::Error> {
        let frame = match message_type {
            REGISTER_PUBLISHER => Frame::RegisterPublisher(
                bincode::deserialize(&bytes).map_err(ProtocolError::SerdeError)?,
            ),
            REGISTER_SUBSCRIBER => Frame::RegisterSubscriber(
                bincode::deserialize(&bytes).map_err(ProtocolError::SerdeError)?,
            ),
            REGISTER_REPLIER => Frame::RegisterReplier(
                bincode::deserialize(&bytes).map_err(ProtocolError::SerdeError)?,
            ),
            REGISTER_REQUESTOR => Frame::RegisterRequestor(
                bincode::deserialize(&bytes).map_err(ProtocolError::SerdeError)?,
            ),
            MESSAGE => {
                Frame::Message(bincode::deserialize(&bytes).map_err(ProtocolError::SerdeError)?)
            }
            BATCH_MESSAGE => Frame::BatchMessage(
                bincode::deserialize(&bytes).map_err(ProtocolError::SerdeError)?,
            ),
            ERROR => Frame::Error(bincode::deserialize(&bytes).map_err(ProtocolError::SerdeError)?),
            OK => Frame::Ok,
            _type => return Err(ProtocolError::UnknownMessageType(_type))?,
        };

        Ok(frame)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublisherPayload {
    pub topic: TopicName,
    pub retention_policy: u64,
    pub operations: Vec<Operation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubscriberPayload {
    pub topic: TopicName,
    pub retention_policy: u64,
    pub operations: Vec<Operation>,
    pub offset: Offset,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplierPayload {
    pub topic: TopicName,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestorPayload {
    pub topic: TopicName,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessagePayload {
    pub headers: Headers,
    pub message: Bytes,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BatchPayload {
    pub message: Bytes,
    pub size: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorPayload {
    pub code: u32,
    pub message: Bytes,
}
