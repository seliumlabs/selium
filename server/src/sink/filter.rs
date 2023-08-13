use std::{
    pin::Pin,
    task::{Context, Poll},
};

use futures::{ready, Future, Sink};
use pin_project_lite::pin_project;

pin_project! {
    #[must_use = "sinks do nothing unless polled"]
    pub struct Filter<Si, Fut, F, Item>
        where Si: Sink<Item>,
    {
        #[pin]
        sink: Si,
        f: F,
        #[pin]
        pending_fut: Option<Fut>,
        pending_item: Option<Item>,
    }
}

impl<Si, Fut, F, Item> Filter<Si, Fut, F, Item>
where
    Si: Sink<Item>,
    F: FnMut(&Item) -> Fut,
    Fut: Future<Output = bool>,
{
    pub(super) fn new(sink: Si, f: F) -> Self {
        Self {
            sink,
            f,
            pending_fut: None,
            pending_item: None,
        }
    }
}

impl<Si, Item, Fut, F> Filter<Si, Fut, F, Item>
where
    Si: Sink<Item>,
    F: FnMut(&Item) -> Fut,
    Fut: Future<Output = bool>,
{
    // Completes the processing of previous item if any
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Si::Error>> {
        let mut this = self.project();

        if let Some(fut) = this.pending_fut.as_mut().as_pin_mut() {
            let res = ready!(fut.poll(cx));
            this.pending_fut.set(None);
            if res {
                this.sink.start_send(this.pending_item.take().unwrap())?;
            }
            *this.pending_item = None;
        }

        Poll::Ready(Ok(()))
    }
}

impl<Si, Fut, F, Item> Sink<Item> for Filter<Si, Fut, F, Item>
where
    Si: Sink<Item>,
    F: FnMut(&Item) -> Fut,
    Fut: Future<Output = bool>,
{
    type Error = Si::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().poll(cx))?;
        ready!(self.project().sink.poll_ready(cx)?);
        Poll::Ready(Ok(()))
    }

    fn start_send(self: Pin<&mut Self>, item: Item) -> Result<(), Self::Error> {
        let mut this = self.project();

        this.pending_fut.set(Some((this.f)(&item)));
        *this.pending_item = Some(item);

        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().poll(cx))?;
        ready!(self.project().sink.poll_flush(cx)?);
        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().poll(cx))?;
        ready!(self.project().sink.poll_close(cx)?);
        Poll::Ready(Ok(()))
    }
}
