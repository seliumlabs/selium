use std::mem::size_of;

use crate::types::Operation;
use anyhow::{bail, Result};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};

const REGISTER_PUBLISHER: u8 = 0x0;
const REGISTER_SUBSCRIBER: u8 = 0x1;
const MESSAGE: u8 = 0x2;
const BATCH_MESSAGE: u8 = 0x3;

#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    RegisterPublisher(PublisherPayload),
    RegisterSubscriber(SubscriberPayload),
    Message(Bytes),
    BatchMessage(Vec<Bytes>),
}

impl Frame {
    pub fn get_length(&self) -> Result<u64> {
        let length = match self {
            Self::RegisterPublisher(payload) => bincode::serialized_size(payload)?,
            Self::RegisterSubscriber(payload) => bincode::serialized_size(payload)?,
            Self::Message(bytes) => bytes.len() as u64,
            Self::BatchMessage(messages) => {
                let num_of_messages = size_of::<u64>();
                let messages_len = size_of::<u64>() * messages.len();
                let bytes_len = messages.iter().fold(0, |len, m| len + m.len());

                (num_of_messages + messages_len + bytes_len) as u64
            }
        };

        Ok(length)
    }

    pub fn get_type(&self) -> u8 {
        match self {
            Self::RegisterPublisher(_) => REGISTER_PUBLISHER,
            Self::RegisterSubscriber(_) => REGISTER_SUBSCRIBER,
            Self::Message(_) => MESSAGE,
            Self::BatchMessage(_) => BATCH_MESSAGE,
        }
    }

    pub fn get_topic(&self) -> Option<&str> {
        match self {
            Self::RegisterPublisher(p) => Some(&p.topic),
            Self::RegisterSubscriber(s) => Some(&s.topic),
            Self::Message(_) => None,
            Self::BatchMessage(_) => None,
        }
    }

    pub fn write_to_bytes(self, dst: &mut BytesMut) -> Result<()> {
        match self {
            Frame::RegisterPublisher(payload) => bincode::serialize_into(dst.writer(), &payload)?,
            Frame::RegisterSubscriber(payload) => bincode::serialize_into(dst.writer(), &payload)?,
            Frame::Message(bytes) => dst.extend_from_slice(&bytes),
            Frame::BatchMessage(messages) => {
                // Put a u64 into dst representing the amount of messages in the batch
                dst.put_u64(messages.len() as u64);
                messages.iter().for_each(|m| {
                    // Put a u64 into dst representing the length of the message
                    dst.put_u64(m.len() as u64);
                    // Put the message bytes into dst
                    dst.extend_from_slice(m)
                });
            }
        }

        Ok(())
    }
}

impl TryFrom<(u8, BytesMut)> for Frame {
    type Error = anyhow::Error;

    fn try_from((message_type, mut bytes): (u8, BytesMut)) -> Result<Self> {
        let frame = match message_type {
            REGISTER_PUBLISHER => Frame::RegisterPublisher(bincode::deserialize(&bytes)?),
            REGISTER_SUBSCRIBER => Frame::RegisterSubscriber(bincode::deserialize(&bytes)?),
            MESSAGE => Frame::Message(bytes.into()),
            BATCH_MESSAGE => {
                let num_of_messages = bytes.get_u64();
                let mut messages = Vec::with_capacity(num_of_messages as usize);

                for _ in 0..num_of_messages {
                    let message_len = bytes.get_u64();
                    let message_bytes = bytes.split_to(message_len as usize);
                    messages.push(message_bytes.into());
                }

                Frame::BatchMessage(messages)
            }
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubscriberPayload {
    pub topic: String,
    pub retention_policy: u64,
    pub operations: Vec<Operation>,
}
