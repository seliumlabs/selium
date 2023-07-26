use std::{
    collections::HashMap,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{ready, Sink, Stream};
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
    pub(super) fn new(sink: Si) -> Self {
        Self {
            sink,
            cache: HashMap::new(),
            last_sent: 0,
        }
    }

    fn try_send_cached(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Si::Error>> {
        let mut this = self.project();
        ready!(this.sink.as_mut().poll_ready(cx))?;
        while let Some(item) = this.cache.remove(&(*this.last_sent + 1)) {
            this.sink.as_mut().start_send(item)?;
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
            sink.start_send(item)
        } else {
            debug_assert!(seq > *last_sent);
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
    fn ordered(self) -> Ordered<Self, Item>
    where
        Self: Sized,
    {
        Ordered::new(self)
    }
}
