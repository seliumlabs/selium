//! Flatbuffers protocol helpers for the switchboard control plane.

use std::convert::TryFrom;

use flatbuffers::{FlatBufferBuilder, InvalidFlatbuffer};
use thiserror::Error;

/// Generated Flatbuffers bindings for the switchboard protocol.
#[allow(missing_docs)]
#[allow(warnings)]
#[rustfmt::skip]
pub mod fbs;

use crate::fbs::selium::switchboard as fb;

/// Switchboard endpoint identifier.
pub type EndpointId = u32;
/// Schema identifier carried by endpoints (16-byte BLAKE3 hash).
pub type SchemaId = [u8; 16];

/// Cardinality constraint for endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cardinality {
    /// No channels are permitted.
    Zero,
    /// At most one channel may be attached.
    One,
    /// Any number of channels may be attached.
    Many,
}

/// Direction metadata for an endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Direction {
    schema_id: SchemaId,
    cardinality: Cardinality,
}

/// Input/output directions for an endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointDirections {
    input: Direction,
    output: Direction,
}

/// Inbound wiring update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WiringIngress {
    /// Endpoint producing this flow.
    pub from: EndpointId,
    /// Shared channel handle used for this flow.
    pub channel: u64,
}

/// Outbound wiring update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WiringEgress {
    /// Endpoint consuming this flow.
    pub to: EndpointId,
    /// Shared channel handle used for this flow.
    pub channel: u64,
}

/// Switchboard protocol message envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    /// Register a new endpoint with the switchboard.
    RegisterRequest {
        /// Correlation identifier supplied by the client.
        request_id: u64,
        /// Endpoint directions.
        directions: EndpointDirections,
        /// Shared handle for the client's update channel.
        updates_channel: u64,
    },
    /// Connect two endpoints.
    ConnectRequest {
        /// Correlation identifier supplied by the client.
        request_id: u64,
        /// Producer endpoint identifier.
        from: EndpointId,
        /// Consumer endpoint identifier.
        to: EndpointId,
        /// Shared handle for the client's update channel.
        reply_channel: u64,
    },
    /// Register response carrying the allocated endpoint id.
    ResponseRegister {
        /// Correlation identifier supplied by the client.
        request_id: u64,
        /// Endpoint identifier assigned by the switchboard.
        endpoint_id: EndpointId,
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
        /// Error message supplied by the switchboard.
        message: String,
    },
    /// Wiring update for a single endpoint.
    WiringUpdate {
        /// Endpoint identifier receiving this update.
        endpoint_id: EndpointId,
        /// Inbound connections.
        inbound: Vec<WiringIngress>,
        /// Outbound connections.
        outbound: Vec<WiringEgress>,
    },
}

/// Errors produced while encoding or decoding switchboard messages.
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// Flatbuffers payload failed to verify.
    #[error("invalid flatbuffer: {0:?}")]
    InvalidFlatbuffer(InvalidFlatbuffer),
    /// Message payload was not present.
    #[error("switchboard message missing payload")]
    MissingPayload,
    /// Message payload type is unsupported.
    #[error("unknown switchboard payload type")]
    UnknownPayload,
    /// Schema identifier was missing.
    #[error("missing schema identifier")]
    MissingSchemaId,
    /// Schema identifier was not 16 bytes.
    #[error("schema identifier length mismatch")]
    InvalidSchemaId,
    /// Cardinality variant was not recognised.
    #[error("unknown cardinality variant")]
    UnknownCardinality,
    /// Switchboard message identifier did not match.
    #[error("invalid switchboard message identifier")]
    InvalidIdentifier,
}

const SWITCHBOARD_IDENTIFIER: &str = "SBSW";

impl Cardinality {
    /// Returns true if the provided count is permitted.
    pub fn allows(self, count: usize) -> bool {
        match self {
            Cardinality::Zero => count == 0,
            Cardinality::One => count <= 1,
            Cardinality::Many => true,
        }
    }
}

impl Direction {
    /// Create a new direction with the supplied schema and cardinality.
    pub fn new(schema_id: SchemaId, cardinality: Cardinality) -> Self {
        Self {
            schema_id,
            cardinality,
        }
    }

    /// Schema identifier associated with this direction.
    pub fn schema_id(&self) -> SchemaId {
        self.schema_id
    }

    /// Cardinality constraint for this direction.
    pub fn cardinality(&self) -> Cardinality {
        self.cardinality
    }
}

impl EndpointDirections {
    /// Create a new pair of input/output directions.
    pub fn new(input: Direction, output: Direction) -> Self {
        Self { input, output }
    }

    /// Inbound direction for the endpoint.
    pub fn input(&self) -> &Direction {
        &self.input
    }

    /// Outbound direction for the endpoint.
    pub fn output(&self) -> &Direction {
        &self.output
    }
}

impl TryFrom<fb::Cardinality> for Cardinality {
    type Error = ProtocolError;

    fn try_from(value: fb::Cardinality) -> Result<Self, Self::Error> {
        match value {
            fb::Cardinality::Zero => Ok(Cardinality::Zero),
            fb::Cardinality::One => Ok(Cardinality::One),
            fb::Cardinality::Many => Ok(Cardinality::Many),
            _ => Err(ProtocolError::UnknownCardinality),
        }
    }
}

impl From<Cardinality> for fb::Cardinality {
    fn from(value: Cardinality) -> Self {
        match value {
            Cardinality::Zero => fb::Cardinality::Zero,
            Cardinality::One => fb::Cardinality::One,
            Cardinality::Many => fb::Cardinality::Many,
        }
    }
}

impl From<InvalidFlatbuffer> for ProtocolError {
    fn from(value: InvalidFlatbuffer) -> Self {
        ProtocolError::InvalidFlatbuffer(value)
    }
}

/// Encode a switchboard message to Flatbuffers bytes.
pub fn encode_message(message: &Message) -> Result<Vec<u8>, ProtocolError> {
    let mut builder = FlatBufferBuilder::new();
    let (request_id, payload_type, payload) = match message {
        Message::RegisterRequest {
            request_id,
            directions,
            updates_channel,
        } => {
            let directions = encode_directions(&mut builder, directions);
            let payload = fb::RegisterRequest::create(
                &mut builder,
                &fb::RegisterRequestArgs {
                    directions: Some(directions),
                    updates_channel: *updates_channel,
                },
            );
            (
                *request_id,
                fb::SwitchboardPayload::RegisterRequest,
                Some(payload.as_union_value()),
            )
        }
        Message::ConnectRequest {
            request_id,
            from,
            to,
            reply_channel,
        } => {
            let payload = fb::ConnectRequest::create(
                &mut builder,
                &fb::ConnectRequestArgs {
                    from: *from,
                    to: *to,
                    reply_channel: *reply_channel,
                },
            );
            (
                *request_id,
                fb::SwitchboardPayload::ConnectRequest,
                Some(payload.as_union_value()),
            )
        }
        Message::ResponseRegister {
            request_id,
            endpoint_id,
        } => {
            let payload = fb::RegisterResponse::create(
                &mut builder,
                &fb::RegisterResponseArgs {
                    endpoint_id: *endpoint_id,
                },
            );
            (
                *request_id,
                fb::SwitchboardPayload::RegisterResponse,
                Some(payload.as_union_value()),
            )
        }
        Message::ResponseOk { request_id } => {
            let payload = fb::OkResponse::create(&mut builder, &fb::OkResponseArgs {});
            (
                *request_id,
                fb::SwitchboardPayload::OkResponse,
                Some(payload.as_union_value()),
            )
        }
        Message::ResponseError {
            request_id,
            message,
        } => {
            let msg = builder.create_string(message);
            let payload = fb::ErrorResponse::create(
                &mut builder,
                &fb::ErrorResponseArgs { message: Some(msg) },
            );
            (
                *request_id,
                fb::SwitchboardPayload::ErrorResponse,
                Some(payload.as_union_value()),
            )
        }
        Message::WiringUpdate {
            endpoint_id,
            inbound,
            outbound,
        } => {
            let inbound_vec = encode_ingress(&mut builder, inbound);
            let outbound_vec = encode_egress(&mut builder, outbound);
            let payload = fb::WiringUpdate::create(
                &mut builder,
                &fb::WiringUpdateArgs {
                    endpoint_id: *endpoint_id,
                    inbound: Some(inbound_vec),
                    outbound: Some(outbound_vec),
                },
            );
            (
                0,
                fb::SwitchboardPayload::WiringUpdate,
                Some(payload.as_union_value()),
            )
        }
    };

    let message = fb::SwitchboardMessage::create(
        &mut builder,
        &fb::SwitchboardMessageArgs {
            request_id,
            payload_type,
            payload,
        },
    );
    builder.finish(message, Some(SWITCHBOARD_IDENTIFIER));
    Ok(builder.finished_data().to_vec())
}

/// Decode Flatbuffers bytes into a switchboard message.
pub fn decode_message(bytes: &[u8]) -> Result<Message, ProtocolError> {
    if !fb::switchboard_message_buffer_has_identifier(bytes) {
        return Err(ProtocolError::InvalidIdentifier);
    }
    let message = flatbuffers::root::<fb::SwitchboardMessage>(bytes)?;

    match message.payload_type() {
        fb::SwitchboardPayload::RegisterRequest => {
            let req = message
                .payload_as_register_request()
                .ok_or(ProtocolError::MissingPayload)?;
            let directions =
                decode_directions(req.directions().ok_or(ProtocolError::MissingPayload)?)?;
            Ok(Message::RegisterRequest {
                request_id: message.request_id(),
                directions,
                updates_channel: req.updates_channel(),
            })
        }
        fb::SwitchboardPayload::ConnectRequest => {
            let req = message
                .payload_as_connect_request()
                .ok_or(ProtocolError::MissingPayload)?;
            Ok(Message::ConnectRequest {
                request_id: message.request_id(),
                from: req.from(),
                to: req.to(),
                reply_channel: req.reply_channel(),
            })
        }
        fb::SwitchboardPayload::RegisterResponse => {
            let resp = message
                .payload_as_register_response()
                .ok_or(ProtocolError::MissingPayload)?;
            Ok(Message::ResponseRegister {
                request_id: message.request_id(),
                endpoint_id: resp.endpoint_id(),
            })
        }
        fb::SwitchboardPayload::OkResponse => Ok(Message::ResponseOk {
            request_id: message.request_id(),
        }),
        fb::SwitchboardPayload::ErrorResponse => {
            let resp = message
                .payload_as_error_response()
                .ok_or(ProtocolError::MissingPayload)?;
            Ok(Message::ResponseError {
                request_id: message.request_id(),
                message: resp.message().unwrap_or_default().to_string(),
            })
        }
        fb::SwitchboardPayload::WiringUpdate => {
            let update = message
                .payload_as_wiring_update()
                .ok_or(ProtocolError::MissingPayload)?;
            Ok(Message::WiringUpdate {
                endpoint_id: update.endpoint_id(),
                inbound: decode_ingress(update.inbound())?,
                outbound: decode_egress(update.outbound())?,
            })
        }
        _ => Err(ProtocolError::UnknownPayload),
    }
}

fn encode_directions<'bldr>(
    builder: &mut FlatBufferBuilder<'bldr>,
    directions: &EndpointDirections,
) -> flatbuffers::WIPOffset<fb::EndpointDirections<'bldr>> {
    let input = encode_direction(builder, directions.input());
    let output = encode_direction(builder, directions.output());
    fb::EndpointDirections::create(
        builder,
        &fb::EndpointDirectionsArgs {
            input: Some(input),
            output: Some(output),
        },
    )
}

fn encode_direction<'bldr>(
    builder: &mut FlatBufferBuilder<'bldr>,
    direction: &Direction,
) -> flatbuffers::WIPOffset<fb::Direction<'bldr>> {
    let schema_id = builder.create_vector(&direction.schema_id());
    fb::Direction::create(
        builder,
        &fb::DirectionArgs {
            schema_id: Some(schema_id),
            cardinality: direction.cardinality().into(),
        },
    )
}

fn encode_ingress<'bldr>(
    builder: &mut FlatBufferBuilder<'bldr>,
    inbound: &[WiringIngress],
) -> flatbuffers::WIPOffset<
    flatbuffers::Vector<'bldr, flatbuffers::ForwardsUOffset<fb::WiringIngress<'bldr>>>,
> {
    let items: Vec<_> = inbound
        .iter()
        .map(|ingress| {
            fb::WiringIngress::create(
                builder,
                &fb::WiringIngressArgs {
                    from: ingress.from,
                    channel: ingress.channel,
                },
            )
        })
        .collect();
    builder.create_vector(&items)
}

fn encode_egress<'bldr>(
    builder: &mut FlatBufferBuilder<'bldr>,
    outbound: &[WiringEgress],
) -> flatbuffers::WIPOffset<
    flatbuffers::Vector<'bldr, flatbuffers::ForwardsUOffset<fb::WiringEgress<'bldr>>>,
> {
    let items: Vec<_> = outbound
        .iter()
        .map(|egress| {
            fb::WiringEgress::create(
                builder,
                &fb::WiringEgressArgs {
                    to: egress.to,
                    channel: egress.channel,
                },
            )
        })
        .collect();
    builder.create_vector(&items)
}

fn decode_directions(
    directions: fb::EndpointDirections<'_>,
) -> Result<EndpointDirections, ProtocolError> {
    let input = decode_direction(directions.input().ok_or(ProtocolError::MissingPayload)?)?;
    let output = decode_direction(directions.output().ok_or(ProtocolError::MissingPayload)?)?;
    Ok(EndpointDirections::new(input, output))
}

fn decode_direction(direction: fb::Direction<'_>) -> Result<Direction, ProtocolError> {
    let schema_id = decode_schema_id(direction.schema_id())?;
    let cardinality = Cardinality::try_from(direction.cardinality())?;
    Ok(Direction::new(schema_id, cardinality))
}

fn decode_schema_id(
    schema_id: Option<flatbuffers::Vector<'_, u8>>,
) -> Result<SchemaId, ProtocolError> {
    let vec = schema_id.ok_or(ProtocolError::MissingSchemaId)?;
    if vec.len() != 16 {
        return Err(ProtocolError::InvalidSchemaId);
    }
    let mut out = [0u8; 16];
    for (idx, value) in vec.iter().enumerate() {
        if idx >= out.len() {
            break;
        }
        out[idx] = value;
    }
    Ok(out)
}

fn decode_ingress(
    inbound: Option<flatbuffers::Vector<'_, flatbuffers::ForwardsUOffset<fb::WiringIngress<'_>>>>,
) -> Result<Vec<WiringIngress>, ProtocolError> {
    let mut items = Vec::new();
    if let Some(vec) = inbound {
        for ingress in vec {
            items.push(WiringIngress {
                from: ingress.from(),
                channel: ingress.channel(),
            });
        }
    }
    Ok(items)
}

fn decode_egress(
    outbound: Option<flatbuffers::Vector<'_, flatbuffers::ForwardsUOffset<fb::WiringEgress<'_>>>>,
) -> Result<Vec<WiringEgress>, ProtocolError> {
    let mut items = Vec::new();
    if let Some(vec) = outbound {
        for egress in vec {
            items.push(WiringEgress {
                to: egress.to(),
                channel: egress.channel(),
            });
        }
    }
    Ok(items)
}
