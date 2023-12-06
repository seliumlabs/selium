use crate::traits::{ShutdownSink, ShutdownStream};
use crate::{error_codes, Frame, MessageCodec};
use futures::{Sink, SinkExt, Stream, StreamExt};
use quinn::{Connection, RecvStream, SendStream, StreamId};
use selium_std::errors::{QuicError, Result, SeliumError};
use std::{
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll},
};
use tokio_util::codec::{FramedRead, FramedWrite};

pub struct WriteHalf(FramedWrite<SendStream, MessageCodec>);

pub struct ReadHalf(FramedRead<RecvStream, MessageCodec>);

pub struct BiStream {
    write: WriteHalf,
    read: ReadHalf,
}

impl Sink<Frame> for WriteHalf {
    type Error = SeliumError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_ready_unpin(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Frame) -> Result<(), Self::Error> {
        self.0.start_send_unpin(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_flush_unpin(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.0.poll_close_unpin(cx)
    }
}

impl ShutdownSink for WriteHalf {
    fn shutdown_sink(&mut self) {
        let _ = self.0.get_mut().reset(error_codes::SHUTDOWN_IN_PROGRESS);
    }
}

impl From<SendStream> for WriteHalf {
    fn from(send: SendStream) -> Self {
        Self(FramedWrite::new(send, MessageCodec))
    }
}

impl Deref for WriteHalf {
    type Target = SendStream;

    fn deref(&self) -> &Self::Target {
        self.0.get_ref()
    }
}

impl DerefMut for WriteHalf {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.get_mut()
    }
}

impl Stream for ReadHalf {
    type Item = Result<Frame, SeliumError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_next_unpin(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl ShutdownStream for ReadHalf {
    fn shutdown_stream(&mut self) {
        let _ = self.0.get_mut().stop(error_codes::SHUTDOWN_IN_PROGRESS);
    }
}

impl From<RecvStream> for ReadHalf {
    fn from(recv: RecvStream) -> Self {
        Self(FramedRead::new(recv, MessageCodec))
    }
}

impl Deref for ReadHalf {
    type Target = RecvStream;

    fn deref(&self) -> &Self::Target {
        self.0.get_ref()
    }
}

impl DerefMut for ReadHalf {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.get_mut()
    }
}

impl BiStream {
    pub async fn try_from_connection(connection: &Connection) -> Result<Self> {
        let stream = connection
            .open_bi()
            .await
            .map_err(QuicError::ConnectionError)?;
        Ok(Self::from(stream))
    }

    pub fn split(self) -> (WriteHalf, ReadHalf) {
        (self.write, self.read)
    }

    pub fn get_recv_stream_id(&self) -> StreamId {
        self.read.id()
    }

    pub fn get_send_stream_id(&self) -> StreamId {
        self.write.id()
    }

    pub fn read(&mut self) -> &mut RecvStream {
        &mut self.read
    }

    pub fn write(&mut self) -> &mut SendStream {
        &mut self.write
    }

    pub async fn finish(&mut self) -> Result<()> {
        self.write.finish().await.map_err(QuicError::WriteError)?;
        Ok(())
    }
}

impl From<(SendStream, RecvStream)> for BiStream {
    fn from((send, recv): (SendStream, RecvStream)) -> Self {
        Self {
            write: send.into(),
            read: recv.into(),
        }
    }
}

impl Sink<Frame> for BiStream {
    type Error = SeliumError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.write.poll_ready_unpin(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Frame) -> Result<(), Self::Error> {
        self.write.start_send_unpin(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.write.poll_flush_unpin(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.write.poll_close_unpin(cx)
    }
}

impl Stream for BiStream {
    type Item = Result<Frame, SeliumError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.read.poll_next_unpin(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.read.size_hint()
    }
}

impl ShutdownSink for BiStream {
    fn shutdown_sink(&mut self) {
        let _ = self.write().reset(error_codes::SHUTDOWN_IN_PROGRESS);
    }
}

impl ShutdownStream for BiStream {
    fn shutdown_stream(&mut self) {
        let _ = self.read().stop(error_codes::SHUTDOWN_IN_PROGRESS);
    }
}
