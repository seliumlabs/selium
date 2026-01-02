use rkyv::{Archive, Deserialize, Serialize};

/// Request to read data from a reader.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct IoRead {
    /// Handle of the reader.
    pub handle: crate::GuestUint,
    /// Maximum number of bytes to read.
    pub len: crate::GuestUint,
}

/// Request to write data to a writer.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct IoWrite {
    /// Handle of the writer.
    pub handle: crate::GuestUint,
    /// Payload to be written.
    pub payload: Vec<u8>,
}

/// Response carrying an attributed frame.
#[derive(Debug, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(bytecheck())]
pub struct IoFrame {
    /// Identifier of the writer that produced this frame.
    pub writer_id: u16,
    /// Frame payload.
    pub payload: Vec<u8>,
}
