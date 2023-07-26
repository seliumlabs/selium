use anyhow::Result;
use bytes::{Bytes, BytesMut};

pub trait MessageEncoder<Item> {
    fn encode(&self, item: Item) -> Result<Bytes>;
}

pub trait MessageDecoder<T> {
    fn decode(&self, buffer: &mut BytesMut) -> Result<T>;
}
