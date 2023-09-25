use super::ChunkConfig;
use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use selium_protocol::Chunk;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug)]
pub struct ChunkEntry {
    pub(crate) total_length: u64,
    pub(crate) chunks: Vec<Chunk>,
}

impl ChunkEntry {
    pub fn new(seq_length: u32) -> Self {
        Self {
            total_length: 0,
            chunks: Vec::with_capacity(seq_length as usize),
        }
    }
}

#[derive(Debug)]
pub struct ChunkStore {
    pub(crate) store: HashMap<Uuid, ChunkEntry>,
    pub(crate) config: ChunkConfig,
}

impl ChunkStore {
    pub fn insert(&mut self, id: Uuid, chunk: Chunk) {
        let entry = self
            .store
            .entry(id)
            .or_insert(ChunkEntry::new(chunk.headers.seq_length));

        entry.total_length += chunk.payload.len() as u64;
        entry.chunks.push(chunk);
    }

    pub fn combine(&mut self, id: Uuid) -> Result<Bytes> {
        let entry = self
            .store
            .remove(&id)
            .context("Sequence not found in message chunk store")?;

        let mut combined = BytesMut::with_capacity(entry.total_length as usize);

        for chunk in entry.chunks {
            combined.extend_from_slice(&chunk.payload);
        }

        Ok(combined.into())
    }
}

impl From<ChunkConfig> for ChunkStore {
    fn from(config: ChunkConfig) -> Self {
        Self {
            store: HashMap::new(),
            config,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selium_protocol::ChunkHeaders;

    #[test]
    fn inserts_chunks_in_sequence() {
        let first_uuid = Uuid::new_v4();
        let second_uuid = Uuid::new_v4();

        let config = ChunkConfig::max();
        let store = ChunkStore::from(config);

        let chunks = vec![
            Chunk::new(
                ChunkHeaders::new(first_uuid, 1, 3),
                Bytes::from("hello, world"),
            ),
            Chunk::new(
                ChunkHeaders::new(first_uuid, 3, 3),
                Bytes::from("hello, world"),
            ),
            Chunk::new(
                ChunkHeaders::new(second_uuid, 2, 2),
                Bytes::from("hello, world"),
            ),
            Chunk::new(
                ChunkHeaders::new(first_uuid, 2, 3),
                Bytes::from("hello, world"),
            ),
            Chunk::new(
                ChunkHeaders::new(second_uuid, 1, 2),
                Bytes::from("hello, world"),
            ),
        ];
    }
}
