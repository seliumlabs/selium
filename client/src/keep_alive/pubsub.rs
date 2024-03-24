use super::helpers::{is_recoverable_error, is_sink_disconnected, is_stream_disconnected};
use super::{BackoffStrategy, ConnectionStatus};
use crate::keep_alive::NextAttempt;
use crate::logging;
use crate::pubsub::Publisher;
use crate::traits::KeepAliveStream;
use futures::{ready, FutureExt, Sink, SinkExt, Stream, StreamExt};
use selium_std::errors::{QuicError, Result, SeliumError};
use selium_std::traits::codec::MessageEncoder;
use std::ops::{Deref, DerefMut};
use std::{
    pin::Pin,
    task::{Context, Poll},
};

#[doc(hidden)]
pub struct KeepAlive<T> {
    stream: T,
    backoff_strategy: BackoffStrategy,
    status: ConnectionStatus,
}

impl<T> KeepAlive<T>
where
    T: KeepAliveStream + Send + Unpin,
{
    pub fn new(stream: T, backoff_strategy: BackoffStrategy) -> Self {
        Self {
            stream,
            backoff_strategy,
            status: ConnectionStatus::Connected,
        }
    }

    fn on_disconnect(&mut self, cx: &mut Context<'_>) {
        if let ConnectionStatus::Connected = self.status {
            logging::keep_alive::connection_lost();
            self.status = ConnectionStatus::disconnected(self.backoff_strategy.clone());
        }

        if let ConnectionStatus::Disconnected(ref mut state) = self.status {
            let NextAttempt {
                duration,
                attempt_num,
                max_attempts,
            } = match state.attempts.next() {
                Some(next) => next,
                None => {
                    logging::keep_alive::too_many_retries();
                    self.status = ConnectionStatus::Exhausted;
                    cx.waker().wake_by_ref();
                    return;
                }
            };

            let connection = self.stream.get_connection();
            let headers = self.stream.get_headers();
            logging::keep_alive::reconnect_attempt(attempt_num, max_attempts);

            state.current_attempt = Box::pin(async move {
                tokio::time::sleep(duration).await;
                T::reestablish_connection(connection, headers).await
            });
        } else {
            unreachable!();
        }

        cx.waker().wake_by_ref();
    }

    fn poll_reconnect(&mut self, cx: &mut Context<'_>) -> Result<()> {
        if let ConnectionStatus::Disconnected(ref mut state) = self.status {
            match state.current_attempt.poll_unpin(cx) {
                Poll::Ready(Ok(stream)) => {
                    self.status = ConnectionStatus::Connected;
                    self.stream.on_reconnect(stream);
                    logging::keep_alive::successful_reconnection();
                    cx.waker().wake_by_ref();
                }
                Poll::Ready(Err(err)) if is_recoverable_error(&err) => {
                    logging::keep_alive::reconnect_error(&err);
                    self.on_disconnect(cx);
                }
                Poll::Ready(Err(err)) => {
                    logging::keep_alive::unrecoverable_error(&err);
                    return Err(err);
                }
                _ => (),
            }

            Ok(())
        } else {
            unreachable!();
        }
    }
}

impl<E, Item> KeepAlive<Publisher<E, Item>>
where
    E: MessageEncoder<Item> + Clone + Send + Unpin,
    Item: Unpin + Send + Clone,
{
    pub async fn finish(self) -> Result<()> {
        self.stream.finish().await
    }

    pub async fn duplicate(&self) -> Result<Self> {
        self.stream.duplicate().await
    }
}

impl<T, Item> Sink<Item> for KeepAlive<T>
where
    T: KeepAliveStream + Sink<Item, Error = SeliumError> + Send + Unpin,
    Item: Unpin + Send + Clone,
{
    type Error = SeliumError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.status {
            ConnectionStatus::Connected => {
                let result = self.stream.poll_ready_unpin(cx);

                if is_sink_disconnected(&result) {
                    self.on_disconnect(cx);
                    Poll::Pending
                } else {
                    result
                }
            }
            ConnectionStatus::Disconnected(_) => {
                self.poll_reconnect(cx)?;
                Poll::Pending
            }
            ConnectionStatus::Exhausted => Poll::Ready(Err(QuicError::TooManyRetries)?),
        }
    }

    fn start_send(mut self: Pin<&mut Self>, item: Item) -> Result<(), Self::Error> {
        self.stream.start_send_unpin(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.status {
            ConnectionStatus::Connected => {
                let result = self.stream.poll_flush_unpin(cx);

                if is_sink_disconnected(&result) {
                    self.on_disconnect(cx);
                    Poll::Pending
                } else {
                    result
                }
            }
            ConnectionStatus::Disconnected(_) => {
                self.poll_reconnect(cx)?;
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
            ConnectionStatus::Exhausted => Poll::Ready(Err(QuicError::TooManyRetries)?),
        }
    }
}

impl<T, Item> Stream for KeepAlive<T>
where
    T: KeepAliveStream + Stream<Item = Result<Item>> + Send + Unpin,
    Item: Unpin + Send,
{
    type Item = Result<Item>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.status {
            ConnectionStatus::Connected => {
                let result = ready!(self.stream.poll_next_unpin(cx));

                if let Some(result) = result {
                    if is_stream_disconnected(&result) {
                        self.on_disconnect(cx);
                        Poll::Pending
                    } else {
                        Poll::Ready(Some(result))
                    }
                } else {
                    self.on_disconnect(cx);
                    Poll::Pending
                }
            }
            ConnectionStatus::Disconnected(_) => {
                self.poll_reconnect(cx)?;
                Poll::Pending
            }
            ConnectionStatus::Exhausted => Poll::Ready(Some(Err(QuicError::TooManyRetries)?)),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.stream.size_hint()
    }
}

impl<T> Deref for KeepAlive<T>
where
    T: KeepAliveStream + Send + Unpin,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.stream
    }
}

impl<T> DerefMut for KeepAlive<T>
where
    T: KeepAliveStream + Send + Unpin,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.stream
    }
}
