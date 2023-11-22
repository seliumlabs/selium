use quinn::{ConnectError, ConnectionError, WriteError};
use std::net::AddrParseError;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, SeliumError>;

#[derive(Error, Debug)]
pub enum SeliumError {
    #[error("Error sending message to topic.")]
    WriteError(#[from] WriteError),

    #[error("Failed to establish connection.")]
    ConnectError(#[from] ConnectError),

    #[error("An error occured on an existing connection.")]
    ConnectionError(#[from] ConnectionError),

    #[error("Payload size ({0} bytes) is greater than maximum allowed size ({1} bytes).")]
    PayloadTooLarge(u64, u64),

    #[error("Unknown message type: {0}")]
    UnknownMessageType(u8),

    #[error("Failed to compress payload.")]
    CompressionFailure,

    #[error("Failed to decompress payload.")]
    DecompressionFailure,

    #[error("Failed to encode message payload.")]
    EncodeFailure,

    #[error("Failed to decode message frame.")]
    DecodeFailure,

    #[error("Failed to serialize/deserialize message on protocol.")]
    ProtocolSerdeError(#[from] bincode::Error),

    #[error("Failed to load keys from file: {0}")]
    InvalidKeys(&'static str),

    #[error("Failed to load certs from file: {0}")]
    InvalidCerts(&'static str),

    #[error("No valid root cert found in file.")]
    InvalidRootCert,

    #[error("Too many connection retries.")]
    TooManyRetries,

    #[error("Failed to parse valid endpoint address.")]
    ParseEndpointAddressError(#[source] AddrParseError),

    #[error("Failed to parse valid socket address.")]
    ParseSocketAddressError,

    #[error("Failed to parse millis from duration.")]
    ParseDurationMillis,

    #[error("Unexpected IO error.")]
    IoError(#[from] std::io::Error),
}
