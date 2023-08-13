use std::{
    collections::HashMap,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{ready, Sink, Stream};
use log::{debug, trace};
use pin_project_lite::pin_project;

pin_project! {
    #[project = OrderedProj]
    #[derive(Debug)]
    #[must_use = "sinks do nothing unless polled"]
    pub struct Ordered<Si, Item> {
        #[pin]
        sink: Si,
        cache: HashMap<usize, Item>,
        last_sent: usize,
    }
}

impl<Si: Sink<Item>, Item> Ordered<Si, Item> {
    pub(super) fn new(sink: Si, last_sent: usize) -> Self {
        Self {
            sink,
            cache: HashMap::new(),
            last_sent,
        }
    }

    fn try_send_cached(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Si::Error>> {
        let mut this = self.project();
        ready!(this.sink.as_mut().poll_ready(cx))?;
        while let Some(item) = this.cache.remove(&(*this.last_sent + 1)) {
            trace!(
                "Sending ordered message from cache: {}",
                *this.last_sent + 1
            );
            this.sink.as_mut().start_send(item)?;
            *this.last_sent += 1;
            if !this.cache.is_empty() {
                ready!(this.sink.as_mut().poll_ready(cx))?;
            }
        }
        Poll::Ready(Ok(()))
    }
}

impl<S, Item> Stream for Ordered<S, Item>
where
    S: Sink<Item> + Stream,
{
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().sink.poll_next(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.sink.size_hint()
    }
}

impl<Si: Sink<Item>, Item> Sink<(usize, Item)> for Ordered<Si, Item> {
    type Error = Si::Error;

    fn poll_ready(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, (seq, item): (usize, Item)) -> Result<(), Self::Error> {
        let OrderedProj {
            sink,
            cache,
            last_sent,
        } = self.project();

        if seq == *last_sent + 1 {
            *last_sent = seq;
            trace!("Sending ordered message: {seq}");
            sink.start_send(item)
        } else if seq < *last_sent {
            debug!("Sending sequence {seq} out of order (last sent={last_sent})");
            trace!("Sending ordered message: {seq}");
            sink.start_send(item)
        } else {
            trace!(
                "Caching ordered message: {seq} - waiting for {}",
                *last_sent + 1
            );
            cache.insert(seq, item);
            Ok(())
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().try_send_cached(cx))?;
        self.project().sink.poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().try_send_cached(cx))?;
        self.project().sink.poll_close(cx)
    }
}

impl<T: ?Sized, Item> OrderedExt<Item> for T where T: Sink<Item> {}

pub trait OrderedExt<Item>: Sink<Item> {
    fn ordered(self, last_sent: usize) -> Ordered<Self, Item>
    where
        Self: Sized,
    {
        debug!("Ordering messages starting from {last_sent}");
        Ordered::new(self, last_sent)
    }
}
