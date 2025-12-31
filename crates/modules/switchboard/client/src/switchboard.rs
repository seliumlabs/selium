//! Guest-side switchboard client for intent-based wiring.

use core::{
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use futures::{Future, SinkExt, Stream, StreamExt};
pub use selium_switchboard_protocol::{Cardinality, EndpointId};
use selium_switchboard_protocol::{
    Direction, EndpointDirections, Message, ProtocolError, WiringEgress, WiringIngress,
    decode_message, encode_message,
};
use thiserror::Error;
use tracing::{debug, warn};

use selium_userland::{
    encoding::{FlatMsg, HasSchema},
    io::{Channel, ChannelHandle, DriverError, Reader, SharedChannel, Writer},
};

type PendingWiring<In, Out> =
    Pin<Box<dyn Future<Output = Result<EndpointWiring<In, Out>, SwitchboardError>>>>;

const CONTROL_CHUNK_SIZE: u32 = 64 * 1024;
const UPDATE_CHANNEL_CAPACITY: u32 = 64 * 1024;
const INTERNAL_CHANNEL_CAPACITY: u32 = 16 * 1024;
const DATA_CHUNK_SIZE: u32 = 64 * 1024;

/// Errors produced by the switchboard client.
#[derive(Debug, Error)]
pub enum SwitchboardError {
    /// Driver returned an error while creating or destroying channels.
    #[error("driver error: {0}")]
    Driver(#[from] DriverError),
    /// Control-plane protocol could not be decoded.
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    /// The switchboard service returned an error.
    #[error("switchboard error: {0}")]
    Remote(String),
    /// The switchboard update stream was closed.
    #[error("endpoint closed")]
    EndpointClosed,
    /// No route is available yet for this endpoint.
    #[error("no route available")]
    NoRoute,
    /// Internal switchboard state was unavailable.
    #[error("switchboard state unavailable")]
    StateUnavailable,
}

/// Switchboard front-end that guests use to register endpoints.
#[derive(Clone)]
pub struct Switchboard {
    request_channel: Channel,
    _updates_channel: Channel,
    updates_shared: SharedChannel,
    state: Arc<Mutex<ClientState>>,
    next_request_id: Arc<AtomicU64>,
}

struct ClientState {
    pending: HashMap<u64, Channel>,
    endpoints: HashMap<EndpointId, Channel>,
    queued_updates: HashMap<EndpointId, Vec<Vec<u8>>>,
}

/// Builder for a new endpoint.
pub struct EndpointBuilder<In, Out> {
    switchboard: Switchboard,
    input: Cardinality,
    output: Cardinality,
    _in: PhantomData<In>,
    _out: PhantomData<Out>,
}

/// Registered endpoint that implements `Stream` for inbound frames and `Sink` for outbound frames.
pub struct EndpointHandle<In, Out> {
    id: EndpointId,
    updates: Reader,
    pending: Option<PendingWiring<In, Out>>,
    /// Inbound/outbound channel handles for this endpoint.
    pub io: EndpointIo<In, Out>,
}

/// Data-plane handles for an endpoint.
pub struct EndpointIo<In, Out> {
    /// Inbound links connected to this endpoint.
    pub inbound: Vec<InboundLink<In>>,
    /// Outbound publishers for this endpoint.
    pub outbound: Vec<RawPublisher<Out>>,
    outbound_map: HashMap<EndpointId, ChannelHandle>,
}

/// Single inbound link description.
pub struct InboundLink<In> {
    /// Producer endpoint identifier.
    pub from: EndpointId,
    /// Subscriber receiving the inbound data.
    pub subscriber: RawSubscriber<In>,
}

struct EndpointWiring<In, Out> {
    inbound: Vec<InboundLink<In>>,
    outbound: Vec<RawPublisher<Out>>,
    outbound_map: HashMap<EndpointId, ChannelHandle>,
}

/// Publisher that encodes typed payloads onto a channel.
pub struct RawPublisher<T> {
    writer: Writer,
    _marker: PhantomData<T>,
}

/// Subscriber that decodes typed payloads from a channel.
pub struct RawSubscriber<T> {
    reader: Reader,
    _marker: PhantomData<T>,
}

impl Switchboard {
    /// Connect to the switchboard service using the shared request channel handle.
    pub async fn attach(request_channel: SharedChannel) -> Result<Self, SwitchboardError> {
        let request_channel = Channel::attach_shared(request_channel).await?;
        let updates_channel = Channel::create(UPDATE_CHANNEL_CAPACITY).await?;
        let updates_shared = updates_channel.share().await?;
        let updates_reader = updates_channel.subscribe(CONTROL_CHUNK_SIZE).await?;

        let state = Arc::new(Mutex::new(ClientState {
            pending: HashMap::new(),
            endpoints: HashMap::new(),
            queued_updates: HashMap::new(),
        }));

        let dispatcher_state = Arc::clone(&state);
        spawn_dispatcher(updates_reader, dispatcher_state);
        debug!(
            updates_channel = updates_shared.raw(),
            "switchboard: dispatcher spawned"
        );

        Ok(Self {
            request_channel,
            _updates_channel: updates_channel,
            updates_shared,
            state,
            next_request_id: Arc::new(AtomicU64::new(1)),
        })
    }

    /// Begin building a new endpoint carrying `In` inbound frames and `Out` outbound frames.
    pub fn endpoint<In, Out>(&self) -> EndpointBuilder<In, Out>
    where
        In: FlatMsg + HasSchema + Send + Unpin + 'static,
        Out: FlatMsg + HasSchema + Send + Unpin + 'static,
    {
        EndpointBuilder {
            switchboard: self.clone(),
            input: Cardinality::One,
            output: Cardinality::One,
            _in: PhantomData,
            _out: PhantomData,
        }
    }

    /// Connect two endpoints by their identifiers, triggering a reconcile.
    pub async fn connect_ids(
        &self,
        from: EndpointId,
        to: EndpointId,
    ) -> Result<(), SwitchboardError> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let response = self
            .send_request(Message::ConnectRequest {
                request_id,
                from,
                to,
                reply_channel: self.updates_shared.raw(),
            })
            .await?;

        match response {
            Message::ResponseOk { .. } => Ok(()),
            Message::ResponseError { message, .. } => Err(SwitchboardError::Remote(message)),
            _ => Err(SwitchboardError::Protocol(ProtocolError::UnknownPayload)),
        }
    }

    /// Connect two endpoints by handle, triggering a reconcile.
    pub async fn connect<In, Out, Common>(
        &self,
        from: &EndpointHandle<In, Common>,
        to: &EndpointHandle<Common, Out>,
    ) -> Result<(), SwitchboardError>
    where
        In: FlatMsg + Send + Unpin + 'static,
        Out: FlatMsg + Send + Unpin + 'static,
        Common: FlatMsg + Send + Unpin + 'static,
    {
        self.connect_ids(from.id, to.id).await
    }

    async fn register_endpoint(
        &self,
        directions: EndpointDirections,
    ) -> Result<EndpointId, SwitchboardError> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        debug!(request_id, "switchboard: registering endpoint");
        let response = self
            .send_request(Message::RegisterRequest {
                request_id,
                directions,
                updates_channel: self.updates_shared.raw(),
            })
            .await?;

        match response {
            Message::ResponseRegister { endpoint_id, .. } => Ok(endpoint_id),
            Message::ResponseError { message, .. } => Err(SwitchboardError::Remote(message)),
            _ => Err(SwitchboardError::Protocol(ProtocolError::UnknownPayload)),
        }
    }

    async fn send_request(&self, message: Message) -> Result<Message, SwitchboardError> {
        let request_id = request_id_for(&message);
        debug!(request_id, "switchboard: sending request");
        let response_channel = Channel::create(INTERNAL_CHANNEL_CAPACITY).await?;
        let mut response_reader = response_channel.subscribe(CONTROL_CHUNK_SIZE).await?;

        {
            let mut guard = self.state()?;
            guard.pending.insert(request_id, response_channel.clone());
        }

        let mut writer = self.request_channel.publish_weak().await?;
        debug!(request_id, "switchboard: request channel opened");
        let bytes = encode_message(&message)?;
        writer.send(bytes).await?;
        debug!(request_id, "switchboard: request sent");

        let response = match response_reader.next().await {
            Some(Ok(frame)) => decode_message(&frame.payload)?,
            Some(Err(err)) => return Err(SwitchboardError::Driver(err)),
            None => return Err(SwitchboardError::EndpointClosed),
        };
        debug!(request_id, "switchboard: received response");

        {
            let mut guard = self.state()?;
            guard.pending.remove(&request_id);
        }

        response_channel.delete().await?;

        Ok(response)
    }

    async fn register_endpoint_channel(
        &self,
        endpoint_id: EndpointId,
        channel: Channel,
    ) -> Result<(), SwitchboardError> {
        let queued = {
            let mut guard = self.state()?;
            guard.endpoints.insert(endpoint_id, channel.clone());
            guard.queued_updates.remove(&endpoint_id)
        };

        if let Some(pending) = queued {
            for payload in pending {
                forward_bytes(channel.clone(), payload).await?;
            }
        }

        Ok(())
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, ClientState>, SwitchboardError> {
        self.state
            .lock()
            .map_err(|_| SwitchboardError::StateUnavailable)
    }
}

impl<In, Out> EndpointBuilder<In, Out>
where
    In: FlatMsg + HasSchema + Send + Unpin + 'static,
    Out: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    /// Set the inbound cardinality constraint. Defaults to `Cardinality::One`.
    pub fn inputs(mut self, cardinality: Cardinality) -> Self {
        self.input = cardinality;
        self
    }

    /// Set the outbound cardinality constraint. Defaults to `Cardinality::One`.
    pub fn outputs(mut self, cardinality: Cardinality) -> Self {
        self.output = cardinality;
        self
    }

    /// Register the endpoint with the switchboard.
    pub async fn register(self) -> Result<EndpointHandle<In, Out>, SwitchboardError> {
        let directions = EndpointDirections::new(
            Direction::new(In::SCHEMA.hash, self.input),
            Direction::new(Out::SCHEMA.hash, self.output),
        );

        let internal_channel = Channel::create(INTERNAL_CHANNEL_CAPACITY).await?;
        let updates = internal_channel.subscribe(CONTROL_CHUNK_SIZE).await?;

        let endpoint_id = self.switchboard.register_endpoint(directions).await?;
        self.switchboard
            .register_endpoint_channel(endpoint_id, internal_channel)
            .await?;

        Ok(EndpointHandle {
            id: endpoint_id,
            updates,
            pending: None,
            io: EndpointIo::new(),
        })
    }
}

impl<In, Out> EndpointHandle<In, Out> {
    /// Return the endpoint identifier assigned by the switchboard.
    pub fn id(&self) -> EndpointId {
        self.id
    }

    /// Backwards-compatible alias for the endpoint identifier.
    pub fn get_id(&self) -> EndpointId {
        self.id
    }

    /// Lookup the outbound channel handle targeting a specific endpoint.
    pub fn outbound_handle(&self, target: EndpointId) -> Option<ChannelHandle> {
        self.io.outbound_handle(target)
    }
}

impl<In, Out> EndpointHandle<In, Out>
where
    In: FlatMsg + Send + Unpin + 'static,
    Out: FlatMsg + Send + Unpin + 'static,
{
    pub(crate) fn poll_updates(&mut self, cx: &mut Context<'_>) -> Result<(), SwitchboardError> {
        loop {
            if let Some(pending) = self.pending.as_mut() {
                match pending.as_mut().poll(cx) {
                    Poll::Ready(Ok(wiring)) => {
                        self.pending = None;
                        self.io.apply_wiring(wiring);
                    }
                    Poll::Ready(Err(err)) => {
                        self.pending = None;
                        return Err(err);
                    }
                    Poll::Pending => {}
                }
            }

            match Pin::new(&mut self.updates).poll_next(cx) {
                Poll::Ready(Some(Ok(frame))) => {
                    if frame.payload.is_empty() {
                        continue;
                    }
                    let message = decode_message(&frame.payload)?;
                    if let Message::WiringUpdate {
                        inbound, outbound, ..
                    } = message
                    {
                        self.pending = Some(Box::pin(build_wiring(inbound, outbound)));
                    }
                }
                Poll::Ready(Some(Err(err))) => return Err(SwitchboardError::Driver(err)),
                Poll::Ready(None) => return Err(SwitchboardError::EndpointClosed),
                Poll::Pending => break,
            }
        }

        Ok(())
    }
}

impl<In, Out> EndpointIo<In, Out> {
    /// Lookup the outbound channel handle targeting a specific endpoint.
    pub fn outbound_handle(&self, target: EndpointId) -> Option<ChannelHandle> {
        self.outbound_map.get(&target).cloned()
    }
}

impl<In, Out> EndpointIo<In, Out>
where
    In: FlatMsg + Send + Unpin + 'static,
    Out: FlatMsg + Send + Unpin + 'static,
{
    fn new() -> Self {
        Self {
            inbound: Vec::new(),
            outbound: Vec::new(),
            outbound_map: HashMap::new(),
        }
    }

    fn apply_wiring(&mut self, wiring: EndpointWiring<In, Out>) {
        self.inbound = wiring.inbound;
        self.outbound = wiring.outbound;
        self.outbound_map = wiring.outbound_map;
    }
}

impl<T> RawPublisher<T> {
    fn new(writer: Writer) -> Self {
        Self {
            writer,
            _marker: PhantomData,
        }
    }

    pub(crate) async fn from_channel_handle(
        handle: ChannelHandle,
    ) -> Result<Self, SwitchboardError> {
        let channel = unsafe { Channel::from_raw(handle) };
        let writer = channel.publish_weak().await?;
        Ok(Self::new(writer))
    }
}

impl<T> RawSubscriber<T> {
    fn new(reader: Reader) -> Self {
        Self {
            reader,
            _marker: PhantomData,
        }
    }
}

impl<T> futures::Sink<T> for RawPublisher<T>
where
    T: FlatMsg + Send + Unpin + 'static,
{
    type Error = SwitchboardError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match Pin::new(&mut self.get_mut().writer).poll_ready(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(SwitchboardError::Driver(err))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        let encoded = FlatMsg::encode(&item);
        Pin::new(&mut self.get_mut().writer)
            .start_send(encoded)
            .map_err(SwitchboardError::Driver)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match Pin::new(&mut self.get_mut().writer).poll_flush(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(SwitchboardError::Driver(err))),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match Pin::new(&mut self.get_mut().writer).poll_close(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(SwitchboardError::Driver(err))),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> futures::Stream for RawSubscriber<T>
where
    T: FlatMsg + Send + Unpin + 'static,
{
    type Item = Result<T, SwitchboardError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.get_mut().reader).poll_next(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                let decoded =
                    T::decode(&frame.payload).map_err(|err| SwitchboardError::Protocol(err.into()));
                Poll::Ready(Some(decoded))
            }
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(SwitchboardError::Driver(err)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

async fn build_wiring<In, Out>(
    inbound: Vec<WiringIngress>,
    outbound: Vec<WiringEgress>,
) -> Result<EndpointWiring<In, Out>, SwitchboardError>
where
    In: FlatMsg + Send + Unpin + 'static,
    Out: FlatMsg + Send + Unpin + 'static,
{
    let mut inbound_links = Vec::with_capacity(inbound.len());
    for ingress in inbound {
        let shared = unsafe { SharedChannel::from_raw(ingress.channel) };
        let channel = Channel::attach_shared(shared).await?;
        let reader = channel.subscribe(DATA_CHUNK_SIZE).await?;
        inbound_links.push(InboundLink {
            from: ingress.from,
            subscriber: RawSubscriber::new(reader),
        });
    }

    let mut outbound_links = Vec::with_capacity(outbound.len());
    let mut outbound_map = HashMap::with_capacity(outbound.len());
    for egress in outbound {
        let shared = unsafe { SharedChannel::from_raw(egress.channel) };
        let channel = Channel::attach_shared(shared).await?;
        let writer = channel.publish_weak().await?;
        outbound_map.insert(egress.to, channel.handle());
        outbound_links.push(RawPublisher::new(writer));
    }

    Ok(EndpointWiring {
        inbound: inbound_links,
        outbound: outbound_links,
        outbound_map,
    })
}

fn request_id_for(message: &Message) -> u64 {
    match message {
        Message::RegisterRequest { request_id, .. }
        | Message::ConnectRequest { request_id, .. }
        | Message::ResponseRegister { request_id, .. }
        | Message::ResponseOk { request_id, .. }
        | Message::ResponseError { request_id, .. } => *request_id,
        Message::WiringUpdate { .. } => 0,
    }
}

fn spawn_dispatcher(reader: Reader, state: Arc<Mutex<ClientState>>) {
    #[cfg(test)]
    {
        tokio::spawn(async move {
            drive_updates(reader, state).await;
        });
    }
    #[cfg(not(test))]
    {
        selium_userland::spawn(async move {
            drive_updates(reader, state).await;
        });
    }
}

async fn drive_updates(mut reader: Reader, state: Arc<Mutex<ClientState>>) {
    while let Some(frame) = reader.next().await {
        let payload = match frame {
            Ok(frame) if frame.payload.is_empty() => continue,
            Ok(frame) => frame.payload,
            Err(err) => {
                warn!(?err, "switchboard: update stream failed");
                break;
            }
        };

        let message = match decode_message(&payload) {
            Ok(message) => message,
            Err(err) => {
                warn!(?err, "switchboard: failed to decode update");
                continue;
            }
        };

        match message {
            Message::ResponseOk { request_id }
            | Message::ResponseRegister { request_id, .. }
            | Message::ResponseError { request_id, .. } => {
                debug!(request_id, "switchboard: dispatching response");
                let channel = match state.lock() {
                    Ok(mut guard) => guard.pending.remove(&request_id),
                    Err(_) => None,
                };
                if let Some(channel) = channel
                    && let Err(err) = forward_bytes(channel, payload).await
                {
                    warn!(?err, "switchboard: failed to deliver response");
                }
            }
            Message::WiringUpdate { endpoint_id, .. } => {
                debug!(endpoint_id, "switchboard: dispatching wiring update");
                let (channel, queued) = match state.lock() {
                    Ok(mut guard) => {
                        if let Some(channel) = guard.endpoints.get(&endpoint_id).cloned() {
                            (Some(channel), None)
                        } else {
                            let entry = guard.queued_updates.entry(endpoint_id).or_default();
                            entry.push(payload.clone());
                            (None, Some(()))
                        }
                    }
                    Err(_) => (None, Some(())),
                };
                if let Some(channel) = channel
                    && let Err(err) = forward_bytes(channel, payload).await
                {
                    warn!(?err, "switchboard: failed to deliver update");
                }

                if queued.is_some() {
                    continue;
                }
            }
            _ => {}
        }
    }
}

async fn forward_bytes(channel: Channel, payload: Vec<u8>) -> Result<(), DriverError> {
    let mut writer = channel.publish_weak().await?;
    writer.send(payload).await?;
    Ok(())
}

#[cfg(test)]
impl Switchboard {
    /// Create a local switchboard instance for tests.
    pub async fn new() -> Result<Self, SwitchboardError> {
        let request_channel = Channel::create(UPDATE_CHANNEL_CAPACITY).await?;
        let request_shared = request_channel.share().await?;
        spawn_local_service(request_shared);
        Switchboard::attach(request_shared).await
    }
}

#[cfg(test)]
fn spawn_local_service(request_channel: SharedChannel) {
    tokio::spawn(async move {
        if let Err(err) = run_local_service(request_channel).await {
            warn!(?err, "switchboard: local service terminated");
        }
    });
}

#[cfg(test)]
async fn run_local_service(request_channel: SharedChannel) -> Result<(), SwitchboardError> {
    use selium_switchboard_core::{
        ChannelKey, SwitchboardCore, SwitchboardError as CoreError, best_compatible_match,
    };

    let request_channel = Channel::attach_shared(request_channel).await?;
    let mut reader = request_channel.subscribe(CONTROL_CHUNK_SIZE).await?;

    struct LocalChannelEntry {
        channel: Channel,
        shared: SharedChannel,
        key: ChannelKey,
    }

    #[derive(Default)]
    struct EndpointUpdate {
        inbound: Vec<WiringIngress>,
        outbound: Vec<WiringEgress>,
    }

    struct LocalService {
        core: SwitchboardCore,
        endpoints: HashMap<EndpointId, Writer>,
        channels: Vec<LocalChannelEntry>,
    }

    impl LocalService {
        fn new() -> Self {
            Self {
                core: SwitchboardCore::default(),
                endpoints: HashMap::new(),
                channels: Vec::new(),
            }
        }

        async fn handle_message(&mut self, message: Message) -> Result<(), SwitchboardError> {
            match message {
                Message::RegisterRequest {
                    request_id,
                    directions,
                    updates_channel,
                } => {
                    self.handle_register(request_id, directions, updates_channel)
                        .await
                }
                Message::ConnectRequest {
                    request_id,
                    from,
                    to,
                    reply_channel,
                } => {
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
        ) -> Result<(), SwitchboardError> {
            let updates_channel = unsafe { SharedChannel::from_raw(updates_channel) };
            let updates_writer = Channel::attach_shared(updates_channel)
                .await?
                .publish_weak()
                .await?;

            let endpoint_id = self.core.add_endpoint(directions);
            self.endpoints.insert(endpoint_id, updates_writer);

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
            self.send_response(updates_channel, response).await?;
            Ok(())
        }

        async fn handle_connect(
            &mut self,
            request_id: u64,
            from: EndpointId,
            to: EndpointId,
            reply_channel: u64,
        ) -> Result<(), SwitchboardError> {
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
        ) -> Result<(), SwitchboardError> {
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
            err: SwitchboardError,
        ) -> Result<(), SwitchboardError> {
            let response = Message::ResponseError {
                request_id,
                message: err.to_string(),
            };
            self.send_response(channel, response).await
        }

        async fn reconcile(&mut self) -> Result<(), SwitchboardError> {
            let solution = self.core.solve().map_err(SwitchboardError::from)?;
            let wiring = self.apply_solution(solution).await?;
            self.send_updates(wiring).await?;
            Ok(())
        }

        async fn apply_solution(
            &mut self,
            solution: selium_switchboard_core::Solution,
        ) -> Result<HashMap<EndpointId, EndpointUpdate>, SwitchboardError> {
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
                    let entry = create_local_channel(desired_key.clone()).await?;
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

            let mut wiring: HashMap<EndpointId, EndpointUpdate> = HashMap::new();
            for route in &solution.routes {
                for flow in &route.flows {
                    let handle = resolved
                        .get(flow.channel)
                        .ok_or(SwitchboardError::from(CoreError::Unsolveable))?;
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
            mut wiring: HashMap<EndpointId, EndpointUpdate>,
        ) -> Result<(), SwitchboardError> {
            for (endpoint_id, writer) in &mut self.endpoints {
                let update = wiring.remove(endpoint_id).unwrap_or_default();
                let message = Message::WiringUpdate {
                    endpoint_id: *endpoint_id,
                    inbound: update.inbound,
                    outbound: update.outbound,
                };
                let bytes = encode_message(&message)?;
                if let Err(err) = writer.send(bytes).await {
                    warn!(?err, endpoint_id, "switchboard: failed to send update");
                }
            }
            Ok(())
        }
    }

    async fn create_local_channel(key: ChannelKey) -> Result<LocalChannelEntry, SwitchboardError> {
        let channel = Channel::create(UPDATE_CHANNEL_CAPACITY).await?;
        let shared = channel.share().await?;
        Ok(LocalChannelEntry {
            channel,
            shared,
            key,
        })
    }

    let mut service = LocalService::new();

    while let Some(frame) = reader.next().await {
        match frame {
            Ok(frame) => {
                if frame.payload.is_empty() {
                    continue;
                }
                let message = decode_message(&frame.payload)?;
                service.handle_message(message).await?;
            }
            Err(err) => return Err(SwitchboardError::Driver(err)),
        }
    }

    Ok(())
}

#[cfg(test)]
impl From<selium_switchboard_core::SwitchboardError> for SwitchboardError {
    fn from(value: selium_switchboard_core::SwitchboardError) -> Self {
        SwitchboardError::Remote(value.to_string())
    }
}
