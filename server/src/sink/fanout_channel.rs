use std::{
    pin::Pin,
    fmt::{Debug, Display},
    task::{Context, Poll},
};

use anyhow::{anyhow, Result};
use futures::Sink;
use pin_project_lite::pin_project;
use tokio::sync::mpsc::{channel, Receiver, Sender};

use super::fanout_many::FanoutMany;

pin_project! {
    #[project = FanoutChannelProj]
    #[must_use = "sinks do nothing unless polled"]
    pub struct FanoutChannel<Si> {
        #[pin]
        sinks: FanoutMany<usize, Si>,
        next_sink_id: usize,
        #[pin]
        handle: Receiver<Si>,
    }
}

pub struct FanoutChannelHandle<Si>(Sender<Si>);
impl<Si> FanoutChannel<Si> {
    pub fn pair() -> (Self, FanoutChannelHandle<Si>) {
        let (tx, rx) = channel(10);
        (
            Self {
                sinks: FanoutMany::new(),
                next_sink_id: 0,
                handle: rx,
            },
            FanoutChannelHandle::new(tx),
        )
    }

    pub fn add_sink(&mut self, sink: Si) {
        self.sinks.insert(self.next_sink_id, sink);
        self.next_sink_id += 1;
    }
}

impl<Item, Si> Sink<Item> for FanoutChannel<Si>
where
    Si: Sink<Item> + Unpin,
    Si::Error: Debug + Display + Send + Sync + 'static,
    Item: Clone + Unpin,
{
    type Error = Si::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut this = self.project();

        match this.handle.poll_recv(cx) {
            Poll::Ready(Some(si)) => {
                this.sinks.insert(*this.next_sink_id, si);
                *this.next_sink_id += 1;
            }
            // If handle is terminated, the stream is dead
            Poll::Ready(None) => {
                return Poll::Ready(Err(anyhow!("Handle terminated")
                    .downcast::<Self::Error>()
                    .unwrap()))
            }
            Poll::Pending => (),
        }

        this.sinks.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Item) -> Result<(), Self::Error> {
        let this = self.project();
        this.sinks.start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        this.sinks.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let this = self.project();
        this.sinks.poll_close(cx)
    }
}

impl<Si> FanoutChannelHandle<Si> {
    fn new(handle: Sender<Si>) -> Self {
        Self(handle)
    }

    pub async fn add_sink(&self, sink: &Si) -> Result<()> {
        self.0.send(sink).await.map_err(|e| anyhow!(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{channel::mpsc, SinkExt};
    use tokio::pin;

    #[tokio::test]
    async fn integration_test() {
        let (tx, mut rx) = mpsc::channel(10);
        let (edge, handle) = FanoutChannel::pair();
        pin!(edge);

        handle.add_sink(&tx).await.unwrap();
        edge.send("hello!").await.unwrap();

        assert_eq!(rx.try_next().unwrap(), Some("hello!"));
    }
}
