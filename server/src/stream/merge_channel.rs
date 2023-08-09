use std::{
    pin::Pin,
    task::{Context, Poll},
};

use anyhow::{anyhow, Result};
use futures::{ready, Stream};
use pin_project_lite::pin_project;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio_stream::{StreamMap, StreamNotifyClose};

pin_project! {
    #[project = MergeChannelProj]
    #[must_use = "streams do nothing unless polled"]
    pub struct MergeChannel<St> {
        #[pin]
        streams: StreamMap<usize, StreamNotifyClose<St>>,
        next_stream_id: usize,
        #[pin]
        handle: Receiver<StreamNotifyClose<St>>,
    }
}

pub struct MergeChannelHandle<St>(Sender<StreamNotifyClose<St>>);

impl<St> MergeChannel<St> {
    pub fn pair() -> (Self, MergeChannelHandle<St>) {
        let (tx, rx) = channel(10);
        (
            Self {
                streams: StreamMap::new(),
                next_stream_id: 0,
                handle: rx,
            },
            MergeChannelHandle::new(tx),
        )
    }
}

impl<St> Stream for MergeChannel<St>
where
    St: Stream + Unpin,
{
    type Item = St::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            match this.handle.poll_recv(cx) {
                Poll::Ready(Some(st)) => {
                    this.streams.insert(*this.next_stream_id, st);
                    *this.next_stream_id += 1;
                    println!("{}", this.next_stream_id);
                }
                Poll::Ready(None) => return Poll::Ready(None), // if handle is terminated, the stream is dead
                Poll::Pending => (),
            }

            match ready!(this.streams.as_mut().poll_next(cx)) {
                Some((_, Some(item))) => {
                    println!("Got message");
                    return Poll::Ready(Some(item))
                },
                // This stream has died (gets removed by StreamMap)
                Some((_, None)) => (),
                // All streams have died
                None => return Poll::Ready(None),
            }
        }
    }
}

impl<St> MergeChannelHandle<St> {
    fn new(handle: Sender<StreamNotifyClose<St>>) -> Self {
        Self(handle)
    }

    pub async fn add_stream(&self, stream: StreamNotifyClose<St>) -> Result<()> {
        self.0
            .send(stream)
            .await
            .map_err(|e| anyhow!(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{channel::mpsc, SinkExt, StreamExt};
    use tokio::pin;

    #[tokio::test]
    async fn integration_test() {
        let (mut tx, rx) = mpsc::channel(10);
        let (edge, handle) = MergeChannel::pair();
        pin!(edge);

        handle.add_stream(StreamNotifyClose::new(rx)).await.unwrap();
        tx.send("hello!").await.unwrap();
        tx.close().await.unwrap();

        assert_eq!(edge.next().await, Some("hello!"));
        assert_eq!(edge.next().await, None);
    }
}
