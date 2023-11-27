//! Router is based on tokio-stream::StreamMap

use std::{
    borrow::{Borrow, BorrowMut},
    fmt::Debug,
    hash::Hash,
    pin::Pin,
    slice::IterMut,
    task::{Context, Poll},
};

use anyhow::{anyhow, Result};
use futures::Sink;
use log::error;
use selium_protocol::{Frame, MessagePayload};
use tokio::pin;

const CLIENT_ID_HEADER: &'static str = "cid";

#[must_use = "sinks do nothing unless you poll them"]
pub struct Router<K, V> {
    entries: Vec<(K, V)>,
}

impl<K, V> Router<K, V> {
    pub fn new() -> Self {
        Self { entries: vec![] }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
        }
    }

    pub fn iter_mut(&mut self) -> IterMut<'_, (K, V)> {
        self.entries.iter_mut()
    }

    pub fn insert(&mut self, k: K, sink: V) -> Option<V>
    where
        K: Hash + Eq,
    {
        let ret = self.remove(&k);
        self.entries.push((k, sink));

        ret
    }

    pub fn remove<Q: ?Sized>(&mut self, k: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        for i in 0..self.entries.len() {
            if self.entries[i].0.borrow() == k {
                return Some(self.entries.swap_remove(i).1);
            }
        }

        None
    }
}

impl<K, V> Default for Router<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

// Note that this is an inefficient implementation as a slow sink will block other sinks
// from receiving data, despite the slow sink not actually affecting other sinks.
impl<K, V> Sink<Frame> for Router<K, V>
where
    K: Unpin,
    V: Sink<Frame> + Unpin,
    V::Error: Debug,
{
    type Error = anyhow::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut idx = 0;
        while idx < self.entries.len() {
            let (_, sink) = self.entries[idx].borrow_mut();
            pin!(sink);
            match sink.poll_ready(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(_)) => {
                    self.entries.swap_remove(idx);
                }
                Poll::Ready(Ok(())) => idx += 1,
            }
        }

        Poll::Ready(Ok(()))
    }

    fn start_send(mut self: Pin<&mut Self>, frame: Frame) -> Result<(), Self::Error> {
        let payload = frame.unwrap_message();
        if payload.headers.is_none() {
            return Err(anyhow!("Expected headers for message"));
        }
        let mut headers = payload.headers.unwrap();
        if !headers.contains_key(CLIENT_ID_HEADER) {
            return Err(anyhow!("Missing CLIENT_ID_HEADER in message headers"));
        }
        let cid = headers.remove(CLIENT_ID_HEADER).unwrap().parse::<usize>()?;
        let headers = if headers.is_empty() {
            None
        } else {
            Some(headers)
        };

        let (_, sink) = self.entries[cid].borrow_mut();
        pin!(sink);
        if let Err(e) = sink.start_send(Frame::Message(MessagePayload {
            headers,
            message: payload.message,
        })) {
            error!("Evicting broken sink from Router::start_send with err: {e:?}");
            self.entries.swap_remove(cid);
        }

        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut idx = 0;
        while idx < self.entries.len() {
            let (_, sink) = self.entries[idx].borrow_mut();
            pin!(sink);
            match sink.poll_flush(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(_)) => {
                    self.entries.swap_remove(idx);
                }
                Poll::Ready(Ok(())) => idx += 1,
            }
        }

        Poll::Ready(Ok(()))
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut idx = 0;
        while idx < self.entries.len() {
            let (_, sink) = self.entries[idx].borrow_mut();
            pin!(sink);
            match sink.poll_close(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(_)) => {
                    self.entries.swap_remove(idx);
                }
                Poll::Ready(Ok(())) => idx += 1,
            }
        }

        Poll::Ready(Ok(()))
    }
}

impl<K, V> FromIterator<(K, V)> for Router<K, V>
where
    K: Hash + Eq,
{
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        let iterator = iter.into_iter();
        let (lower_bound, _) = iterator.size_hint();
        let mut sink_map = Self::with_capacity(lower_bound);

        for (key, value) in iterator {
            sink_map.insert(key, value);
        }

        sink_map
    }
}

impl<K, V> Extend<(K, V)> for Router<K, V> {
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = (K, V)>,
    {
        self.entries.extend(iter);
    }
}
