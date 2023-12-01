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

/// Constructs a [ClientBuilder] in its initial state. Prefer invoking this function over
/// explicitly constructing a [ClientBuilder].
///
/// ```
/// let client = selium::custom();
/// ```
pub fn custom() -> ClientBuilder<CustomWantsEndpoint> {
    ClientBuilder {
        state: CustomWantsEndpoint::default(),
    }
}

/// Constructs a [ClientBuilder] in its initial state. Prefer invoking this function over
/// explicitly constructing a [ClientBuilder].
pub fn cloud() -> ClientBuilder<CloudWantsCertAndKey> {
    ClientBuilder {
        state: CloudWantsCertAndKey::default(),
    }
}

/// A client containing an authenticated connection to the `Selium` server.
///
/// The [Client] struct is the entry point to opening various `Selium` streams, such as the
/// [Publisher](crate::Publisher) and [Subscriber](crate::Subscriber) stream types.
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
