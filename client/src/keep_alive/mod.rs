mod backoff_strategy;
mod connection_status;

pub use backoff_strategy::*;
pub(crate) use connection_status::*;

use crate::{Publisher, traits::KeepAliveStream};
use anyhow::{Result, anyhow};
use futures::{FutureExt, Sink, SinkExt, Stream, StreamExt, ready};
use selium_std::traits::codec::MessageEncoder;
use std::{
    pin::Pin,
    task::{Context, Poll}, marker::PhantomData,
};

pub struct KeepAlive<T, Item> {
    stream: T,
    backoff_strategy: BackoffStrategy,
    status: ConnectionStatus,
    _marker: PhantomData<Item>
}

impl<T, Item> KeepAlive<T, Item>
where
    T: KeepAliveStream + Send + Unpin,
    Item: Send + Unpin
{
    pub fn new(stream: T, backoff_strategy: BackoffStrategy) -> Self {
        Self {
            stream,
            backoff_strategy,
            status: ConnectionStatus::Connected,
            _marker: PhantomData
        }
    }

    fn on_disconnect(&mut self, cx: &mut Context<'_>) {
        if let ConnectionStatus::Connected = self.status {
            self.status = ConnectionStatus::disconnected(self.backoff_strategy.clone());
        }

        if let ConnectionStatus::Disconnected(ref mut state) = self.status {
            let duration = match state.attempts.next() {
                Some(duration) => duration,
                None => {
                    self.status = ConnectionStatus::Exhausted;
                    cx.waker().wake_by_ref();
                    return;
                }
            };

            let connection = self.stream.get_connection();
            let headers = self.stream.get_headers();

            state.current_attempt = Box::pin(async move {
                tokio::time::sleep(duration).await;
                T::reestablish_connection(connection, headers).await
            });
        } else {
            unreachable!();
        }

        cx.waker().wake_by_ref();
    }

    fn poll_reconnect(&mut self, cx: &mut Context<'_>) {
        if let ConnectionStatus::Disconnected(ref mut state) = self.status {
            match state.current_attempt.poll_unpin(cx) {
                Poll::Ready(Ok(stream)) => {
                    self.status = ConnectionStatus::Connected;
                    self.stream.on_reconnect(stream);
                    cx.waker().wake_by_ref();
                }
                Poll::Ready(Err(_)) => {
                    self.on_disconnect(cx);
                }
                _ => (),
            }
        } else {
            unreachable!();
        }
    }

    // TODO: This needs to match on explicit RecvStream errors
    fn is_stream_disconnected(&self, result: &Result<Item>) -> bool {
        if let Err(_) = result {
            true
        } else {
            false
        }
    }

    // TODO: This needs to match on explicit SendStream errors
    fn is_sink_disconnected(&self, result: &Poll<Result<()>>) -> bool {
        if let Poll::Ready(Err(_)) = result {
            true
        } else {
            false
        }
    }
}

impl<E, Item> KeepAlive<Publisher<E, Item>, Item> 
where
    E: MessageEncoder<Item> + Clone + Send + Unpin,
    Item: Unpin + Send,
{
    pub async fn finish(self) -> Result<()> {
        self.stream.finish().await
    }

    pub async fn duplicate(&self) -> Result<Self> {
        self.stream.duplicate().await
    }
}

impl<T, Item> Sink<Item> for KeepAlive<T, Item>
where
    T: KeepAliveStream + Sink<Item, Error = anyhow::Error> + Send + Unpin,
    Item: Clone + Unpin + Send
{
    type Error = anyhow::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.status {
            ConnectionStatus::Connected => {
                let result = self.stream.poll_ready_unpin(cx);

                if self.is_sink_disconnected(&result) {
                    self.on_disconnect(cx);
                    Poll::Pending
                } else {
                    result
                }
            }
            ConnectionStatus::Disconnected(_) => {
                self.poll_reconnect(cx);
                Poll::Pending
            }
            ConnectionStatus::Exhausted => Poll::Ready(Err(anyhow!("Too many retries!"))),
        }
    }

    fn start_send(mut self: Pin<&mut Self>, item: Item) -> Result<(), Self::Error> {
        self.stream.start_send_unpin(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.status {
            ConnectionStatus::Connected => {
                let result = self.stream.poll_flush_unpin(cx);

                if self.is_sink_disconnected(&result) {
                    self.on_disconnect(cx);
                    Poll::Pending
                } else {
                    result
                }
            }
            ConnectionStatus::Disconnected(_) => {
                self.poll_reconnect(cx);
                Poll::Pending
            }
            ConnectionStatus::Exhausted => Poll::Ready(Ok(())),
        }
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.status {
            ConnectionStatus::Connected => {
                let result = self.stream.poll_close_unpin(cx);

                if result.is_ready() {
                    self.on_disconnect(cx);
                }

                result
            }
            ConnectionStatus::Disconnected(_) => Poll::Pending,
            ConnectionStatus::Exhausted => Poll::Ready(Err(anyhow!("Too many retries!"))),
        }
    }
}

impl<T, Item> Stream for KeepAlive<T, Item> 
where
    T: KeepAliveStream + Stream<Item = Result<Item>> + Send + Unpin,
    Item: Unpin + Send
{
    type Item = Result<Item>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
       match self.status {
            ConnectionStatus::Connected => {
                let result = ready!(self.stream.poll_next_unpin(cx));

                if let Some(result) = result {
                    if self.is_stream_disconnected(&result) {
                        self.on_disconnect(cx);
                        Poll::Pending
                    } else {
                        Poll::Ready(Some(result))
                    }
                } else {
                    self.on_disconnect(cx);
                    Poll::Pending
                }

            },
            ConnectionStatus::Disconnected(_) => {
                self.poll_reconnect(cx);
                Poll::Pending
            },
            ConnectionStatus::Exhausted => Poll::Ready(Some(Err(anyhow!("Too many retries!"))))
        } 
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
       self.stream.size_hint() 
    }
}