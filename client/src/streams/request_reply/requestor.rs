use super::states::*;
use crate::{StreamBuilder, Client};
use crate::connection::ClientConnection;
use crate::streams::aliases::{Comp, Decomp};
use crate::traits::Open;
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::{SinkExt, StreamExt};
use selium_protocol::{RequesterPayload, BiStream, Frame, MessagePayload};
use selium_std::errors::{CodecError, SeliumError};
use selium_std::traits::codec::{MessageEncoder, MessageDecoder};
use selium_std::traits::compression::{Compress, Decompress};
use selium_std::errors::Result;
use tokio::sync::MutexGuard;
use std::{sync::Arc, marker::PhantomData};

impl StreamBuilder<RequestorWantsRequestEncoder> {
    pub fn with_request_encoder<E, ReqItem>(
        self,
        encoder: E,
    ) -> StreamBuilder<RequestorWantsReplyDecoder<E, ReqItem>> {
        let next_state = RequestorWantsReplyDecoder::new(self.state, encoder);

        StreamBuilder {
            state: next_state,
            client: self.client,
        }
    }
}

impl<E, ReqItem> StreamBuilder<RequestorWantsReplyDecoder<E, ReqItem>> {
    pub fn with_request_compression<T>(mut self, comp: T) -> Self
    where
        T: Compress + Send + Sync + 'static,
    {
        self.state.compression = Some(Arc::new(comp));
        self
    }

    pub fn with_reply_decoder<D, ResItem>(
        self,
        decoder: D,
    ) -> StreamBuilder<RequestorWantsOpen<E, D, ReqItem, ResItem>> {
        let next_state = RequestorWantsOpen::new(self.state, decoder);

        StreamBuilder {
            state: next_state,
            client: self.client,
        }
    }
}

impl<E, D, ReqItem, ResItem> StreamBuilder<RequestorWantsOpen<E, D, ReqItem, ResItem>> {
    pub fn with_reply_decompression<T>(mut self, decomp: T) -> Self
    where
        T: Decompress + Send + Sync + 'static,
    {
        self.state.decompression = Some(Arc::new(decomp));
        self
    }
}

#[async_trait()]
impl<E, D, ReqItem, ResItem> Open for StreamBuilder<RequestorWantsOpen<E, D, ReqItem, ResItem>>
where
    E: MessageEncoder<ReqItem> + Send + Unpin,
    D: MessageDecoder<ResItem> + Send + Unpin,
    ReqItem: Unpin + Send,
    ResItem: Unpin + Send
{
    type Output = Requestor<E, D, ReqItem, ResItem>;

    async fn open(self) -> Result<Self::Output> {
        let headers = RequesterPayload { topic: self.state.endpoint };

        let requestor = Requestor::spawn(
            self.client,
            headers,
            self.state.encoder,
            self.state.decoder,
            self.state.compression,
            self.state.decompression,
        ).await?;

        Ok(requestor)
    }
}

pub struct Requestor<E, D, ReqItem, ResItem> {
    client: Client,
    stream: BiStream,
    headers: RequesterPayload,
    encoder: E,
    decoder: D,
    compression: Option<Comp>,
    decompression: Option<Decomp>,
    _req_marker: PhantomData<ReqItem>,
    _res_marker: PhantomData<ResItem>,
}

impl<E, D, ReqItem, ResItem> Requestor<E, D, ReqItem, ResItem> 
where
    E: MessageEncoder<ReqItem> + Send + Unpin,
    D: MessageDecoder<ResItem> + Send + Unpin,
    ReqItem: Unpin + Send,
    ResItem: Unpin + Send 
{
    async fn spawn(
        client: Client,
        headers: RequesterPayload,
        encoder: E,
        decoder: D,
        compression: Option<Comp>,
        decompression: Option<Decomp>,
    ) -> Result<Self> {
        let lock = client.connection.lock().await;
        let stream = Self::open_stream(lock, headers.clone()).await?;

        let requestor = Self {
            client,
            stream,
            headers,
            encoder,
            decoder,
            compression,
            decompression,
            _req_marker: PhantomData,
            _res_marker: PhantomData,
        };

        Ok(requestor)
    }

    async fn open_stream(
        lock: MutexGuard<'_, ClientConnection>,
        headers: RequesterPayload,
    ) -> Result<BiStream> {
        let mut stream = BiStream::try_from_connection(lock.conn()).await?;
        drop(lock);

        let frame = Frame::RegisterRequester(headers);
        stream.send(frame).await?;

        Ok(stream)
    }

    fn encode_request(&mut self, item: ReqItem) -> Result<Bytes> {
        let mut encoded = self.encoder.encode(item).map_err(CodecError::EncodeFailure)?;

        if let Some(comp) = self.compression.as_ref() {
            encoded = comp
                .compress(encoded)
                .map_err(CodecError::CompressFailure)?;
        }

        Ok(encoded)
    }

    fn decode_response(&mut self, mut bytes: Bytes) -> Result<ResItem> {
        if let Some(decomp) = self.decompression.as_ref() {
            bytes = decomp
                .decompress(bytes)
                .map_err(CodecError::DecompressFailure)?;
        }

        let mut mut_bytes = BytesMut::with_capacity(bytes.len());
        mut_bytes.extend_from_slice(&bytes);

        Ok(self.decoder.decode(&mut mut_bytes).map_err(CodecError::DecodeFailure)?)
    }

    pub async fn request(&mut self, req: ReqItem) -> Result<ResItem> {
        let encoded = self.encode_request(req)?;
        
        let req_payload = MessagePayload {
            headers: None,
            message: encoded,
        };

        let frame = Frame::Message(req_payload);
        println!("Sending {frame:?}");
        self.stream.send(frame).await?;

        if let Some(Ok(response)) = self.stream.next().await {
            println!("Requestor got response: {response:?}");
            if let Frame::Message(res_payload) = response {
                let decoded = self.decode_response(res_payload.message)?;
                Ok(decoded)
            } else {
                Err(SeliumError::RequestFailed)
            }
        } else {
            Err(SeliumError::RequestFailed)
        }
    }
}
