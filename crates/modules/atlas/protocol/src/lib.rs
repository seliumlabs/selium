//! Flatbuffers protocol helpers for the Atlas control plane.

use flatbuffers::{FlatBufferBuilder, InvalidFlatbuffer};
use thiserror::Error;

/// Generated Flatbuffers bindings for the Atlas protocol.
#[allow(missing_docs)]
#[allow(warnings)]
#[rustfmt::skip]
pub mod fbs;
/// URI parsing and Flatbuffers helpers.
pub mod uri;

use crate::fbs::atlas::{protocol as fb, uri as fb_uri};
use crate::uri::Uri;

/// Id type stored in the Atlas.
pub type AtlasId = u64;

const ATLAS_IDENTIFIER: &str = "ATLS";

/// Atlas protocol message envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// Retrieve a single Atlas entry by URI.
    GetRequest {
        /// Correlation identifier supplied by the client.
        request_id: u64,
        /// URI for the lookup.
        uri: Uri,
        /// Shared handle for the response channel.
        reply_channel: u64,
    },
    /// Insert or update an Atlas entry.
    InsertRequest {
        /// Correlation identifier supplied by the client.
        request_id: u64,
        /// URI to insert.
        uri: Uri,
        /// Value to store against the URI.
        id: AtlasId,
        /// Shared handle for the response channel.
        reply_channel: u64,
    },
    /// Remove an Atlas entry by URI.
    RemoveRequest {
        /// Correlation identifier supplied by the client.
        request_id: u64,
        /// URI for the removal.
        uri: Uri,
        /// Shared handle for the response channel.
        reply_channel: u64,
    },
    /// Lookup Atlas entries matching the supplied URI pattern.
    LookupRequest {
        /// Correlation identifier supplied by the client.
        request_id: u64,
        /// Pattern used to match stored URIs.
        pattern: String,
        /// Shared handle for the response channel.
        reply_channel: u64,
    },
    /// Response to a get request.
    ResponseGet {
        /// Correlation identifier supplied by the client.
        request_id: u64,
        /// Stored id, if found.
        id: AtlasId,
        /// Whether the entry existed.
        found: bool,
    },
    /// Response to a remove request.
    ResponseRemove {
        /// Correlation identifier supplied by the client.
        request_id: u64,
        /// Removed id, if found.
        id: AtlasId,
        /// Whether the entry existed.
        found: bool,
    },
    /// Response to a lookup request.
    ResponseLookup {
        /// Correlation identifier supplied by the client.
        request_id: u64,
        /// Matching Atlas identifiers.
        ids: Vec<AtlasId>,
    },
    /// Empty response acknowledging a request.
    ResponseOk {
        /// Correlation identifier supplied by the client.
        request_id: u64,
    },
    /// Error response for a request.
    ResponseError {
        /// Correlation identifier supplied by the client.
        request_id: u64,
        /// Error message supplied by the Atlas service.
        message: String,
    },
}

/// Errors produced while encoding or decoding Atlas messages.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// Flatbuffers payload failed to verify.
    #[error("invalid flatbuffer: {0:?}")]
    InvalidFlatbuffer(InvalidFlatbuffer),
    /// Message payload was not present.
    #[error("atlas message missing payload")]
    MissingPayload,
    /// Message payload type is unsupported.
    #[error("unknown atlas payload type")]
    UnknownPayload,
    /// Atlas message identifier did not match.
    #[error("invalid atlas message identifier")]
    InvalidIdentifier,
}

impl From<InvalidFlatbuffer> for ProtocolError {
    fn from(value: InvalidFlatbuffer) -> Self {
        ProtocolError::InvalidFlatbuffer(value)
    }
}

impl Message {
    /// Return the request identifier associated with this message.
    pub fn request_id(&self) -> u64 {
        match self {
            Message::GetRequest { request_id, .. }
            | Message::InsertRequest { request_id, .. }
            | Message::RemoveRequest { request_id, .. }
            | Message::LookupRequest { request_id, .. }
            | Message::ResponseGet { request_id, .. }
            | Message::ResponseRemove { request_id, .. }
            | Message::ResponseLookup { request_id, .. }
            | Message::ResponseOk { request_id }
            | Message::ResponseError { request_id, .. } => *request_id,
        }
    }
}

/// Encode an Atlas message to Flatbuffers bytes.
pub fn encode_message(message: &Message) -> Result<Vec<u8>, ProtocolError> {
    let mut builder = FlatBufferBuilder::new();
    let (request_id, payload_type, payload) = match message {
        Message::GetRequest {
            request_id,
            uri,
            reply_channel,
        } => {
            let uri = encode_uri(&mut builder, uri);
            let payload = fb::GetRequest::create(
                &mut builder,
                &fb::GetRequestArgs {
                    uri: Some(uri),
                    reply_channel: *reply_channel,
                },
            );
            (
                *request_id,
                fb::AtlasPayload::GetRequest,
                Some(payload.as_union_value()),
            )
        }
        Message::InsertRequest {
            request_id,
            uri,
            id,
            reply_channel,
        } => {
            let uri = encode_uri(&mut builder, uri);
            let payload = fb::InsertRequest::create(
                &mut builder,
                &fb::InsertRequestArgs {
                    uri: Some(uri),
                    id: *id,
                    reply_channel: *reply_channel,
                },
            );
            (
                *request_id,
                fb::AtlasPayload::InsertRequest,
                Some(payload.as_union_value()),
            )
        }
        Message::RemoveRequest {
            request_id,
            uri,
            reply_channel,
        } => {
            let uri = encode_uri(&mut builder, uri);
            let payload = fb::RemoveRequest::create(
                &mut builder,
                &fb::RemoveRequestArgs {
                    uri: Some(uri),
                    reply_channel: *reply_channel,
                },
            );
            (
                *request_id,
                fb::AtlasPayload::RemoveRequest,
                Some(payload.as_union_value()),
            )
        }
        Message::LookupRequest {
            request_id,
            pattern,
            reply_channel,
        } => {
            let pattern = builder.create_string(pattern);
            let payload = fb::LookupRequest::create(
                &mut builder,
                &fb::LookupRequestArgs {
                    pattern: Some(pattern),
                    reply_channel: *reply_channel,
                },
            );
            (
                *request_id,
                fb::AtlasPayload::LookupRequest,
                Some(payload.as_union_value()),
            )
        }
        Message::ResponseGet {
            request_id,
            id,
            found,
        } => {
            let payload = fb::GetResponse::create(
                &mut builder,
                &fb::GetResponseArgs {
                    id: *id,
                    found: *found,
                },
            );
            (
                *request_id,
                fb::AtlasPayload::GetResponse,
                Some(payload.as_union_value()),
            )
        }
        Message::ResponseRemove {
            request_id,
            id,
            found,
        } => {
            let payload = fb::RemoveResponse::create(
                &mut builder,
                &fb::RemoveResponseArgs {
                    id: *id,
                    found: *found,
                },
            );
            (
                *request_id,
                fb::AtlasPayload::RemoveResponse,
                Some(payload.as_union_value()),
            )
        }
        Message::ResponseLookup { request_id, ids } => {
            let ids = builder.create_vector(ids);
            let payload = fb::LookupResponse::create(
                &mut builder,
                &fb::LookupResponseArgs { ids: Some(ids) },
            );
            (
                *request_id,
                fb::AtlasPayload::LookupResponse,
                Some(payload.as_union_value()),
            )
        }
        Message::ResponseOk { request_id } => {
            let payload = fb::OkResponse::create(&mut builder, &fb::OkResponseArgs {});
            (
                *request_id,
                fb::AtlasPayload::OkResponse,
                Some(payload.as_union_value()),
            )
        }
        Message::ResponseError {
            request_id,
            message,
        } => {
            let message = builder.create_string(message);
            let payload = fb::ErrorResponse::create(
                &mut builder,
                &fb::ErrorResponseArgs {
                    message: Some(message),
                },
            );
            (
                *request_id,
                fb::AtlasPayload::ErrorResponse,
                Some(payload.as_union_value()),
            )
        }
    };

    let message = fb::AtlasMessage::create(
        &mut builder,
        &fb::AtlasMessageArgs {
            request_id,
            payload_type,
            payload,
        },
    );
    builder.finish(message, Some(ATLAS_IDENTIFIER));
    Ok(builder.finished_data().to_vec())
}

/// Decode Flatbuffers bytes into an Atlas message.
pub fn decode_message(bytes: &[u8]) -> Result<Message, ProtocolError> {
    if !fb::atlas_message_buffer_has_identifier(bytes) {
        return Err(ProtocolError::InvalidIdentifier);
    }
    let message = flatbuffers::root::<fb::AtlasMessage>(bytes)?;

    match message.payload_type() {
        fb::AtlasPayload::GetRequest => {
            let req = message
                .payload_as_get_request()
                .ok_or(ProtocolError::MissingPayload)?;
            let uri = decode_uri(req.uri().ok_or(ProtocolError::MissingPayload)?);
            Ok(Message::GetRequest {
                request_id: message.request_id(),
                uri,
                reply_channel: req.reply_channel(),
            })
        }
        fb::AtlasPayload::InsertRequest => {
            let req = message
                .payload_as_insert_request()
                .ok_or(ProtocolError::MissingPayload)?;
            let uri = decode_uri(req.uri().ok_or(ProtocolError::MissingPayload)?);
            Ok(Message::InsertRequest {
                request_id: message.request_id(),
                uri,
                id: req.id(),
                reply_channel: req.reply_channel(),
            })
        }
        fb::AtlasPayload::RemoveRequest => {
            let req = message
                .payload_as_remove_request()
                .ok_or(ProtocolError::MissingPayload)?;
            let uri = decode_uri(req.uri().ok_or(ProtocolError::MissingPayload)?);
            Ok(Message::RemoveRequest {
                request_id: message.request_id(),
                uri,
                reply_channel: req.reply_channel(),
            })
        }
        fb::AtlasPayload::LookupRequest => {
            let req = message
                .payload_as_lookup_request()
                .ok_or(ProtocolError::MissingPayload)?;
            let pattern = req
                .pattern()
                .ok_or(ProtocolError::MissingPayload)?
                .to_string();
            Ok(Message::LookupRequest {
                request_id: message.request_id(),
                pattern,
                reply_channel: req.reply_channel(),
            })
        }
        fb::AtlasPayload::GetResponse => {
            let resp = message
                .payload_as_get_response()
                .ok_or(ProtocolError::MissingPayload)?;
            Ok(Message::ResponseGet {
                request_id: message.request_id(),
                id: resp.id(),
                found: resp.found(),
            })
        }
        fb::AtlasPayload::RemoveResponse => {
            let resp = message
                .payload_as_remove_response()
                .ok_or(ProtocolError::MissingPayload)?;
            Ok(Message::ResponseRemove {
                request_id: message.request_id(),
                id: resp.id(),
                found: resp.found(),
            })
        }
        fb::AtlasPayload::LookupResponse => {
            let resp = message
                .payload_as_lookup_response()
                .ok_or(ProtocolError::MissingPayload)?;
            Ok(Message::ResponseLookup {
                request_id: message.request_id(),
                ids: decode_ids(resp.ids()),
            })
        }
        fb::AtlasPayload::OkResponse => Ok(Message::ResponseOk {
            request_id: message.request_id(),
        }),
        fb::AtlasPayload::ErrorResponse => {
            let resp = message
                .payload_as_error_response()
                .ok_or(ProtocolError::MissingPayload)?;
            let error_message = resp
                .message()
                .ok_or(ProtocolError::MissingPayload)?
                .to_string();
            Ok(Message::ResponseError {
                request_id: message.request_id(),
                message: error_message,
            })
        }
        _ => Err(ProtocolError::UnknownPayload),
    }
}

fn encode_uri<'bldr>(
    builder: &mut FlatBufferBuilder<'bldr>,
    uri: &Uri,
) -> flatbuffers::WIPOffset<fb_uri::Uri<'bldr>> {
    uri.write_flatbuffer(builder)
}

fn decode_uri(table: fb_uri::Uri<'_>) -> Uri {
    Uri::from_flatbuffer_table(table)
}

fn decode_ids(ids: Option<flatbuffers::Vector<'_, AtlasId>>) -> Vec<AtlasId> {
    let mut out = Vec::new();
    if let Some(vec) = ids {
        out.extend(vec.iter());
    }
    out
}
