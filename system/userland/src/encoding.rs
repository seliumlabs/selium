//! Flatbuffers-centric encoding helpers.
//!
//! Selium guests use Flatbuffers on public wires. This module defines small traits for bridging
//! between idiomatic Rust types and Flatbuffers payloads.
//!
//! # Examples
//! ```
//! use selium_userland::encoding::{FlatMsg, FlatResult};
//!
//! fn main() -> Result<(), flatbuffers::InvalidFlatbuffer> {
//!     let result = FlatResult::ok(9, b"hello".to_vec());
//!     let bytes = FlatMsg::encode(&result);
//!     let decoded: FlatResult = FlatMsg::decode(&bytes)?;
//!     assert_eq!(decoded.request_id(), 9);
//!     Ok(())
//! }
//! ```

use flatbuffers::{FlatBufferBuilder, InvalidFlatbuffer};

use crate::fbs::selium::result::{self as fb};

/// Marker trait linking a Rust type to a Flatbuffers schema.
pub trait HasSchema {
    /// Static schema descriptor used for port metadata.
    const SCHEMA: SchemaDescriptor;
}

/// Flatbuffers-backed message that can be transmitted over an endpoint.
pub trait FlatMsg: Sized {
    /// Encode the owned value into Flatbuffer bytes.
    fn encode(value: &Self) -> Vec<u8>;
    /// Decode the owned value from Flatbuffer bytes.
    fn decode(bytes: &[u8]) -> Result<Self, flatbuffers::InvalidFlatbuffer>;
}

/// Helper for encoding schema fields into Flatbuffers-ready values.
pub trait FieldEncoder {
    /// Output type written into Flatbuffers args or vectors.
    type Output<'bldr>;

    /// Encode the field for Flatbuffers builders.
    fn encode_field<'bldr, A: flatbuffers::Allocator + 'bldr>(
        &self,
        builder: &mut FlatBufferBuilder<'bldr, A>,
    ) -> Self::Output<'bldr>;
}

/// Helper for converting Flatbuffer string accessors into owned `String`s.
pub trait StringFieldValue {
    /// Convert the accessor into an owned `String`.
    fn into_owned(self) -> String;
}

/// Static descriptor describing the schema carried by an endpoint.
#[derive(Clone, Copy, Debug)]
pub struct SchemaDescriptor {
    /// Fully qualified schema name (used for human-friendly diagnostics).
    pub fqname: &'static str,
    /// 16-byte content hash identifying the schema.
    pub hash: [u8; 16],
}

/// Generic success/error envelope transported over channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlatResult {
    /// Successful reply with an opaque payload.
    Ok {
        /// Request identifier used to correlate this result.
        request_id: u64,
        /// Raw payload bytes (typically another Flatbuffers message).
        payload: Vec<u8>,
    },
    /// Error reply with a human-readable message.
    Err {
        /// Request identifier used to correlate this result.
        request_id: u64,
        /// Human-readable error message.
        message: String,
    },
}

impl FlatMsg for () {
    fn encode(_value: &Self) -> Vec<u8> {
        Vec::new()
    }

    fn decode(_bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> {
        Ok(())
    }
}

impl HasSchema for () {
    const SCHEMA: SchemaDescriptor = SchemaDescriptor {
        fqname: "empty_tuple",
        hash: [0; 16],
    };
}

impl FlatMsg for u32 {
    fn encode(value: &Self) -> Vec<u8> {
        value.to_le_bytes().into()
    }

    fn decode(bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> {
        Ok(u32::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| InvalidFlatbuffer::ApparentSizeTooLarge)?,
        ))
    }
}

impl HasSchema for u32 {
    const SCHEMA: SchemaDescriptor = SchemaDescriptor {
        fqname: "unsigned_thirty_two_bit_int",
        hash: [0, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3],
    };
}

impl FlatMsg for i32 {
    fn encode(value: &Self) -> Vec<u8> {
        value.to_le_bytes().into()
    }

    fn decode(bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> {
        Ok(i32::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| InvalidFlatbuffer::ApparentSizeTooLarge)?,
        ))
    }
}

impl HasSchema for i32 {
    const SCHEMA: SchemaDescriptor = SchemaDescriptor {
        fqname: "signed_thirty_two_bit_int",
        hash: [1, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3],
    };
}

impl FlatMsg for String {
    fn encode(value: &Self) -> Vec<u8> {
        value.as_bytes().to_owned()
    }

    fn decode(bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> {
        Ok(str::from_utf8(bytes)
            .map_err(|e| InvalidFlatbuffer::Utf8Error {
                error: e,
                range: 0..bytes.len(),
                error_trace: Default::default(),
            })?
            .to_owned())
    }
}

impl HasSchema for String {
    const SCHEMA: SchemaDescriptor = SchemaDescriptor {
        fqname: "string",
        hash: [1; 16],
    };
}

impl StringFieldValue for &str {
    fn into_owned(self) -> String {
        self.to_string()
    }
}

impl StringFieldValue for Option<&str> {
    fn into_owned(self) -> String {
        self.unwrap_or_default().to_string()
    }
}

impl FlatResult {
    /// Construct a successful result.
    pub fn ok(request_id: u64, payload: Vec<u8>) -> Self {
        Self::Ok {
            request_id,
            payload,
        }
    }

    /// Construct an error result.
    pub fn err(request_id: u64, message: impl Into<String>) -> Self {
        Self::Err {
            request_id,
            message: message.into(),
        }
    }

    /// Request identifier used to correlate this result.
    pub fn request_id(&self) -> u64 {
        match self {
            FlatResult::Ok { request_id, .. } | FlatResult::Err { request_id, .. } => *request_id,
        }
    }
}

impl FlatMsg for FlatResult {
    fn encode(value: &Self) -> Vec<u8> {
        let mut builder = FlatBufferBuilder::new();
        let offset = match value {
            FlatResult::Ok {
                request_id,
                payload,
            } => {
                let payload = builder.create_vector(payload);
                fb::FlatResult::create(
                    &mut builder,
                    &fb::FlatResultArgs {
                        request_id: *request_id,
                        payload: Some(payload),
                        error: None,
                    },
                )
            }
            FlatResult::Err {
                request_id,
                message,
            } => {
                let error = builder.create_string(message);
                fb::FlatResult::create(
                    &mut builder,
                    &fb::FlatResultArgs {
                        request_id: *request_id,
                        payload: None,
                        error: Some(error),
                    },
                )
            }
        };
        builder.finish(offset, Some("SRES"));
        builder.finished_data().to_vec()
    }

    fn decode(bytes: &[u8]) -> Result<Self, flatbuffers::InvalidFlatbuffer> {
        let view = fb::root_as_flat_result(bytes)?;
        if let Some(error) = view.error() {
            Ok(FlatResult::Err {
                request_id: view.request_id(),
                message: error.to_string(),
            })
        } else {
            let payload = view
                .payload()
                .map(|vec| vec.iter().collect())
                .unwrap_or_default();
            Ok(FlatResult::Ok {
                request_id: view.request_id(),
                payload,
            })
        }
    }
}

impl<T> FlatMsg for anyhow::Result<T>
where
    T: FlatMsg,
{
    fn encode(value: &Self) -> Vec<u8> {
        match value {
            Ok(ok) => FlatMsg::encode(&FlatResult::ok(0, FlatMsg::encode(ok))),
            Err(e) => FlatMsg::encode(&FlatResult::err(0, e.to_string())),
        }
    }

    fn decode(bytes: &[u8]) -> Result<Self, InvalidFlatbuffer> {
        let result: FlatResult = FlatMsg::decode(bytes)?;
        match result {
            FlatResult::Ok { payload, .. } => T::decode(&payload).map(Ok),
            FlatResult::Err { message, .. } => Ok(Err(anyhow::anyhow!(message))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_roundtrip() {
        let result = FlatResult::ok(9, vec![4, 5]);
        let bytes = FlatMsg::encode(&result);
        let decoded = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(result, decoded);
    }

    #[test]
    fn error_roundtrip() {
        let result = FlatResult::err(3, "nope");
        let bytes = FlatMsg::encode(&result);
        let decoded = FlatMsg::decode(&bytes).expect("decode");
        assert_eq!(result, decoded);
    }
}
