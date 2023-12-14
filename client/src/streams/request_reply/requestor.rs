use super::states::*;
use crate::connection::ClientConnection;
use crate::streams::aliases::{Comp, Decomp};
use crate::streams::handle_reply;
use crate::traits::{Open, TryIntoU64};
use crate::{Client, StreamBuilder};
use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use futures::{SinkExt, StreamExt};
use selium_protocol::{
    BiStream, Frame, MessagePayload, ReadHalf, RequestId, RequestorPayload, TopicName, WriteHalf,
};
use selium_std::errors::Result;
use selium_std::errors::{CodecError, SeliumError};
use selium_std::traits::codec::{MessageDecoder, MessageEncoder};
use selium_std::traits::compression::{Compress, Decompress};
use std::collections::HashMap;
use std::time::Duration;
use std::{marker::PhantomData, sync::Arc};
use tokio::sync::oneshot::{self, Receiver, Sender};
use tokio::sync::{Mutex, MutexGuard};

type SharedPendingRequests = Arc<Mutex<HashMap<u32, Sender<Bytes>>>>;
type SharedReadHalf = Arc<Mutex<ReadHalf>>;
type SharedWriteHalf = Arc<Mutex<WriteHalf>>;

impl StreamBuilder<RequestorWantsRequestEncoder> {
    /// Specifies the encoder a [Requestor] uses for encoding outgoing requests. 
    ///
    /// An encoder can be any type implementing
    /// [MessageEncoder](crate::std::traits::codec::MessageEncoder).
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
    /// Specifies the compression implementation a [Requestor] uses for
    /// compressing outgoing requests.
    ///
    /// A compressor can be any type implementing [Compress](crate::std::traits::compression::Compress).
    pub fn with_request_compression<T>(mut self, comp: T) -> Self
    where
        T: Compress + Send + Sync + 'static,
    {
        self.state.compression = Some(Arc::new(comp));
        self
    }

    /// Specifies the decoder a [Requestor] uses for decoding incoming replies.
    ///
    /// A decoder can be any type implementing
    /// [MessageDecoder](crate::std::traits::codec::MessageDecoder).
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
    /// Specifies the decompression implementation a [Requestor] uses for decompressing incoming 
    /// reply payloads.
    ///
    /// A decompressor can be any type implementing
    /// [Decompress](crate::std::traits::compression::Decompress).
    pub fn with_reply_decompression<T>(mut self, decomp: T) -> Self
    where
        T: Decompress + Send + Sync + 'static,
    {
        self.state.decompression = Some(Arc::new(decomp));
        self
    }

    /// Overrides the default `request_timeout` setting for the [Requestor] stream. 
    ///
    /// Requests that exceed the timeout duration will be aborted, to prevent slow replies from
    /// blocking the current task for too long.
    ///
    /// Accepts any `timeout` argument that can be *fallibly* converted into a [u64] via the
    /// [TryIntoU64](crate::traits::TryIntoU64) trait.
    /// 
    /// # Errors
    ///
    /// Returns [Err] if the provided timeout fails to be convert to a [u64].
    pub fn with_request_timeout<T>(mut self, timeout: T) -> Result<Self>
    where
        T: TryIntoU64,
    {
        let millis = timeout.try_into_u64()?;
        self.state.request_timeout = Duration::from_millis(millis);
        Ok(self)
    }
}

#[async_trait()]
impl<E, D, ReqItem, ResItem> Open for StreamBuilder<RequestorWantsOpen<E, D, ReqItem, ResItem>>
where
    E: MessageEncoder<ReqItem> + Send + Unpin,
    D: MessageDecoder<ResItem> + Send + Unpin,
    ReqItem: Unpin + Send,
    ResItem: Unpin + Send,
{
    type Output = Requestor<E, D, ReqItem, ResItem>;

    async fn open(self) -> Result<Self::Output> {
        let topic = TopicName::try_from(self.state.endpoint.as_str())?;

        let headers = RequestorPayload { topic };

        let requestor = Requestor::spawn(
            self.client,
            headers,
            self.state.encoder,
            self.state.decoder,
            self.state.compression,
            self.state.decompression,
            self.state.request_timeout,
        )
        .await?;

        Ok(requestor)
    }
}

/// A Requestor stream that dispatches requests to any [Replier](crate::streams::request_reply::Replier) streams 
/// bound to the specified topic.
///
/// Requestor streams are synchronous, meaning that they will block the current task while awaiting
/// a response, as opposed to the asynchronous, non-blocking nature of [Publisher](crate::streams::pubsub::Publisher) streams. 
/// This makes them ideal for any use-cases relying on the RPC messaging pattern, when a response
/// is expected before resuming the task.
///
/// Once constructed, requests can be dispatched by calling and awaiting the
/// [request](Requestor::request) method. 
///
/// # Concurrency
///
/// The `Requestor` type derives the [Clone] trait, so requests can safely be made concurrently, as the `Requestor` 
/// will implicitly handle routing replies to the correct task.
#[derive(Clone)]
pub struct Requestor<E, D, ReqItem, ResItem> {
    request_id: Arc<RequestId>,
    write_half: SharedWriteHalf,
    encoder: E,
    decoder: D,
    compression: Option<Comp>,
    decompression: Option<Decomp>,
    request_timeout: Duration,
    pending_requests: SharedPendingRequests,
    _req_marker: PhantomData<ReqItem>,
    _res_marker: PhantomData<ResItem>,
}

impl<E, D, ReqItem, ResItem> Requestor<E, D, ReqItem, ResItem>
where
    E: MessageEncoder<ReqItem> + Send + Unpin,
    D: MessageDecoder<ResItem> + Send + Unpin,
    ReqItem: Unpin + Send,
    ResItem: Unpin + Send,
{
    async fn spawn(
        client: Client,
        headers: RequestorPayload,
        encoder: E,
        decoder: D,
        compression: Option<Comp>,
        decompression: Option<Decomp>,
        request_timeout: Duration,
    ) -> Result<Self> {
        let lock = client.connection.lock().await;

        let stream = Self::open_stream(lock, headers).await?;
        let (write_half, read_half) = stream.split();
        let write_half = Arc::new(Mutex::new(write_half));
        let read_half = Arc::new(Mutex::new(read_half));

        let pending_requests = Arc::new(Mutex::new(HashMap::new()));
        let request_id = Arc::new(RequestId::default());

        poll_replies(read_half.clone(), pending_requests.clone());

        let requestor = Self {
            request_id,
            write_half,
            encoder,
            decoder,
            compression,
            decompression,
            request_timeout,
            pending_requests,
            _req_marker: PhantomData,
            _res_marker: PhantomData,
        };

        Ok(requestor)
    }

    async fn open_stream(
        lock: MutexGuard<'_, ClientConnection>,
        headers: RequestorPayload,
    ) -> Result<BiStream> {
        let mut stream = BiStream::try_from_connection(lock.conn()).await?;
        drop(lock);

        let frame = Frame::RegisterRequestor(headers);
        stream.send(frame).await?;

        handle_reply(&mut stream).await?;
        Ok(stream)
    }

    fn encode_request(&mut self, item: ReqItem) -> Result<Bytes> {
        let mut encoded = self
            .encoder
            .encode(item)
            .map_err(CodecError::EncodeFailure)?;

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

        Ok(self
            .decoder
            .decode(&mut mut_bytes)
            .map_err(CodecError::DecodeFailure)?)
    }

    async fn queue_request(&mut self) -> (u32, Receiver<Bytes>) {
        let (tx, rx) = oneshot::channel();
        let mut lock = self.pending_requests.lock().await;
        let req_id = self.request_id.next_id();

        lock.insert(req_id, tx);

        (req_id, rx)
    }

    /// Dispatches a request and blocks the current task while waiting for a response, or a request
    /// timeout.
    ///
    /// # Errors
    ///
    /// Returns `Err` under the current conditions:
    ///
    /// - The request fails to be encoded.
    /// - The encoded request fails to dispatched.
    /// - The request times out.
    /// - The reply fails to be decoded.
    pub async fn request(&mut self, req: ReqItem) -> Result<ResItem> {
        let encoded = self.encode_request(req)?;
        let (req_id, rx) = self.queue_request().await;

        let mut headers = HashMap::new();
        headers.insert("req_id".to_owned(), req_id.to_string());

        let req_payload = MessagePayload {
            headers: Some(headers),
            message: encoded,
        };

        let frame = Frame::Message(req_payload);
        self.write_half.lock().await.send(frame).await?;

        let response = tokio::time::timeout(self.request_timeout, rx)
            .await
            .map_err(|_| SeliumError::RequestTimeout)?
            .map_err(|_| SeliumError::RequestFailed)?;

        let decoded = self.decode_response(response)?;

        Ok(decoded)
    }
}

fn poll_replies(read_half: SharedReadHalf, pending_requests: SharedPendingRequests) {
    tokio::spawn(async move {
        let mut read_half = read_half.lock().await;

        while let Some(Ok(Frame::Message(res_payload))) = read_half.next().await {
            if let Some(headers) = res_payload.headers {
                if let Some(req_id) = headers.get("req_id") {
                    let mut lock = pending_requests.lock().await;

                    if let Ok(req_id) = req_id.parse() {
                        if let Some(pending) = lock.remove(&req_id) {
                            let _ = pending.send(res_payload.message);
                        }
                    }
                }
            }
        }
    });
}
