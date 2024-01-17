mod builder;
mod cloud;
mod custom;

use crate::connection::SharedConnection;
use crate::keep_alive::BackoffStrategy;
use crate::pubsub::states::{PublisherWantsEncoder, SubscriberWantsDecoder};
use crate::request_reply::states::{ReplierWantsRequestDecoder, RequestorWantsRequestEncoder};
use crate::StreamBuilder;

pub use builder::*;
pub use cloud::*;
pub use custom::*;

/// Constructs a Custom [ClientBuilder] in its initial state to prepare to connect to a self-hosted
/// `Selium` server.
///
/// Prefer invoking this function over explicitly constructing a [ClientBuilder].
pub fn custom() -> ClientBuilder<CustomWantsEndpoint> {
    ClientBuilder {
        state: CustomWantsEndpoint::default(),
        client_type: ClientType::Custom,
    }
}

/// Constructs a Cloud [ClientBuilder] in its initial state to prepare to connect to `Selium
/// Cloud`.
///
/// **NOTE:** Selium Cloud is a managed service for Selium, eliminating the need to run and maintain your
/// own Selium Server. If you have registered for an account already, use this endpoint to connect
/// to the Cloud. Otherwise you can create a free account at [selium.com](https://selium.com).
///
/// Prefer invoking this function over explicitly constructing a [ClientBuilder].
pub fn cloud() -> ClientBuilder<CloudWantsCertAndKey> {
    ClientBuilder {
        state: CloudWantsCertAndKey::default(),
        client_type: ClientType::Cloud,
    }
}

/// A client containing an authenticated connection to either `Selium Cloud` or a self-hosted `Selium`
/// server.
///
/// The [Client] struct is the entry point to opening various `Selium` streams, such as the
/// [Pub/Sub](crate::streams::pubsub) streams and [Request/Reply](crate::streams::request_reply) streams.
///
/// Multiple streams can be opened from a single connected [Client] without extinguishing the underlying
/// connection, through the use of [QUIC](https://quicwg.org) multiplexing.
///
/// **NOTE:** The [Client] struct should never be used directly, and is intended to be constructed by a
/// [ClientBuilder], following a successfully established connection to the `Selium` server.
#[derive(Clone)]
pub struct Client {
    pub(crate) connection: SharedConnection,
    pub(crate) backoff_strategy: BackoffStrategy,
}

impl Client {
    /// Returns a new [StreamBuilder](crate::StreamBuilder) instance, with an initial `Subscriber`
    /// state.
    pub fn subscriber(&self, topic: &str) -> StreamBuilder<SubscriberWantsDecoder> {
        StreamBuilder::new(self.clone(), SubscriberWantsDecoder::new(topic))
    }

    /// Returns a new [StreamBuilder](crate::StreamBuilder) instance, with an initial `Publisher`
    /// state.
    pub fn publisher(&self, topic: &str) -> StreamBuilder<PublisherWantsEncoder> {
        StreamBuilder::new(self.clone(), PublisherWantsEncoder::new(topic))
    }

    /// Returns a new [StreamBuilder](crate::StreamBuilder)  instance, with an initial `Replier`
    /// state.
    pub fn replier(&self, endpoint: &str) -> StreamBuilder<ReplierWantsRequestDecoder> {
        StreamBuilder::new(self.clone(), ReplierWantsRequestDecoder::new(endpoint))
    }

    /// Returns a new [StreamBuilder](crate::StreamBuilder) instance, with an initial `Requestor`
    /// state.
    pub fn requestor(&self, endpoint: &str) -> StreamBuilder<RequestorWantsRequestEncoder> {
        StreamBuilder::new(self.clone(), RequestorWantsRequestEncoder::new(endpoint))
    }
}
