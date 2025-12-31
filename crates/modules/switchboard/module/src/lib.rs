//! Switchboard service module that manages endpoint wiring.

use std::collections::HashMap;

use anyhow::Result;
use futures::{SinkExt, StreamExt};
use selium_switchboard_core::{
    ChannelKey, SwitchboardCore, SwitchboardError as CoreError, best_compatible_match,
};
use selium_switchboard_protocol::{
    EndpointDirections, EndpointId, Message, ProtocolError, WiringEgress, WiringIngress,
    decode_message, encode_message,
};
use selium_userland::{
    entrypoint,
    io::{Channel, DriverError, SharedChannel, Writer},
};
use thiserror::Error;
use tracing::{debug, info, instrument, warn};

const REQUEST_CHUNK_SIZE: u32 = 64 * 1024;
const CHANNEL_CAPACITY: u32 = 64 * 1024;

#[derive(Debug, Error)]
enum SwitchboardServiceError {
    #[error("driver error: {0}")]
    Driver(#[from] DriverError),
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("solver error: {0}")]
    Solver(#[from] CoreError),
}

struct ChannelEntry {
    channel: Channel,
    shared: SharedChannel,
    key: ChannelKey,
}

struct EndpointRegistration {
    updates: Writer,
}

#[derive(Default)]
struct EndpointWiring {
    inbound: Vec<WiringIngress>,
    outbound: Vec<WiringEgress>,
}

struct SwitchboardService {
    core: SwitchboardCore,
    endpoints: HashMap<EndpointId, EndpointRegistration>,
    channels: Vec<ChannelEntry>,
}

/// Entry point for the switchboard module.
#[entrypoint]
#[instrument(name = "switchboard.start")]
pub async fn start() -> Result<()> {
    let request_channel = Channel::create(CHANNEL_CAPACITY).await?;
    let shared = request_channel.share().await?;
    info!(
        request_channel = shared.raw(),
        "switchboard: created request channel"
    );
    // TODO(@maintainer): Register the shared handle for discovery once the registry path exists.
    let mut reader = request_channel.subscribe(REQUEST_CHUNK_SIZE).await?;

    let mut service = SwitchboardService::new();

    while let Some(frame) = reader.next().await {
        match frame {
            Ok(frame) => {
                debug!(
                    len = frame.payload.len(),
                    "switchboard: received request frame"
                );
                if frame.payload.is_empty() {
                    continue;
                }
                if let Err(err) = service.handle_payload(&frame.payload).await {
                    warn!(?err, "switchboard: failed to handle request");
                }
            }
            Err(err) => {
                warn!(?err, "switchboard: request stream failed");
                break;
            }
        }
    }

    Ok(())
}

impl SwitchboardService {
    fn new() -> Self {
        Self {
            core: SwitchboardCore::default(),
            endpoints: HashMap::new(),
            channels: Vec::new(),
        }
    }

    async fn handle_payload(&mut self, payload: &[u8]) -> Result<(), SwitchboardServiceError> {
        let message = decode_message(payload)?;
        self.handle_message(message).await
    }

    async fn handle_message(&mut self, message: Message) -> Result<(), SwitchboardServiceError> {
        match message {
            Message::RegisterRequest {
                request_id,
                directions,
                updates_channel,
            } => {
                debug!(
                    request_id,
                    updates_channel, "switchboard: register request received"
                );
                self.handle_register(request_id, directions, updates_channel)
                    .await
            }
            Message::ConnectRequest {
                request_id,
                from,
                to,
                reply_channel,
            } => {
                debug!(
                    request_id,
                    from, to, reply_channel, "switchboard: connect request received"
                );
                self.handle_connect(request_id, from, to, reply_channel)
                    .await
            }
            _ => Ok(()),
        }
    }

    async fn handle_register(
        &mut self,
        request_id: u64,
        directions: EndpointDirections,
        updates_channel: u64,
    ) -> Result<(), SwitchboardServiceError> {
        let updates_channel = unsafe { SharedChannel::from_raw(updates_channel) };
        let updates_writer = Channel::attach_shared(updates_channel)
            .await?
            .publish_weak()
            .await?;

        let endpoint_id = self.core.add_endpoint(directions);
        debug!(request_id, endpoint_id, "switchboard: registered endpoint");
        self.endpoints.insert(
            endpoint_id,
            EndpointRegistration {
                updates: updates_writer,
            },
        );

        if let Err(err) = self.reconcile().await {
            self.core.remove_endpoint(endpoint_id);
            self.endpoints.remove(&endpoint_id);
            self.send_error(updates_channel, request_id, err).await?;
            return Ok(());
        }

        let response = Message::ResponseRegister {
            request_id,
            endpoint_id,
        };
        let bytes = encode_message(&response)?;
        if let Some(registration) = self.endpoints.get_mut(&endpoint_id) {
            registration.updates.send(bytes).await?;
        }

        Ok(())
    }

    async fn handle_connect(
        &mut self,
        request_id: u64,
        from: EndpointId,
        to: EndpointId,
        reply_channel: u64,
    ) -> Result<(), SwitchboardServiceError> {
        let reply_channel = unsafe { SharedChannel::from_raw(reply_channel) };

        if let Err(err) = self.core.add_intent(from, to) {
            self.send_error(reply_channel, request_id, err.into())
                .await?;
            return Ok(());
        }

        if let Err(err) = self.reconcile().await {
            self.core.remove_intent(from, to);
            self.send_error(reply_channel, request_id, err).await?;
            return Ok(());
        }

        let response = Message::ResponseOk { request_id };
        self.send_response(reply_channel, response).await?;
        Ok(())
    }

    async fn send_response(
        &self,
        channel: SharedChannel,
        message: Message,
    ) -> Result<(), SwitchboardServiceError> {
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
        err: SwitchboardServiceError,
    ) -> Result<(), SwitchboardServiceError> {
        let response = Message::ResponseError {
            request_id,
            message: err.to_string(),
        };
        self.send_response(channel, response).await
    }

    async fn reconcile(&mut self) -> Result<(), SwitchboardServiceError> {
        let solution = self.core.solve()?;
        let wiring = self.apply_solution(solution).await?;
        self.send_updates(wiring).await?;
        Ok(())
    }

    async fn apply_solution(
        &mut self,
        solution: selium_switchboard_core::Solution,
    ) -> Result<HashMap<EndpointId, EndpointWiring>, SwitchboardServiceError> {
        let mut available = std::mem::take(&mut self.channels);
        let mut retained = Vec::with_capacity(solution.channels.len());
        let mut resolved: Vec<SharedChannel> = Vec::with_capacity(solution.channels.len());

        for spec in &solution.channels {
            let desired_key = spec.key().clone();
            let position = available
                .iter()
                .position(|entry| entry.key == desired_key)
                .or_else(|| {
                    let keys: Vec<ChannelKey> =
                        available.iter().map(|entry| entry.key.clone()).collect();
                    best_compatible_match(&keys, &desired_key)
                });

            if let Some(pos) = position {
                let mut entry = available.swap_remove(pos);
                if entry.key != desired_key {
                    entry.key = desired_key.clone();
                }
                resolved.push(entry.shared);
                retained.push(entry);
            } else {
                let entry = Self::create_channel(desired_key.clone()).await?;
                resolved.push(entry.shared);
                retained.push(entry);
            }
        }

        for entry in available {
            if let Err(err) = entry.channel.drain().await {
                warn!(?err, "switchboard: failed to drain channel");
            }
            if let Err(err) = entry.channel.delete().await {
                warn!(?err, "switchboard: failed to delete channel");
            }
        }

        self.channels = retained;

        let mut wiring: HashMap<EndpointId, EndpointWiring> = HashMap::new();
        for route in &solution.routes {
            for flow in &route.flows {
                let handle = resolved.get(flow.channel).ok_or(CoreError::Unsolveable)?;
                wiring
                    .entry(flow.producer)
                    .or_default()
                    .outbound
                    .push(WiringEgress {
                        to: flow.consumer,
                        channel: handle.raw(),
                    });
                wiring
                    .entry(flow.consumer)
                    .or_default()
                    .inbound
                    .push(WiringIngress {
                        from: flow.producer,
                        channel: handle.raw(),
                    });
            }
        }

        for entry in wiring.values_mut() {
            entry
                .inbound
                .sort_unstable_by_key(|ingress| (ingress.channel, ingress.from));
            entry.inbound.dedup_by_key(|ingress| ingress.channel);
            entry
                .outbound
                .sort_unstable_by_key(|egress| (egress.channel, egress.to));
            entry.outbound.dedup_by_key(|egress| egress.channel);
        }

        Ok(wiring)
    }

    async fn send_updates(
        &mut self,
        mut wiring: HashMap<EndpointId, EndpointWiring>,
    ) -> Result<(), SwitchboardServiceError> {
        for (endpoint_id, registration) in &mut self.endpoints {
            let update = wiring.remove(endpoint_id).unwrap_or_default();
            let message = Message::WiringUpdate {
                endpoint_id: *endpoint_id,
                inbound: update.inbound,
                outbound: update.outbound,
            };
            let bytes = encode_message(&message)?;
            if let Err(err) = registration.updates.send(bytes).await {
                warn!(?err, endpoint_id, "switchboard: failed to send update");
            }
        }
        Ok(())
    }

    async fn create_channel(key: ChannelKey) -> Result<ChannelEntry, SwitchboardServiceError> {
        let channel = Channel::create(CHANNEL_CAPACITY).await?;
        let shared = channel.share().await?;
        Ok(ChannelEntry {
            channel,
            shared,
            key,
        })
    }
}
