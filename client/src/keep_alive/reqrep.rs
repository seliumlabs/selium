use super::backoff_strategy::*;
use super::helpers::is_recoverable_error;
use crate::logging;
use crate::request_reply::{Replier, Requestor};
use crate::traits::KeepAliveStream;
use futures::Future;
use selium_std::errors::QuicError;
use selium_std::errors::Result;
use selium_std::traits::codec::{MessageDecoder, MessageEncoder};
use std::fmt::Debug;

#[doc(hidden)]
pub struct KeepAlive<T> {
    stream: T,
    backoff_strategy: BackoffStrategy,
}

impl<T> KeepAlive<T>
where
    T: KeepAliveStream,
{
    pub fn new(stream: T, backoff_strategy: BackoffStrategy) -> Self {
        Self {
            stream,
            backoff_strategy,
        }
    }

    async fn try_reconnect(&mut self, attempts: &mut BackoffStrategyIter) -> Result<()> {
        logging::keep_alive::connection_lost();

        loop {
            let NextAttempt {
                duration,
                attempt_num,
                max_attempts,
            } = match attempts.next() {
                Some(next) => next,
                None => {
                    logging::keep_alive::too_many_retries();
                    return Err(QuicError::TooManyRetries)?;
                }
            };

            let connection = self.stream.get_connection();
            let headers = self.stream.get_headers();

            logging::keep_alive::reconnect_attempt(attempt_num, max_attempts);
            tokio::time::sleep(duration).await;

            match T::reestablish_connection(connection, headers).await {
                Ok(stream) => {
                    logging::keep_alive::successful_reconnection();
                    self.stream.on_reconnect(stream);
                    return Ok(());
                }
                Err(err) if is_recoverable_error(&err) => {
                    logging::keep_alive::reconnect_error(&err)
                }
                Err(err) => {
                    logging::keep_alive::unrecoverable_error(&err);
                    return Err(err);
                }
            }
        }
    }
}

impl<E, D, ReqItem, ResItem> Clone for KeepAlive<Requestor<E, D, ReqItem, ResItem>>
where
    E: MessageEncoder<ReqItem> + Send + Unpin + Clone,
    D: MessageDecoder<ResItem> + Send + Unpin + Clone,
    ReqItem: Unpin + Send + Clone,
    ResItem: Unpin + Send + Clone,
{
    fn clone(&self) -> Self {
        Self {
            stream: self.stream.clone(),
            backoff_strategy: self.backoff_strategy.clone(),
        }
    }
}

impl<E, D, ReqItem, ResItem> KeepAlive<Requestor<E, D, ReqItem, ResItem>>
where
    E: MessageEncoder<ReqItem> + Send + Unpin + Clone,
    D: MessageDecoder<ResItem> + Send + Unpin + Clone,
    ReqItem: Unpin + Send + Clone,
    ResItem: Unpin + Send + Clone,
{
    pub async fn request(&mut self, req: ReqItem) -> Result<ResItem> {
        let mut attempts = self.backoff_strategy.clone().into_iter();

        loop {
            match self.stream.request(req.clone()).await {
                Ok(res) => return Ok(res),
                Err(err) if is_recoverable_error(&err) => self.try_reconnect(&mut attempts).await?,
                Err(err) => {
                    logging::keep_alive::unrecoverable_error(&err);
                    return Err(err);
                }
            };
        }
    }
}

impl<D, E, Err, F, Fut, ReqItem, ResItem> KeepAlive<Replier<E, D, F, ReqItem, ResItem>>
where
    D: MessageDecoder<ReqItem> + Send + Unpin,
    E: MessageEncoder<ResItem> + Send + Unpin,
    Err: Debug,
    F: FnMut(ReqItem) -> Fut + Send + Unpin,
    Fut: Future<Output = std::result::Result<ResItem, Err>>,
    ReqItem: Unpin + Send,
    ResItem: Unpin + Send,
{
    pub async fn listen(&mut self) -> Result<()> {
        let mut attempts = self.backoff_strategy.clone().into_iter();

        loop {
            match self.stream.listen().await {
                Err(err) if !is_recoverable_error(&err) => {
                    logging::keep_alive::unrecoverable_error(&err);
                    return Err(err);
                }
                _ => self.try_reconnect(&mut attempts).await?,
            };
        }
    }
}
