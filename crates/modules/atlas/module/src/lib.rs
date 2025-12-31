//! Atlas service module that manages endpoint discovery.

use std::collections::HashMap;

use anyhow::Result;
use futures::{SinkExt, StreamExt};
use selium_atlas_protocol::{
    AtlasId, Message, ProtocolError,
    decode_message, encode_message,
    uri::{Uri, UriError},
};
use selium_userland::{
    entrypoint,
    io::{Channel, DriverError, SharedChannel},
};
use thiserror::Error;
use tracing::{debug, info, instrument, warn};

const REQUEST_CHUNK_SIZE: u32 = 64 * 1024;
const CHANNEL_CAPACITY: u32 = 64 * 1024;

#[derive(Debug, Error)]
enum AtlasServiceError {
    #[error("driver error: {0}")]
    Driver(#[from] DriverError),
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("uri error: {0}")]
    Uri(#[from] UriError),
}

#[derive(Default)]
struct AtlasStore {
    entries: HashMap<Uri, AtlasId>,
}

struct AtlasService {
    store: AtlasStore,
}

impl AtlasStore {
    fn get(&self, uri: &Uri) -> Option<AtlasId> {
        self.entries.get(uri).copied()
    }

    fn insert(&mut self, uri: Uri, id: AtlasId) {
        self.entries.insert(uri, id);
    }

    fn remove(&mut self, uri: &Uri) -> Option<AtlasId> {
        self.entries.remove(uri)
    }

    fn lookup(&self, pattern: &str) -> Result<Vec<AtlasId>, UriError> {
        let test_uri = Uri::parse(pattern)?;
        Ok(self
            .entries
            .iter()
            .filter_map(|(uri, id)| {
                if test_uri.contains(uri) {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect())
    }
}

impl AtlasService {
    fn new() -> Self {
        Self {
            store: AtlasStore::default(),
        }
    }

    async fn handle_payload(&mut self, payload: &[u8]) -> Result<(), AtlasServiceError> {
        let message = decode_message(payload)?;
        self.handle_message(message).await
    }

    async fn handle_message(&mut self, message: Message) -> Result<(), AtlasServiceError> {
        match message {
            Message::GetRequest {
                request_id,
                uri,
                reply_channel,
            } => self.handle_get(request_id, uri, reply_channel).await,
            Message::InsertRequest {
                request_id,
                uri,
                id,
                reply_channel,
            } => {
                self.handle_insert(request_id, uri, id, reply_channel)
                    .await
            }
            Message::RemoveRequest {
                request_id,
                uri,
                reply_channel,
            } => self.handle_remove(request_id, uri, reply_channel).await,
            Message::LookupRequest {
                request_id,
                pattern,
                reply_channel,
            } => self.handle_lookup(request_id, pattern, reply_channel).await,
            _ => Ok(()),
        }
    }

    async fn handle_get(
        &mut self,
        request_id: u64,
        uri: Uri,
        reply_channel: u64,
    ) -> Result<(), AtlasServiceError> {
        let reply_channel = unsafe { SharedChannel::from_raw(reply_channel) };
        let (found, id) = match self.store.get(&uri) {
            Some(id) => (true, id),
            None => (false, 0),
        };

        let response = Message::ResponseGet {
            request_id,
            id,
            found,
        };
        self.send_response(reply_channel, response).await
    }

    async fn handle_insert(
        &mut self,
        request_id: u64,
        uri: Uri,
        id: AtlasId,
        reply_channel: u64,
    ) -> Result<(), AtlasServiceError> {
        let reply_channel = unsafe { SharedChannel::from_raw(reply_channel) };
        self.store.insert(uri, id);
        let response = Message::ResponseOk { request_id };
        self.send_response(reply_channel, response).await
    }

    async fn handle_remove(
        &mut self,
        request_id: u64,
        uri: Uri,
        reply_channel: u64,
    ) -> Result<(), AtlasServiceError> {
        let reply_channel = unsafe { SharedChannel::from_raw(reply_channel) };
        let (found, id) = match self.store.remove(&uri) {
            Some(id) => (true, id),
            None => (false, 0),
        };

        let response = Message::ResponseRemove {
            request_id,
            id,
            found,
        };
        self.send_response(reply_channel, response).await
    }

    async fn handle_lookup(
        &mut self,
        request_id: u64,
        pattern: String,
        reply_channel: u64,
    ) -> Result<(), AtlasServiceError> {
        let reply_channel = unsafe { SharedChannel::from_raw(reply_channel) };

        match self.store.lookup(&pattern) {
            Ok(ids) => {
                let response = Message::ResponseLookup { request_id, ids };
                self.send_response(reply_channel, response).await?;
            }
            Err(err) => {
                self.send_error(reply_channel, request_id, err.into())
                    .await?;
            }
        }

        Ok(())
    }

    async fn send_response(
        &self,
        channel: SharedChannel,
        message: Message,
    ) -> Result<(), AtlasServiceError> {
        let channel = Channel::attach_shared(channel).await?;
        let mut writer = channel.publish_weak().await?;
        let bytes = encode_message(&message)?;
        writer.send(bytes).await?;
        Ok(())
    }

    async fn send_error(
        &self,
        channel: SharedChannel,
        request_id: u64,
        err: AtlasServiceError,
    ) -> Result<(), AtlasServiceError> {
        let response = Message::ResponseError {
            request_id,
            message: err.to_string(),
        };
        self.send_response(channel, response).await
    }
}

/// Entry point for the Atlas module.
#[entrypoint]
#[instrument(name = "atlas.start")]
pub async fn start() -> Result<()> {
    let request_channel = Channel::create(CHANNEL_CAPACITY).await?;
    let shared = request_channel.share().await?;
    info!(
        request_channel = shared.raw(),
        "atlas: created request channel"
    );
    // TODO(@maintainer): Register the shared handle for discovery once the registry path exists.
    let mut reader = request_channel.subscribe(REQUEST_CHUNK_SIZE).await?;

    let mut service = AtlasService::new();

    while let Some(frame) = reader.next().await {
        match frame {
            Ok(frame) => {
                debug!(len = frame.payload.len(), "atlas: received request frame");
                if frame.payload.is_empty() {
                    continue;
                }
                if let Err(err) = service.handle_payload(&frame.payload).await {
                    warn!(?err, "atlas: failed to handle request");
                }
            }
            Err(err) => {
                warn!(?err, "atlas: request stream failed");
                break;
            }
        }
    }

    Ok(())
}
