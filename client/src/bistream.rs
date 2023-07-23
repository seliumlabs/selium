use crate::protocol::{Frame, MessageCodec};
use anyhow::Result;
use futures::{Sink, SinkExt, Stream, StreamExt};
use quinn::{Connection, RecvStream, SendStream};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio_util::codec::{FramedRead, FramedWrite};

pub type ReadStream = FramedRead<RecvStream, MessageCodec>;
pub type WriteStream = FramedWrite<SendStream, MessageCodec>;

pub struct BiStream(pub WriteStream, pub ReadStream);

impl BiStream {
    pub async fn try_from_connection(connection: Connection) -> Result<Self> {
        let (send, recv) = connection.open_bi().await?;
        let send_stream = FramedWrite::new(send, MessageCodec);
        let recv_stream = FramedRead::new(recv, MessageCodec);

        Ok(Self(send_stream, recv_stream))
    }

    pub async fn finish(self) -> Result<()> {
        let write_stream = self.0;
        write_stream.into_inner().finish().await?;
        Ok(())
    }
}

impl Sink<Frame> for BiStream {
    type Error = anyhow::Error;

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

impl Stream for BiStream {
    type Item = Result<Frame>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.1.poll_next_unpin(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.1.size_hint()
    }
}
