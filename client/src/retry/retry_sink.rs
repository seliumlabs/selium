use crate::Publisher;
use super::RetryStrategy;
use anyhow::Result;
use futures::{Sink, Future, SinkExt, FutureExt};
use selium_std::traits::codec::MessageEncoder;
use std::{
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

pub enum RetryStatus {
    Connected,
    Disconnected,
    Exhausted,
}

pub struct RetrySink<E, Item> {
    sink: Publisher<E, Item>,
    retry_strategy: RetryStrategy,
    status: RetryStatus,
    attempts_iter: Box<dyn Iterator<Item = Duration>>,
    current_attempt: Pin<Box<dyn Future<Output = Result<()>>>>,
    _marker: PhantomData<Item>,
}

impl<E, Item> RetrySink<E, Item> 
where
    E: MessageEncoder<Item> + Clone + Send + Unpin,
    Item: Unpin,
{
    pub fn new(sink: Publisher<E, Item>, retry_strategy: RetryStrategy) -> Self {
        let attempts_iter = Box::new(retry_strategy.inner().into_iter());
        let current_attempt = Box::pin(async { Ok(()) });

        Self { 
            sink, 
            retry_strategy, 
            status: RetryStatus::Connected, 
            attempts_iter, 
            current_attempt, 
            _marker: PhantomData 
        }
    }

    fn restablish_connection(&mut self, cx: &mut Context<'_>) {
        let duration = match self.attempts_iter.next() {
            Some(duration) => duration,
            None => {
                self.status = RetryStatus::Exhausted;
                return;
            }
        };

        self.current_attempt = Box::pin({
            let mut conn = self.sink.connection();
            async move {
                tokio::time::sleep(duration).await;
                println!("I'm trying to reconnect!");
                conn.reconnect().await?;
                Ok(())
            }
        });

        cx.waker().wake_by_ref();
    } 

    fn poll_reconnect(&mut self, cx: &mut Context<'_>) {
        match self.current_attempt.poll_unpin(cx) {
            Poll::Ready(Ok(())) => {
                cx.waker().wake_by_ref(); 
                self.status = RetryStatus::Connected;
            },
            Poll::Ready(Err(err)) => {
                println!("Failed to reconnect: {err:?}");
                self.restablish_connection(cx);
            }
            Poll::Pending => {}
        }
    }
}

impl<E, Item> Sink<Item> for RetrySink<E, Item>
where
    E: MessageEncoder<Item> + Clone + Send + Unpin,
    Item: Unpin,
{
    type Error = anyhow::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.status {
            RetryStatus::Connected => self.poll_ready_unpin(cx),
            RetryStatus::Disconnected => {
                self.poll_reconnect(cx);
                Poll::Pending
            },
            RetryStatus::Exhausted => Poll::Ready(Ok(())),
        }
    }

    fn start_send(mut self: Pin<&mut Self>, item: Item) -> Result<(), Self::Error> {
        self.start_send_unpin(item)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.status {
            RetryStatus::Connected => {
                self.poll_flush_unpin(cx)
            },
            RetryStatus::Disconnected => {
                self.poll_reconnect(cx);
                Poll::Pending
            },
            RetryStatus::Exhausted => Poll::Ready(Ok(())),
        }
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.status {
            RetryStatus::Connected => self.poll_close_unpin(cx),
            RetryStatus::Disconnected => Poll::Pending,
            RetryStatus::Exhausted => Poll::Ready(Ok(())),
        }
    }
}
