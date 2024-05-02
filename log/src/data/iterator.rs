use crate::{
    error::Result,
    message::{Headers, Message, CRC_SIZE, HEADERS_SIZE},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, BufReader},
};

/// An iterator over a [Data](crate::data::Data) file.
///
/// The LogIterator acts as an active reader over the log, pulling messages from the log
/// and decoding them, while maintaining a cursor to ensure that superflous reads are not performed
/// when a read limit is provided via `end_position`.
#[derive(Debug)]
pub struct LogIterator {
    reader: BufReader<File>,
    cursor: u64,
    end_position: u64,
}

impl LogIterator {
    /// Constructs a new LogIterator instance.
    pub fn new(reader: BufReader<File>, cursor: u64, end_position: u64) -> Self {
        Self {
            reader,
            cursor,
            end_position,
        }
    }

    /// Attempts to decode and retrieve the next message from the `reader`.
    /// Returns [Option::None] if there are no more messages to decode.
    ///
    /// # Errors
    /// Returns std::io::ErrorKind::UnexpectedEof if the an unexpected end-of-file
    /// is encountered due to a partially committed or corrupted message.
    pub async fn next(&mut self) -> Result<Option<Message>> {
        if self.cursor >= self.end_position {
            return Ok(None);
        }

        let mut headers = vec![0; HEADERS_SIZE];
        self.reader.read_exact(&mut headers).await?;

        let headers = Headers::decode(&headers);
        let remainder_len = headers.length() as usize - HEADERS_SIZE;
        let combined_len = HEADERS_SIZE + remainder_len;
        let records_len = remainder_len - CRC_SIZE;

        let mut remainder = vec![0; remainder_len];
        self.reader.read_exact(&mut remainder).await?;

        let records = &remainder[..records_len];
        let mut crc = [0; CRC_SIZE];
        crc.copy_from_slice(&remainder[records_len..]);
        let crc = u32::from_be_bytes(crc);
        let message = Message::new(headers, records, crc);

        self.cursor += combined_len as u64;

        Ok(Some(message))
    }
}
