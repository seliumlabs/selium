//! Guest-side switchboard helpers and re-exports.

pub mod messaging;
pub mod switchboard;

/// Messaging helpers built on the switchboard.
pub use messaging::{
    Client, ClientTarget, Fanout, Publisher, PublisherTarget, RequestCtx, Responder, Server,
    ServerTarget, Subscriber, SubscriberTarget,
};
/// Flatbuffers protocol types for switchboard control messages.
pub use selium_switchboard_protocol as protocol;
/// Switchboard client types for guest code.
pub use switchboard::{
    Cardinality, EndpointBuilder, EndpointHandle, EndpointId, Switchboard, SwitchboardError,
};
