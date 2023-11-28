use std::{sync::Arc, marker::PhantomData, pin::Pin};
use bytes::{Bytes, BytesMut};
use selium_protocol::{BiStream, Frame, ReplierPayload, MessagePayload};
use selium_std::errors::{Result, CodecError};
use async_trait::async_trait;
use futures::{Future, SinkExt, StreamExt};
use selium_std::traits::{codec::{MessageDecoder, MessageEncoder}, compression::{Decompress, Compress}};
use tokio::sync::MutexGuard;
use super::states::*;
use crate::{ StreamBuilder, streams::aliases::{Decomp, Comp}, Client, connection::ClientConnection, traits::Open, };

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
        Fut: Future<Output = Result<ResItem>>,
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
    Fut: Future<Output = Result<ResItem>>,
    ReqItem: Unpin + Send,
    ResItem: Unpin + Send
{
    type Output = Replier<E, D, F, ReqItem, ResItem>;

    async fn open(self) -> Result<Self::Output> {
        let headers = ReplierPayload { topic: self.state.endpoint };

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
    handler: Pin<Box<F>>,
    _req_marker: PhantomData<ReqItem>,
    _res_marker: PhantomData<ResItem>
}

impl<D, E, F, Fut, ReqItem, ResItem> Replier<E, D, F, ReqItem, ResItem>
where
    D: MessageDecoder<ReqItem> + Send + Unpin,
    E: MessageEncoder<ResItem> + Send + Unpin,
    F: FnMut(ReqItem) -> Fut + Send + Unpin,
    Fut: Future<Output = Result<ResItem>>,
    ReqItem: Unpin + Send,
    ResItem: Unpin + Send
{
    async fn spawn(
        client: Client,
        headers: ReplierPayload,
        encoder: E,
        decoder: D,
        compression: Option<Comp>,
        decompression: Option<Decomp>,
        handler: Pin<Box<F>>,
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

    fn decode_message(&mut self, mut bytes: Bytes) -> Result<ReqItem> {
        if let Some(decomp) = self.decompression.as_ref() {
            bytes = decomp
                .decompress(bytes)
                .map_err(CodecError::DecompressFailure)?;
        }

        let mut mut_bytes = BytesMut::with_capacity(bytes.len());
        mut_bytes.extend_from_slice(&bytes);

        Ok(self
            .decoder
            .decode(&mut mut_bytes)
            .map_err(CodecError::DecodeFailure)?)
    }

    fn encode_message(&mut self, item: ResItem) -> Result<Bytes> {
        let mut encoded = self.encoder.encode(item).map_err(CodecError::EncodeFailure)?;

        if let Some(comp) = self.compression.as_ref() {
            encoded = comp
                .compress(encoded)
                .map_err(CodecError::CompressFailure)?;
        }

        Ok(encoded)
    }

    pub async fn listen(mut self) -> Result<()> {
        while let Some(Ok(request)) = self.stream.next().await {
            if let Frame::Message(req_payload) = request {
                let decoded = self.decode_message(req_payload.message)?;
                let response = (self.handler)(decoded).await?;
                let encoded = self.encode_message(response)?;

                let res_payload = MessagePayload {
                    headers: req_payload.headers,
                    message: encoded
                };

                let frame = Frame::Message(res_payload);
                self.stream.send(frame).await?;
            }
        }

        Ok(())
    }
}
