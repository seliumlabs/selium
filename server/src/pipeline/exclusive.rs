use futures::{Sink, SinkExt, Stream, StreamExt};
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::{Mutex, MutexGuard};

pub(super) struct Exclusive<'a, T> {
    inner: Arc<Mutex<T>>,
    guard: MutexGuard<'a, T>,
}

impl<'a, T> Exclusive<'a, T> {
    pub fn new(inner: Arc<Mutex<T>>) -> Self {
        let guard = inner.try_lock().expect("Lockable type must not be locked");

        Self { inner, guard }
    }
}

impl<'a, T> Deref for Exclusive<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard.deref()
    }
}

impl<'a, T> DerefMut for Exclusive<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard.deref_mut()
    }
}

impl<'a, St> Stream for Exclusive<'a, St>
where
    St: Stream,
{
    type Item = St::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.deref_mut().poll_next_unpin(cx)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.deref().size_hint()
    }
}

impl<'a, Si, Item> Sink<Item> for Exclusive<'a, Si>
where
    Si: Sink<Item>,
{
    type Error = Si::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.deref_mut().poll_ready_unpin(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Item) -> Result<(), Self::Error> {
        self.deref_mut().start_send_unpin(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.deref_mut().poll_flush_unpin(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.deref_mut().poll_close_unpin(cx)
    }
}
