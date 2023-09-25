use crate::Operation;
use anyhow::{bail, Result};
use uuid::Uuid;
use bytes::{BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};

const REGISTER_PUBLISHER: u8 = 0x0;
const REGISTER_SUBSCRIBER: u8 = 0x1;
const MESSAGE: u8 = 0x2;
const BATCH_MESSAGE: u8 = 0x3;
const CHUNK_MESSAGE: u8 = 0x4;

#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    RegisterPublisher(PublisherPayload),
    RegisterSubscriber(SubscriberPayload),
    Message(Bytes),
    BatchMessage(Bytes),
    ChunkMessage(Chunk),
}

impl Frame {
    pub fn get_length(&self) -> Result<u64> {
        let length = match self {
            Self::RegisterPublisher(payload) => bincode::serialized_size(payload)?,
            Self::RegisterSubscriber(payload) => bincode::serialized_size(payload)?,
            Self::ChunkMessage(chunk) => bincode::serialized_size(chunk)?,
            Self::Message(bytes) => bytes.len() as u64,
            Self::BatchMessage(bytes) => bytes.len() as u64,
        };

        Ok(length)
    }

    pub fn get_type(&self) -> u8 {
        match self {
            Self::RegisterPublisher(_) => REGISTER_PUBLISHER,
            Self::RegisterSubscriber(_) => REGISTER_SUBSCRIBER,
            Self::ChunkMessage(_) => CHUNK_MESSAGE,
            Self::Message(_) => MESSAGE,
            Self::BatchMessage(_) => BATCH_MESSAGE,
        }
    }

    pub fn get_topic(&self) -> Option<&str> {
        match self {
            Self::RegisterPublisher(p) => Some(&p.topic),
            Self::RegisterSubscriber(s) => Some(&s.topic),
            _ => None,
        }
    }

    pub fn write_to_bytes(self, dst: &mut BytesMut) -> Result<()> {
        match self {
            Frame::RegisterPublisher(payload) => bincode::serialize_into(dst.writer(), &payload)?,
            Frame::RegisterSubscriber(payload) => bincode::serialize_into(dst.writer(), &payload)?,
            Frame::ChunkMessage(chunk) => bincode::serialize_into(dst.writer(), &chunk)?,
            Frame::Message(bytes) => dst.extend_from_slice(&bytes),
            Frame::BatchMessage(bytes) => dst.extend_from_slice(&bytes),
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
            CHUNK_MESSAGE => Frame::ChunkMessage(bincode::deserialize(&bytes)?),
            MESSAGE => Frame::Message(bytes.into()),
            BATCH_MESSAGE => Frame::BatchMessage(bytes.into()),
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chunk {
    pub headers: ChunkHeaders,
    pub payload: Bytes
}

impl Chunk {
    pub fn new(headers: ChunkHeaders, payload: Bytes) -> Self {
        Self {
            headers,
            payload
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChunkHeaders {
    pub uuid: Uuid,
    pub seq_num: u32,
    pub seq_length: u32,
}

impl ChunkHeaders {
    pub fn new(uuid: Uuid, seq_num: u32, seq_length: u32) -> Self {
        assert!(seq_num <= seq_length);

        Self {
            uuid,
            seq_num,
            seq_length
        }
    }
}
