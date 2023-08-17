use crate::types::Operation;
use anyhow::{bail, Result};
use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};

const REGISTER_PUBLISHER: u8 = 0x0;
const REGISTER_SUBSCRIBER: u8 = 0x1;
const MESSAGE: u8 = 0x2;

#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    RegisterPublisher(PublisherPayload),
    RegisterSubscriber(SubscriberPayload),
    Message(Bytes),
}

impl Frame {
    pub fn get_length(&self) -> Result<u64> {
        let length = match self {
            Self::RegisterPublisher(payload) => bincode::serialized_size(payload)?,
            Self::RegisterSubscriber(payload) => bincode::serialized_size(payload)?,
            Self::Message(bytes) => bytes.len() as u64,
        };

        Ok(length)
    }

    pub fn get_type(&self) -> u8 {
        match self {
            Self::RegisterPublisher(_) => REGISTER_PUBLISHER,
            Self::RegisterSubscriber(_) => REGISTER_SUBSCRIBER,
            Self::Message(_) => MESSAGE,
        }
    }

    pub fn get_topic(&self) -> Option<&str> {
        match self {
            Self::RegisterPublisher(p) => Some(&p.topic),
            Self::RegisterSubscriber(s) => Some(&s.topic),
            Self::Message(_) => None,
        }
    }

    pub fn write_to_bytes(self, dst: &mut BytesMut) -> Result<()> {
        match self {
            Frame::RegisterPublisher(payload) => bincode::serialize_into(dst.writer(), &payload)?,
            Frame::RegisterSubscriber(payload) => bincode::serialize_into(dst.writer(), &payload)?,
            Frame::Message(bytes) => dst.extend_from_slice(&bytes),
        }

        Ok(())
    }
}

impl TryFrom<(u8, BytesMut)> for Frame {
    type Error = anyhow::Error;

    fn try_from((message_type, bytes): (u8, BytesMut)) -> Result<Self> {
        let frame = match message_type {
            REGISTER_PUBLISHER => Frame::RegisterPublisher(bincode::deserialize(&bytes)?),
            REGISTER_SUBSCRIBER => Frame::RegisterSubscriber(bincode::deserialize(&bytes)?),
            MESSAGE => Frame::Message(bytes.into()),
            _ => bail!("Unknown message type"),
        };

        Ok(frame)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublisherPayload {
    pub topic: String,
    pub retention_policy: u64,
    pub operations: Vec<Operation>,
    pub encoder: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubscriberPayload {
    pub topic: String,
    pub retention_policy: u64,
    pub operations: Vec<Operation>,
    pub decoder: Option<String>,
}
