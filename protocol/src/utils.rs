use bytes::{Buf, BufMut, Bytes, BytesMut};

pub fn encode_message_batch(batch: Vec<Bytes>) -> Bytes {
    let mut bytes = BytesMut::new();
    bytes.put_u64(batch.len() as u64);

    batch.iter().for_each(|m| {
        // Put a u64 into dst representing the length of the message
        bytes.put_u64(m.len() as u64);
        // Put the message bytes into dst
        bytes.extend_from_slice(m)
    });

    bytes.into()
}

pub fn decode_message_batch(mut bytes: Bytes) -> Vec<Bytes> {
    let num_of_messages = bytes.get_u64();
    let mut messages = Vec::with_capacity(num_of_messages as usize);

    for _ in 0..num_of_messages {
        let message_len = bytes.get_u64();
        let message_bytes = bytes.split_to(message_len as usize);
        messages.push(message_bytes);
    }

    messages
}
