//! Guest-side Atlas helpers and re-exports.

#[cfg(feature = "switchboard")]
use std::future::Future;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use futures::{SinkExt, StreamExt};
/// Flatbuffers protocol helpers for Atlas control messages.
pub use selium_atlas_protocol as protocol;
/// Atlas protocol types for convenience.
pub use selium_atlas_protocol::{AtlasId, uri::Uri};
use selium_atlas_protocol::{Message, ProtocolError, decode_message, encode_message};
#[cfg(feature = "switchboard")]
use selium_switchboard::{
    Client, Fanout, Publisher, Server, Subscriber, Switchboard, SwitchboardError,
};
use selium_userland::io::{Channel, DriverError, SharedChannel};
use thiserror::Error;
use tracing::debug;

const REQUEST_CHUNK_SIZE: u32 = 64 * 1024;
const RESPONSE_CHANNEL_CAPACITY: u32 = 16 * 1024;

/// Convenience extension for wiring publisher results to Atlas matches.
#[cfg(feature = "switchboard")]
pub trait PublisherAtlasExt {
    /// Connect this `Publisher` to any existing subscribers matching the given pattern.
    fn matching(
        &self,
        atlas: &Atlas,
        switchboard: &Switchboard,
        pattern: &str,
    ) -> impl Future<Output = Result<(), AtlasError>>;
}

/// Convenience extension for wiring Atlas matches into a subscriber.
#[cfg(feature = "switchboard")]
pub trait SubscriberAtlasExt {
    /// Connect this `Subscriber` to any existing publishers matching the given pattern.
    fn connect(
        &self,
        atlas: &Atlas,
        switchboard: &Switchboard,
        pattern: &str,
    ) -> impl Future<Output = Result<(), AtlasError>>;
}

/// Convenience extension for wiring Atlas matches into a server.
#[cfg(feature = "switchboard")]
pub trait ServerAtlasExt {
    /// Accept connections from any existing clients matching the given pattern.
    fn accept(
        &self,
        atlas: &Atlas,
        switchboard: &Switchboard,
        pattern: &str,
    ) -> impl Future<Output = Result<(), AtlasError>>;
}

/// Atlas front-end that guests use to look up endpoints.
#[derive(Clone)]
pub struct Atlas {
    request_channel: Channel,
    next_request_id: Arc<AtomicU64>,
}

/// Errors produced by the Atlas client.
#[derive(Error, Debug)]
pub enum AtlasError {
    /// Driver returned an error while creating or destroying channels.
    #[error("driver error: {0}")]
    Driver(#[from] DriverError),
    /// Control-plane protocol could not be decoded.
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    /// The Atlas service returned an error.
    #[error("atlas error: {0}")]
    Remote(String),
    /// The Atlas response channel was closed.
    #[error("endpoint closed")]
    EndpointClosed,
    /// Switchboard error while wiring Atlas results.
    #[cfg(feature = "switchboard")]
    #[error("switchboard error: {0}")]
    Switchboard(#[from] SwitchboardError),
}

impl Atlas {
    /// Connect to the Atlas service using the shared request channel handle.
    pub async fn attach(request_channel: SharedChannel) -> Result<Self, AtlasError> {
        let request_channel = Channel::attach_shared(request_channel).await?;
        Ok(Self {
            request_channel,
            next_request_id: Arc::new(AtomicU64::new(1)),
        })
    }

    /// Retrieve an endpoint by URI.
    pub async fn get(&self, uri: &Uri) -> Result<Option<AtlasId>, AtlasError> {
        let response = self
            .send_request(|request_id, reply_channel| Message::GetRequest {
                request_id,
                uri: uri.clone(),
                reply_channel,
            })
            .await?;

        match response {
            Message::ResponseGet { id, found, .. } => Ok(found.then_some(id)),
            Message::ResponseError { message, .. } => Err(AtlasError::Remote(message)),
            _ => Err(AtlasError::Protocol(ProtocolError::UnknownPayload)),
        }
    }

    /// Add or update an endpoint in the Atlas.
    pub async fn insert(&self, uri: Uri, id: AtlasId) -> Result<(), AtlasError> {
        let response = self
            .send_request(|request_id, reply_channel| Message::InsertRequest {
                request_id,
                uri,
                id,
                reply_channel,
            })
            .await?;

        match response {
            Message::ResponseOk { .. } => Ok(()),
            Message::ResponseError { message, .. } => Err(AtlasError::Remote(message)),
            _ => Err(AtlasError::Protocol(ProtocolError::UnknownPayload)),
        }
    }

    /// Delete an endpoint from the Atlas.
    pub async fn remove(&self, uri: &Uri) -> Result<Option<AtlasId>, AtlasError> {
        let response = self
            .send_request(|request_id, reply_channel| Message::RemoveRequest {
                request_id,
                uri: uri.clone(),
                reply_channel,
            })
            .await?;

        match response {
            Message::ResponseRemove { id, found, .. } => Ok(found.then_some(id)),
            Message::ResponseError { message, .. } => Err(AtlasError::Remote(message)),
            _ => Err(AtlasError::Protocol(ProtocolError::UnknownPayload)),
        }
    }

    /// Lookup endpoints matching a URI pattern.
    pub async fn lookup(&self, pattern: &str) -> Result<Vec<AtlasId>, AtlasError> {
        let response = self
            .send_request(|request_id, reply_channel| Message::LookupRequest {
                request_id,
                pattern: pattern.to_string(),
                reply_channel,
            })
            .await?;

        match response {
            Message::ResponseLookup { ids, .. } => Ok(ids),
            Message::ResponseError { message, .. } => Err(AtlasError::Remote(message)),
            _ => Err(AtlasError::Protocol(ProtocolError::UnknownPayload)),
        }
    }

    async fn send_request<F>(&self, build: F) -> Result<Message, AtlasError>
    where
        F: FnOnce(u64, u64) -> Message,
    {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let response_channel = Channel::create(RESPONSE_CHANNEL_CAPACITY).await?;
        let response_shared = response_channel.share().await?;
        let mut response_reader = response_channel.subscribe(REQUEST_CHUNK_SIZE).await?;

        let message = build(request_id, response_shared.raw());
        debug!(request_id, "atlas: sending request");
        let bytes = encode_message(&message)?;
        let mut writer = self.request_channel.publish_weak().await?;
        writer.send(bytes).await?;
        debug!(request_id, "atlas: request sent");

        let response = match response_reader.next().await {
            Some(Ok(frame)) => decode_message(&frame.payload)?,
            Some(Err(err)) => return Err(AtlasError::Driver(err)),
            None => return Err(AtlasError::EndpointClosed),
        };
        debug!(request_id, "atlas: received response");

        response_channel.delete().await?;

        Ok(response)
    }
}

#[cfg(feature = "switchboard")]
impl<T> PublisherAtlasExt for Fanout<T> {
    async fn matching(
        &self,
        atlas: &Atlas,
        switchboard: &Switchboard,
        pattern: &str,
    ) -> Result<(), AtlasError> {
        let ids = atlas.lookup(pattern).await?;
        for id in ids {
            switchboard
                .connect_ids(self.endpoint_id(), id as u32)
                .await?;
        }

        Ok(())
    }
}

#[cfg(feature = "switchboard")]
impl<T> PublisherAtlasExt for Publisher<T> {
    async fn matching(
        &self,
        atlas: &Atlas,
        switchboard: &Switchboard,
        pattern: &str,
    ) -> Result<(), AtlasError> {
        let ids = atlas.lookup(pattern).await?;
        for id in ids {
            switchboard
                .connect_ids(self.endpoint_id(), id as u32)
                .await?;
        }

        Ok(())
    }
}

#[cfg(feature = "switchboard")]
impl<T> SubscriberAtlasExt for Subscriber<T> {
    async fn connect(
        &self,
        atlas: &Atlas,
        switchboard: &Switchboard,
        pattern: &str,
    ) -> Result<(), AtlasError> {
        let ids = atlas.lookup(pattern).await?;
        for id in ids {
            switchboard
                .connect_ids(id as u32, self.endpoint_id())
                .await?;
        }

        Ok(())
    }
}

#[cfg(feature = "switchboard")]
impl<Req, Rep> SubscriberAtlasExt for Client<Req, Rep> {
    async fn connect(
        &self,
        atlas: &Atlas,
        switchboard: &Switchboard,
        pattern: &str,
    ) -> Result<(), AtlasError> {
        let ids = atlas.lookup(pattern).await?;
        for id in ids {
            let server_id = id as u32;
            switchboard
                .connect_ids(self.endpoint_id(), server_id)
                .await?;
            switchboard
                .connect_ids(server_id, self.endpoint_id())
                .await?;
        }

        Ok(())
    }
}

#[cfg(feature = "switchboard")]
impl<Req, Rep> ServerAtlasExt for Server<Req, Rep> {
    async fn accept(
        &self,
        atlas: &Atlas,
        switchboard: &Switchboard,
        pattern: &str,
    ) -> Result<(), AtlasError> {
        let ids = atlas.lookup(pattern).await?;
        for id in ids {
            let client_id = id as u32;
            switchboard
                .connect_ids(client_id, self.endpoint_id())
                .await?;
            switchboard
                .connect_ids(self.endpoint_id(), client_id)
                .await?;
        }

        Ok(())
    }
}
