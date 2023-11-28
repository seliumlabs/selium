use std::{sync::{Arc, MutexGuard}, marker::PhantomData};
use selium_protocol::{BiStream, Frame};
use selium_std::errors::Result;
use async_trait::async_trait;
use futures::{Future, SinkExt};
use selium_std::traits::{codec::{MessageDecoder, MessageEncoder}, compression::{Decompress, Compress}};
use super::states::*;
use crate::{ StreamBuilder, streams::aliases::{Decomp, Comp}, Client, connection::ClientConnection, };

impl StreamBuilder<ReplierWantsRequestDecoder> {
    pub fn with_request_decoder<D, ReqItem>(self, decoder: D) -> StreamBuilder<ReplierWantsReplyEncoder<D, ReqItem>> {
        let next_state = ReplierWantsReplyEncoder::new(self.state, decoder);

        StreamBuilder {
            state: next_state,
            client: self.client,
        }
    }
}

impl<D, ReqItem> StreamBuilder<ReplierWantsReplyEncoder<D, ReqItem>> {
    pub fn with_request_decompression<T>(mut self, decomp: T) -> Self 
    where
        T: Decompress + Send + Sync + 'static
    {
        self.state.decompression = Some(Arc::new(decomp));
        self
    }

    pub fn with_reply_encoder<E, ResItem>(self, encoder: E) -> StreamBuilder<ReplierWantsHandler<D, E, ReqItem, ResItem>> {
        let next_state = ReplierWantsHandler::new(self.state, encoder);

        StreamBuilder {
            state: next_state,
            client: self.client,
        }
    }
}

impl<D, E, ReqItem, ResItem> StreamBuilder<ReplierWantsHandler<D, E, ReqItem, ResItem>> {
    pub fn with_reply_compression<T>(mut self, comp: T) -> Self 
    where
        T: Compress + Send + Sync + 'static
    {
        self.state.compression = Some(Arc::new(comp));
        self
    }

    pub fn with_handler<F, Fut>(
        self,
        handler: F,
    ) -> StreamBuilder<ReplierWantsOpen<D, E, F, ReqItem, ResItem>> 
    where
        D: MessageDecoder<ReqItem> + Send + Unpin,
        E: MessageEncoder<ResItem> + Send + Unpin,
        F: FnMut(ReqItem) -> Fut,
        Fut: Future<Output = ResItem>,
        ReqItem: Unpin + Send,
        ResItem: Unpin + Send,
    {
        let next_state = ReplierWantsOpen::new(self.state, handler);

        StreamBuilder {
            state: next_state,
            client: self.client,
        }
    }
}

#[async_trait]
impl<D, E, F, Fut, ReqItem, ResItem> Open for StreamBuilder<ReplierWantsOpen<D, E, F, ReqItem, ResItem>>
where
    D: MessageDecoder<ReqItem> + Send + Unpin,
    E: MessageEncoder<ResItem> + Send + Unpin,
    F: FnMut(ReqItem) -> Fut + Send + Unpin,
    Fut: Future<Output = ResItem>,
    ReqItem: Unpin + Send,
    ResItem: Unpin + Send
{
    type Output = Replier<E, D, F, ReqItem, ResItem>;

    async fn open(self) -> Result<Self::Output> {
        let headers =;

        let replier = Replier::spawn(
            self.client,
            headers,
            self.state.encoder,
            self.state.decoder,
            self.state.compression,
            self.state.decompression,
            self.state.handler,
        )
        .await?;

        Ok(replier)
    }
}

pub struct Replier<E, D, F, ReqItem, ResItem> {
    client: Client,
    stream: BiStream,
    headers: ReplierPayload,
    encoder: E,
    decoder: D,
    compression: Option<Comp>,
    decompression: Option<Decomp>,
    handler: F,
    _req_marker: PhantomData<ReqItem>,
    _res_marker: PhantomData<ResItem>
}

impl<E, D, F, ReqItem, ResItem> Replier<E, D, F, ReqItem, ResItem> {
    async fn spawn(
        client: Client,
        headers: ReplierPayload,
        encoder: E,
        decoder: D,
        compression: Option<Comp>,
        decompression: Option<Decomp>,
        handler: F,
    ) -> Result<Self> {
        let lock = client.connection.lock().await; 
        let stream = Self::open_stream(lock, headers.clone()).await?;

        let replier = Self {
            client,
            stream,
            headers,
            encoder,
            decoder,
            compression,
            decompression,
            handler,
            _req_marker: PhantomData,
            _res_marker: PhantomData,
        };

        Ok(replier)
    }

    async fn open_stream(
        lock: MutexGuard<'_, ClientConnection>,
        headers: ReplierPayload,
    ) -> Result<BiStream> {
        let mut stream = BiStream::try_from_connection(lock.conn()).await?;
        drop(lock);

        let frame = Frame::RegisterReplier(headers);
        stream.send(frame).await?;

        Ok(stream)
    }

    pub async fn listen(self) -> Result<()> {
       Ok(()) 
    }
}
