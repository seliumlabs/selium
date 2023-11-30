use quinn::{ConnectError, ConnectionError, WriteError};
use std::net::AddrParseError;
use thiserror::Error;

pub type Result<T, E = SeliumError> = std::result::Result<T, E>;

#[derive(Error, Debug)]
pub enum CryptoError {
    #[error("Failed to read private key from file.")]
    OpenKeyFileError(#[source] std::io::Error),

    #[error("Malformed PKCS #1 private key.")]
    MalformedPKCS1PrivateKey(#[source] std::io::Error),

    #[error("Malformed PKCS #8 private key.")]
    MalformedPKCS8PrivateKey(#[source] std::io::Error),

    #[error("No private keys found in file.")]
    NoPrivateKeysFound,

    #[error("Failed to read certificate chain from file.")]
    OpenCertFileError(#[source] std::io::Error),

    #[error("Invalid PEM-encoded certificate")]
    InvalidPemCertificate(#[source] std::io::Error),

    #[error("No valid root cert found in file.")]
    InvalidRootCert,
}

#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("Payload size ({0} bytes) is greater than maximum allowed size ({1} bytes).")]
    PayloadTooLarge(u64, u64),

    #[error("Unknown message type: {0}")]
    UnknownMessageType(u8),

    #[error("Failed to serialize/deserialize message on protocol.")]
    SerdeError(#[source] bincode::Error),
}

#[derive(Error, Debug)]
pub enum CodecError {
    #[error("Failed to compress payload.")]
    CompressFailure(#[source] anyhow::Error),

    #[error("Failed to decompress payload.")]
    DecompressFailure(#[source] anyhow::Error),

    #[error("Failed to encode message payload.")]
    EncodeFailure(#[source] anyhow::Error),

    #[error("Failed to decode message frame.")]
    DecodeFailure(#[source] anyhow::Error),
}

#[derive(Error, Debug)]
pub enum QuicError {
    #[error("Error sending message to topic.")]
    WriteError(#[from] WriteError),

    #[error("Failed to establish connection.")]
    ConnectError(#[from] ConnectError),

    #[error("An error occured on an existing connection.")]
    ConnectionError(#[from] ConnectionError),

    #[error("Too many connection retries.")]
    TooManyRetries,
}

#[derive(Error, Debug)]
pub enum ParseRemoteAddressError {
    #[error("Missing remote address port.")]
    MissingPort,

    #[error("Poorly formatted address.")]
    InvalidAddress(#[source] std::io::Error),

    #[error("Couldn't resolve an address.")]
    NoAddressResolved,
}

#[derive(Error, Debug)]
pub enum ParseCertificateHostError {
    #[error("No host address could be resolved.")]
    InvalidHostAddress,
}

#[derive(Error, Debug)]
pub enum ParseEndpointAddressError {
    #[error("Invalid endpoint address.")]
    InvalidAddress(#[source] AddrParseError),
}

#[derive(Error, Debug)]
pub enum SeliumError {
    #[error(transparent)]
    Quic(#[from] QuicError),

    #[error(transparent)]
    Crypto(#[from] CryptoError),

    #[error(transparent)]
    ParseEndpointAddress(#[from] ParseEndpointAddressError),

    #[error(transparent)]
    ParseRemoteAddress(#[from] ParseRemoteAddressError),

    #[error(transparent)]
    ParseCertificateHost(#[from] ParseCertificateHostError),

    #[error("Failed to parse milliseconds from duration.")]
    ParseDurationMillis,

    #[error(transparent)]
    Codec(#[from] CodecError),

    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    #[error("Request failed.")]
    RequestFailed,

    #[error("The request timed out before receiving a reply.")]
    RequestTimeout,

    #[error(transparent)]
    IoError(#[from] std::io::Error),
}
