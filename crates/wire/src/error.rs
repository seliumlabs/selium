use thiserror::Error;

/// Result type for selium-wire operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Single flat error type covering all messaging failure modes.
#[derive(Debug, Clone, Error, PartialEq)]
pub enum Error {
    #[error("invalid layout")]
    InvalidLayout,
    #[error("buffer full")]
    BufferFull,
    #[error("buffer empty")]
    BufferEmpty,
    #[error("reader was overtaken by writers")]
    ReaderBehind,
    #[error("invalid frame header: {0}")]
    InvalidFrame(String),
    #[error("torn frame header at position {position}: checksum mismatch")]
    TornFrame {
        /// Logical byte position where the header failed integrity verification.
        position: u64,
    },
    #[error("capacity exceeded")]
    CapacityExceeded,
    #[error("compare-and-set failed: expected {expected}, got {actual:?}")]
    CasConflict {
        /// Expected value.
        expected: u64,
        /// Actual value found.
        actual: Option<u64>,
    },
    #[error("channel closed")]
    ChannelClosed,
    #[error("connection lost")]
    ConnectionLost,
    #[error("channel terminated")]
    Terminated,
    #[error("serialization error: {0}")]
    SerializationFailed(String),
    #[error("invalid shared region")]
    InvalidRegion,
    #[error("region layout mismatch")]
    LayoutMismatch,
    #[error("ABI error: {0:?}")]
    Abi(selium_abi::AbiErrorCode),
    #[error("guest error: {0}")]
    Guest(String),
    #[error("index out of bounds")]
    IndexOutOfBounds,
    #[error("subscriber data was overwritten: publisher advanced past ring capacity")]
    Overwritten,
    #[error("backpressure not supported on this transport")]
    BackpressureNotSupported,
    #[error("transport error: {0}")]
    Transport(String),
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        // Try to recover the original Error if it was wrapped via io::Error::other.
        if let Some(our_err) = err.get_ref().and_then(|e| e.downcast_ref::<Error>()) {
            return our_err.clone();
        }
        Self::Transport(err.to_string())
    }
}

impl From<selium_memory::MemoryError> for Error {
    fn from(err: selium_memory::MemoryError) -> Self {
        match err {
            selium_memory::MemoryError::CapacityExceeded => Self::CapacityExceeded,
            selium_memory::MemoryError::IndexOutOfBounds => Self::IndexOutOfBounds,
            selium_memory::MemoryError::InvalidLayout => Self::InvalidLayout,
            selium_memory::MemoryError::CorruptedHeader => {
                Self::InvalidFrame("header checksum mismatch".to_string())
            }
            selium_memory::MemoryError::ProviderNotSet => Self::InvalidRegion,
            selium_memory::MemoryError::RegionNotFound(_) => Self::InvalidRegion,
            selium_memory::MemoryError::Other(msg) => Self::Guest(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_covers_core_error_variants() {
        assert_eq!(Error::InvalidLayout.to_string(), "invalid layout");
        assert_eq!(Error::BufferFull.to_string(), "buffer full");
        assert_eq!(Error::BufferEmpty.to_string(), "buffer empty");
        assert_eq!(
            Error::ReaderBehind.to_string(),
            "reader was overtaken by writers"
        );
        assert_eq!(
            Error::InvalidFrame("bad header".to_string()).to_string(),
            "invalid frame header: bad header"
        );
        assert_eq!(
            Error::TornFrame { position: 42 }.to_string(),
            "torn frame header at position 42: checksum mismatch"
        );
        assert_eq!(Error::CapacityExceeded.to_string(), "capacity exceeded");
        assert_eq!(Error::ChannelClosed.to_string(), "channel closed");
        assert_eq!(Error::ConnectionLost.to_string(), "connection lost");
        assert_eq!(Error::Terminated.to_string(), "channel terminated");
        assert_eq!(
            Error::SerializationFailed("bad".to_string()).to_string(),
            "serialization error: bad"
        );
        assert_eq!(Error::InvalidRegion.to_string(), "invalid shared region");
        assert_eq!(Error::LayoutMismatch.to_string(), "region layout mismatch");
        assert_eq!(
            Error::Guest("failed".to_string()).to_string(),
            "guest error: failed"
        );
        assert_eq!(Error::IndexOutOfBounds.to_string(), "index out of bounds");
        assert_eq!(
            Error::Overwritten.to_string(),
            "subscriber data was overwritten: publisher advanced past ring capacity"
        );
    }

    #[test]
    fn flat_error_matching() {
        let err = Error::BufferFull;
        match err {
            Error::BufferFull => {}
            _ => panic!("expected BufferFull"),
        }
    }
}
