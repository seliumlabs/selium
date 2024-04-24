use crate::{
    error::Result,
    message::{Headers, Message, CRC_SIZE, HEADERS_SIZE},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, BufReader},
};

#[derive(Debug)]
pub struct LogIterator {
    reader: BufReader<File>,
    cursor: u64,
    end_position: u64,
}

impl LogIterator {
    pub fn new(reader: BufReader<File>, cursor: u64, end_position: u64) -> Self {
        Self {
            reader,
            cursor,
            end_position,
        }
    }

    pub async fn next(&mut self) -> Option<Result<Message>> {
        if self.cursor >= self.end_position {
            return None;
        }

        let mut headers = vec![0; HEADERS_SIZE];
        self.reader.read_exact(&mut headers).await.ok()?;

        let headers = Headers::decode(&headers);
        let remainder_len = headers.length() as usize - HEADERS_SIZE;
        let combined_len = HEADERS_SIZE + remainder_len;
        let records_len = remainder_len - CRC_SIZE;

        let mut remainder = vec![0; remainder_len];
        self.reader.read_exact(&mut remainder).await.ok()?;

        let records = &remainder[..records_len];
        let mut crc = [0; CRC_SIZE];
        crc.copy_from_slice(&remainder[records_len..]);
        let crc = u32::from_be_bytes(crc);
        let message = Message::decode(headers, records, crc);

        self.cursor += combined_len as u64;

        Some(Ok(message))
    }
}
