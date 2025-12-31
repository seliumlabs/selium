//! Messaging helpers built on the switchboard.

use std::{
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll, ready},
};

use futures::future::poll_fn;
use futures::{Sink, SinkExt, Stream, StreamExt};
use pin_project::pin_project;
use selium_userland::{
    encoding::{FlatMsg, HasSchema},
    io::ChannelHandle,
};

use crate::switchboard::{
    Cardinality, EndpointHandle, EndpointId, RawPublisher, Switchboard, SwitchboardError,
};

/// Target that a [`Client`] can connect to for a given request/response pair.
pub trait ClientTarget<Req, Rep> {
    /// Return the target endpoint identifier.
    fn endpoint_id(&self) -> EndpointId;
}

/// Target that a [`Subscriber`] can connect to for a given payload type.
pub trait SubscriberTarget<Req> {
    /// Return the target endpoint identifier.
    fn endpoint_id(&self) -> EndpointId;
}

/// Target that a [`Publisher`] or [`Fanout`] can connect to for a given payload type.
pub trait PublisherTarget<Rep> {
    /// Return the target endpoint identifier.
    fn endpoint_id(&self) -> EndpointId;
}

/// Target that a [`Server`] can connect to for a given request/response pair.
pub trait ServerTarget<Req, Rep> {
    /// Return the target endpoint identifier.
    fn endpoint_id(&self) -> EndpointId;
}

/// Disseminates data to zero or more [`Subscriber`]s.
#[pin_project(project = PublisherProject)]
pub struct Publisher<Rep> {
    endpoint: EndpointHandle<(), Rep>,
}

/// Receives data from a single [`Publisher`].
#[pin_project(project = SubscriberProject)]
pub struct Subscriber<Req> {
    endpoint: EndpointHandle<Req, ()>,
}

/// Balances messages between one or more [`Subscriber`]s.
#[pin_project(project = FanoutProject)]
pub struct Fanout<Rep> {
    endpoint: EndpointHandle<(), Rep>,
    next: usize,
}

/// Replies to queries sent by [`Client`]s.
#[pin_project(project = ServerProject)]
pub struct Server<Req, Rep> {
    endpoint: EndpointHandle<Req, Rep>,
}

/// Sends queries to [`Server`]s.
#[pin_project(project = ClientProject)]
pub struct Client<Req, Rep> {
    endpoint: EndpointHandle<Rep, Req>,
}

/// Request wrapper that includes an opaque responder bound to the origin endpoint.
pub struct RequestCtx<Req, Rep> {
    request: Req,
    responder: Responder<Rep>,
}

/// Opaque reply handle that targets the originating requester.
pub struct Responder<Rep> {
    handle: ChannelHandle,
    _marker: PhantomData<Rep>,
}

impl<Req, Rep> ClientTarget<Req, Rep> for EndpointId {
    fn endpoint_id(&self) -> EndpointId {
        *self
    }
}

impl<Req, Rep> ClientTarget<Req, Rep> for &Server<Req, Rep>
where
    Req: FlatMsg + HasSchema + Send + Unpin + 'static,
    Rep: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    fn endpoint_id(&self) -> EndpointId {
        Server::endpoint_id(*self)
    }
}

impl<Req> SubscriberTarget<Req> for EndpointId {
    fn endpoint_id(&self) -> EndpointId {
        *self
    }
}

impl<Req> SubscriberTarget<Req> for &Publisher<Req>
where
    Req: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    fn endpoint_id(&self) -> EndpointId {
        Publisher::endpoint_id(*self)
    }
}

impl<Req> SubscriberTarget<Req> for &Fanout<Req>
where
    Req: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    fn endpoint_id(&self) -> EndpointId {
        Fanout::endpoint_id(*self)
    }
}

impl<Rep> PublisherTarget<Rep> for EndpointId {
    fn endpoint_id(&self) -> EndpointId {
        *self
    }
}

impl<Rep> PublisherTarget<Rep> for &Subscriber<Rep>
where
    Rep: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    fn endpoint_id(&self) -> EndpointId {
        Subscriber::endpoint_id(*self)
    }
}

impl<Req, Rep> ServerTarget<Req, Rep> for EndpointId {
    fn endpoint_id(&self) -> EndpointId {
        *self
    }
}

impl<Req, Rep> ServerTarget<Req, Rep> for &Client<Req, Rep>
where
    Req: FlatMsg + HasSchema + Send + Unpin + 'static,
    Rep: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    fn endpoint_id(&self) -> EndpointId {
        Client::endpoint_id(*self)
    }
}

impl<Rep> Clone for Responder<Rep> {
    fn clone(&self) -> Self {
        Self {
            handle: self.handle,
            _marker: PhantomData,
        }
    }
}

impl<Rep> Publisher<Rep>
where
    Rep: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    /// Create a new publisher endpoint.
    pub async fn create(switchboard: &mut Switchboard) -> Result<Self, SwitchboardError> {
        let endpoint = switchboard
            .endpoint()
            .inputs(Cardinality::Zero)
            .outputs(Cardinality::One)
            .register()
            .await?;
        Ok(Self { endpoint })
    }

    /// Return the switchboard endpoint identifier.
    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.get_id()
    }

    /// Connect this publisher to the supplied target, wiring outbound flows.
    pub async fn connect<T>(
        &self,
        switchboard: &Switchboard,
        target: T,
    ) -> Result<(), SwitchboardError>
    where
        T: PublisherTarget<Rep>,
    {
        switchboard
            .connect_ids(self.endpoint_id(), target.endpoint_id())
            .await
    }

    /// Wait for outbound wiring to be established.
    pub async fn ready(&mut self) -> Result<(), SwitchboardError> {
        poll_fn(|cx| {
            self.endpoint.poll_updates(cx)?;
            if self.endpoint.io.outbound.is_empty() {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        })
        .await
    }
}

impl<Rep> Sink<Rep> for Publisher<Rep>
where
    Rep: FlatMsg + Send + Unpin + 'static,
{
    type Error = SwitchboardError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let PublisherProject { endpoint } = self.project();

        endpoint.poll_updates(cx)?;

        if endpoint.io.outbound.is_empty() {
            return Poll::Pending;
        }

        debug_assert_eq!(endpoint.io.outbound.len(), 1);

        let mut publ = Pin::new(&mut endpoint.io.outbound[0]);
        match publ.as_mut().poll_ready(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Rep) -> Result<(), Self::Error> {
        let PublisherProject { endpoint } = self.project();

        if endpoint.io.outbound.is_empty() {
            return Err(SwitchboardError::NoRoute);
        }

        debug_assert_eq!(endpoint.io.outbound.len(), 1);

        let mut publ = Pin::new(&mut endpoint.io.outbound[0]);
        publ.as_mut().start_send(item)?;

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let PublisherProject { endpoint } = self.project();

        endpoint.poll_updates(cx)?;

        debug_assert_eq!(endpoint.io.outbound.len(), 1);

        ready!(
            Pin::new(&mut endpoint.io.outbound[0])
                .as_mut()
                .poll_flush(cx)?
        );

        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }
}

impl<Req> Subscriber<Req>
where
    Req: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    /// Create a new subscriber endpoint.
    pub async fn create(switchboard: &mut Switchboard) -> Result<Self, SwitchboardError> {
        let endpoint = switchboard
            .endpoint()
            .inputs(Cardinality::One)
            .outputs(Cardinality::Zero)
            .register()
            .await?;
        Ok(Self { endpoint })
    }

    /// Return the switchboard endpoint identifier.
    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.get_id()
    }

    /// Connect this subscriber to the supplied target, wiring inbound flows.
    pub async fn connect<T>(
        &self,
        switchboard: &Switchboard,
        source: T,
    ) -> Result<(), SwitchboardError>
    where
        T: SubscriberTarget<Req>,
    {
        switchboard
            .connect_ids(source.endpoint_id(), self.endpoint_id())
            .await
    }

    /// Wait for inbound wiring to be established.
    pub async fn ready(&mut self) -> Result<(), SwitchboardError> {
        poll_fn(|cx| {
            self.endpoint.poll_updates(cx)?;
            if self.endpoint.io.inbound.is_empty() {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        })
        .await
    }
}

impl<Req> Stream for Subscriber<Req>
where
    Req: FlatMsg + Send + Unpin + HasSchema + 'static,
{
    type Item = Result<Req, SwitchboardError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let SubscriberProject { endpoint } = self.project();

        endpoint.poll_updates(cx)?;

        if endpoint.io.inbound.is_empty() {
            return Poll::Pending;
        }

        debug_assert_eq!(endpoint.io.inbound.len(), 1);

        let mut sub = Pin::new(&mut endpoint.io.inbound[0].subscriber);
        match sub.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(item))) => Poll::Ready(Some(Ok(item))),
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<Rep> Fanout<Rep>
where
    Rep: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    /// Create a new fanout endpoint.
    pub async fn create(switchboard: &mut Switchboard) -> Result<Self, SwitchboardError> {
        let endpoint = switchboard
            .endpoint()
            .inputs(Cardinality::Zero)
            .outputs(Cardinality::Many)
            .register()
            .await?;
        Ok(Self { endpoint, next: 0 })
    }

    /// Return the switchboard endpoint identifier.
    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.get_id()
    }

    /// Connect this fanout to the supplied target, wiring outbound flows.
    pub async fn connect<T>(
        &self,
        switchboard: &Switchboard,
        target: T,
    ) -> Result<(), SwitchboardError>
    where
        T: PublisherTarget<Rep>,
    {
        switchboard
            .connect_ids(self.endpoint_id(), target.endpoint_id())
            .await
    }

    /// Wait for outbound wiring to be established.
    pub async fn ready(&mut self) -> Result<(), SwitchboardError> {
        poll_fn(|cx| {
            self.endpoint.poll_updates(cx)?;
            if self.endpoint.io.outbound.is_empty() {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        })
        .await
    }
}

impl<Rep> Sink<Rep> for Fanout<Rep>
where
    Rep: FlatMsg + Send + Unpin + 'static,
{
    type Error = SwitchboardError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let FanoutProject { endpoint, next } = self.project();

        endpoint.poll_updates(cx)?;

        if endpoint.io.outbound.is_empty() {
            return Poll::Pending;
        }

        let idx = *next % endpoint.io.outbound.len();
        let mut publ = Pin::new(&mut endpoint.io.outbound[idx]);
        match publ.as_mut().poll_ready(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Rep) -> Result<(), Self::Error> {
        let FanoutProject { endpoint, next } = self.project();

        if endpoint.io.outbound.is_empty() {
            return Err(SwitchboardError::NoRoute);
        }

        let idx = *next % endpoint.io.outbound.len();
        let mut publ = Pin::new(&mut endpoint.io.outbound[idx]);
        publ.as_mut().start_send(item)?;
        *next = (idx + 1) % endpoint.io.outbound.len();

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        #[allow(unused_variables)]
        let FanoutProject { endpoint, next } = self.project();

        endpoint.poll_updates(cx)?;
        for publ in &mut endpoint.io.outbound {
            let mut pinned = Pin::new(publ);
            ready!(pinned.as_mut().poll_flush(cx)?);
        }

        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }
}

impl<Req, Rep> Server<Req, Rep>
where
    Req: FlatMsg + HasSchema + Send + Unpin + 'static,
    Rep: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    /// Create a new server endpoint.
    pub async fn create(switchboard: &mut Switchboard) -> Result<Self, SwitchboardError> {
        let endpoint = switchboard
            .endpoint()
            .inputs(Cardinality::One)
            .outputs(Cardinality::Many)
            .register()
            .await?;
        Ok(Self { endpoint })
    }

    /// Return the switchboard endpoint identifier.
    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.get_id()
    }

    /// Connect this server to the supplied target, wiring request and reply flows.
    pub async fn connect<T>(
        &self,
        switchboard: &Switchboard,
        client: T,
    ) -> Result<(), SwitchboardError>
    where
        T: ServerTarget<Req, Rep>,
    {
        let client_id = client.endpoint_id();
        switchboard
            .connect_ids(client_id, self.endpoint_id())
            .await?;
        switchboard.connect_ids(self.endpoint_id(), client_id).await
    }

    /// Wait for inbound and outbound wiring to be established.
    pub async fn ready(&mut self) -> Result<(), SwitchboardError> {
        poll_fn(|cx| {
            self.endpoint.poll_updates(cx)?;
            if self.endpoint.io.inbound.is_empty() || self.endpoint.io.outbound.is_empty() {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        })
        .await
    }
}

impl<Req, Rep> Stream for Server<Req, Rep>
where
    Req: FlatMsg + Send + Unpin + HasSchema + 'static,
    Rep: FlatMsg + Send + Unpin + HasSchema + 'static,
{
    type Item = Result<RequestCtx<Req, Rep>, SwitchboardError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let ServerProject { endpoint } = self.project();

        endpoint.poll_updates(cx)?;

        if endpoint.io.inbound.is_empty() {
            return Poll::Pending;
        }

        debug_assert_eq!(endpoint.io.inbound.len(), 1);

        let mut sub = Pin::new(&mut endpoint.io.inbound[0].subscriber);
        match sub.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(item))) => {
                let Some(handle) = endpoint.outbound_handle(endpoint.io.inbound[0].from) else {
                    return Poll::Ready(Some(Err(SwitchboardError::NoRoute)));
                };
                let responder = Responder::new(handle);
                Poll::Ready(Some(Ok(RequestCtx {
                    request: item,
                    responder,
                })))
            }
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<Req, Rep> Client<Req, Rep>
where
    Req: FlatMsg + HasSchema + Send + Unpin + 'static,
    Rep: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    /// Create a new client endpoint.
    pub async fn create(switchboard: &mut Switchboard) -> Result<Self, SwitchboardError> {
        let endpoint = switchboard
            .endpoint()
            .inputs(Cardinality::One)
            .outputs(Cardinality::One)
            .register()
            .await?;
        Ok(Self { endpoint })
    }

    /// Return the switchboard endpoint identifier.
    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.get_id()
    }

    /// Send a request and await the next reply.
    ///
    /// This helper assumes at most one in-flight request at a time.
    pub async fn request(&mut self, request: Req) -> Result<Rep, SwitchboardError> {
        self.send(request).await?;
        match self.next().await {
            Some(Ok(reply)) => Ok(reply),
            Some(Err(err)) => Err(err),
            None => Err(SwitchboardError::EndpointClosed),
        }
    }

    /// Connect this client to the supplied target, wiring request and reply flows.
    pub async fn connect<T>(
        &self,
        switchboard: &Switchboard,
        server: T,
    ) -> Result<(), SwitchboardError>
    where
        T: ClientTarget<Req, Rep>,
    {
        let server_id = server.endpoint_id();
        switchboard
            .connect_ids(self.endpoint_id(), server_id)
            .await?;
        switchboard.connect_ids(server_id, self.endpoint_id()).await
    }

    /// Wait for inbound and outbound wiring to be established.
    pub async fn ready(&mut self) -> Result<(), SwitchboardError> {
        poll_fn(|cx| {
            self.endpoint.poll_updates(cx)?;
            if self.endpoint.io.inbound.is_empty() || self.endpoint.io.outbound.is_empty() {
                Poll::Pending
            } else {
                Poll::Ready(Ok(()))
            }
        })
        .await
    }
}

impl<Req, Rep> Stream for Client<Req, Rep>
where
    Req: FlatMsg + Send + Unpin + HasSchema + 'static,
    Rep: FlatMsg + Send + Unpin + HasSchema + 'static,
{
    type Item = Result<Rep, SwitchboardError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let ClientProject { endpoint } = self.project();

        endpoint.poll_updates(cx)?;

        if endpoint.io.inbound.is_empty() {
            return Poll::Pending;
        }

        debug_assert_eq!(endpoint.io.inbound.len(), 1);

        let mut sub = Pin::new(&mut endpoint.io.inbound[0].subscriber);
        match sub.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(item))) => Poll::Ready(Some(Ok(item))),
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(err))),
            Poll::Ready(None) => {
                endpoint.io.inbound.remove(0);
                Poll::Pending
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<Req, Rep> Sink<Req> for Client<Req, Rep>
where
    Req: FlatMsg + Send + Unpin + 'static,
    Rep: FlatMsg + Send + Unpin + 'static,
{
    type Error = SwitchboardError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let ClientProject { endpoint } = self.project();

        endpoint.poll_updates(cx)?;

        if endpoint.io.outbound.is_empty() {
            return Poll::Pending;
        }

        debug_assert_eq!(endpoint.io.outbound.len(), 1);

        let mut publ = Pin::new(&mut endpoint.io.outbound[0]);
        match publ.as_mut().poll_ready(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Req) -> Result<(), Self::Error> {
        let ClientProject { endpoint } = self.project();

        if endpoint.io.outbound.is_empty() {
            return Err(SwitchboardError::NoRoute);
        }

        debug_assert_eq!(endpoint.io.outbound.len(), 1);

        let mut publ = Pin::new(&mut endpoint.io.outbound[0]);
        publ.as_mut().start_send(item)?;

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let ClientProject { endpoint } = self.project();

        endpoint.poll_updates(cx)?;

        debug_assert_eq!(endpoint.io.outbound.len(), 1);

        ready!(
            Pin::new(&mut endpoint.io.outbound[0])
                .as_mut()
                .poll_flush(cx)?
        );

        Poll::Ready(Ok(()))
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.poll_flush(cx)
    }
}

impl<Req, Rep> RequestCtx<Req, Rep>
where
    Rep: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    /// Decompose the request context into the request payload and responder.
    pub fn into_parts(self) -> (Req, Responder<Rep>) {
        (self.request, self.responder)
    }

    /// Return a clone of the responder handle.
    pub fn responder(&self) -> Responder<Rep> {
        self.responder.clone()
    }

    /// Borrow the request payload.
    pub fn request(&self) -> &Req {
        &self.request
    }

    /// Send a reply back to the requester that issued the corresponding `RequestCtx`.
    pub async fn reply(&self, reply: Rep) -> Result<(), SwitchboardError> {
        self.responder.send(reply).await
    }
}

impl<Rep> Responder<Rep>
where
    Rep: FlatMsg + HasSchema + Send + Unpin + 'static,
{
    fn new(handle: ChannelHandle) -> Self {
        Self {
            handle,
            _marker: PhantomData,
        }
    }

    /// Send a reply back to the requester that issued the corresponding `RequestCtx`.
    pub async fn send(&self, reply: Rep) -> Result<(), SwitchboardError> {
        let mut publisher = RawPublisher::<Rep>::from_channel_handle(self.handle).await?;
        publisher.send(reply).await
    }
}
