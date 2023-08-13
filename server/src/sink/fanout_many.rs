//! FanoutMany is based on tokio-stream::StreamMap

use std::{
    borrow::{Borrow, BorrowMut},
    hash::Hash,
    pin::Pin,
    task::{Context, Poll},
};

use anyhow::Result;
use futures::Sink;
use tokio::pin;

#[must_use = "sinks do nothing unless you poll them"]
pub struct FanoutMany<K, V> {
    entries: Vec<(K, V)>,
}

impl<K, V> FanoutMany<K, V> {
    pub fn new() -> Self {
        Self { entries: vec![] }
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
        }
    }

    // pub fn iter(&self) -> impl Iterator<Item = &(K, V)> {
    //     self.entries.iter()
    // }

    // pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut (K, V)> {
    //     self.entries.iter_mut()
    // }

    // pub fn keys(&self) -> impl Iterator<Item = &K> {
    //     self.iter().map(|(k, _)| k)
    // }

    // pub fn values(&self) -> impl Iterator<Item = &V> {
    //     self.iter().map(|(_, v)| v)
    // }

    // pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
    //     self.iter_mut().map(|(_, v)| v)
    // }

    // pub fn capacity(&self) -> usize {
    //     self.entries.capacity()
    // }

    // pub fn len(&self) -> usize {
    //     self.entries.len()
    // }

    // pub fn is_empty(&self) -> bool {
    //     self.entries.is_empty()
    // }

    // pub fn clear(&mut self) {
    //     self.entries.clear();
    // }

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

    // pub fn contains_key<Q: ?Sized>(&self, k: &Q) -> bool
    // where
    //     K: Borrow<Q>,
    //     Q: Hash + Eq,
    // {
    //     for i in 0..self.entries.len() {
    //         if self.entries[i].0.borrow() == k {
    //             return true;
    //         }
    //     }

    //     false
    // }
}

impl<K, V> Default for FanoutMany<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V, Item> Sink<Item> for FanoutMany<K, V>
where
    K: Unpin,
    V: Sink<Item> + Unpin,
    Item: Clone + Unpin,
{
    type Error = V::Error;

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

    fn start_send(mut self: Pin<&mut Self>, item: Item) -> Result<(), Self::Error> {
        let len = self.entries.len();
        for idx in 0..len {
            let (_, sink) = self.entries[idx].borrow_mut();
            pin!(sink);
            if idx == len - 1 {
                sink.start_send(item)?;
                break;
            };
            sink.start_send(item.clone())?;
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

impl<K, V> FromIterator<(K, V)> for FanoutMany<K, V>
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

impl<K, V> Extend<(K, V)> for FanoutMany<K, V> {
    fn extend<T>(&mut self, iter: T)
    where
        T: IntoIterator<Item = (K, V)>,
    {
        self.entries.extend(iter);
    }
}
