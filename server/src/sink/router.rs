//! Router is based on tokio-stream::StreamMap

use std::{
    collections::{hash_map::IterMut, HashMap},
    fmt::Debug,
    hash::Hash,
    pin::Pin,
    str::FromStr,
    task::{Context, Poll},
};

use anyhow::{anyhow, Result};
use futures::Sink;
use log::error;
use selium_protocol::{Frame, MessagePayload};
use tokio::pin;

const CLIENT_ID_HEADER: &str = "cid";

#[must_use = "sinks do nothing unless you poll them"]
pub struct Router<K, V> {
    entries: HashMap<K, V>,
}

impl<K, V> Router<K, V>
where
    K: Eq + Hash,
{
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
        }
    }

    pub fn iter_mut(&mut self) -> IterMut<K, V> {
        self.entries.iter_mut()
    }

    pub fn insert(&mut self, k: K, sink: V) -> Option<V> {
        let ret = self.remove(&k);
        self.entries.insert(k, sink);

        ret
    }

    pub fn remove(&mut self, k: &K) -> Option<V> {
        self.entries.remove(k)
    }
}

impl<K, V> Default for Router<K, V>
where
    K: Eq + Hash,
{
    fn default() -> Self {
        Self::new()
    }
}

// Note that this is an inefficient implementation as a slow sink will block other sinks
// from receiving data, despite the slow sink not actually affecting other sinks.
// https://github.com/seliumlabs/selium/issues/148
impl<K, V> Sink<Frame> for Router<K, V>
where
    K: Eq + Hash + FromStr,
    <K as FromStr>::Err: std::error::Error + Send + Sync + 'static,
    V: Sink<Frame> + Unpin,
    V::Error: Debug,
    Self: Unpin,
{
    type Error = anyhow::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut pending = false;

        self.entries.retain(|_, sink| {
            if pending {
                return true;
            }
            pin!(sink);
            match sink.poll_ready(cx) {
                Poll::Pending => {
                    pending = true;
                    true
                }
                Poll::Ready(Err(_)) => false,
                Poll::Ready(Ok(())) => true,
            }
        });

        if pending {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
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
        let cid = headers.remove(CLIENT_ID_HEADER).unwrap().parse::<K>()?;
        let headers = if headers.is_empty() {
            None
        } else {
            Some(headers)
        };

        let item = self.entries.get_mut(&cid);

        if item.is_none() {
            return Err(anyhow!("Client ID not found"));
        }

        let sink = item.unwrap();
        pin!(sink);
        if let Err(e) = sink.start_send(Frame::Message(MessagePayload {
            headers,
            message: payload.message,
        })) {
            error!("Evicting broken sink from Router::start_send with err: {e:?}");
            self.entries.remove(&cid);
        }

        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut pending = false;

        self.entries.retain(|_, sink| {
            if pending {
                return true;
            }
            pin!(sink);
            match sink.poll_flush(cx) {
                Poll::Pending => {
                    pending = true;
                    true
                }
                Poll::Ready(Err(_)) => false,
                Poll::Ready(Ok(())) => true,
            }
        });

        if pending {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        let mut pending = false;

        self.entries.retain(|_, sink| {
            if pending {
                return true;
            }
            pin!(sink);
            match sink.poll_close(cx) {
                Poll::Pending => {
                    pending = true;
                    true
                }
                Poll::Ready(Err(_)) => false,
                Poll::Ready(Ok(())) => true,
            }
        });

        if pending {
            Poll::Pending
        } else {
            Poll::Ready(Ok(()))
        }
    }
}

impl<K, V> FromIterator<(K, V)> for Router<K, V>
where
    K: Eq + Hash,
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

impl<K, V> Extend<(K, V)> for Router<K, V>
where
    K: Eq + Hash,
{
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = (K, V)>,
    {
        self.entries.extend(iter);
    }
}
