use super::backoff_strategy::*;
use super::helpers::is_recoverable_error;
use selium_std::errors::QuicError;
use crate::request_reply::{Replier, Requestor};
use crate::traits::KeepAliveStream;
use futures::Future;
use selium_std::errors::Result;
use selium_std::traits::codec::{MessageEncoder, MessageDecoder};
use std::fmt::Debug;

pub struct KeepAliveReqRep<T> {
    stream: T,
    backoff_strategy: BackoffStrategy,
}

impl<T> KeepAliveReqRep<T> where T: KeepAliveStream {
    pub fn new(stream: T, backoff_strategy: BackoffStrategy) -> Self {
        Self {
            stream,
            backoff_strategy,
        }
    }

    async fn try_reconnect(&mut self, attempts: &mut BackoffStrategyIter) -> Result<()> {
        loop {
            let duration = attempts.next().ok_or(QuicError::TooManyRetries)?;
            let connection = self.stream.get_connection();
            let headers = self.stream.get_headers();

            tokio::time::sleep(duration).await;

            match T::reestablish_connection(connection, headers).await {
                Ok(stream) => {
                    self.stream.on_reconnect(stream);
                    return Ok(());
                },
                Err(err) if is_recoverable_error(&err) => (),
                Err(err) => return Err(err)
            }
        }
    }
}

impl<E, D, ReqItem, ResItem> KeepAliveReqRep<Requestor<E, D, ReqItem, ResItem>>
where
    E: MessageEncoder<ReqItem> + Send + Unpin,
    D: MessageDecoder<ResItem> + Send + Unpin,
    ReqItem: Unpin + Send + Clone,
    ResItem: Unpin + Send,
{
    pub async fn request(&mut self, req: ReqItem) -> Result<ResItem> {
        let mut attempts = self.backoff_strategy.clone().into_iter();

        loop {
            match self.stream.request(req.clone()).await {
                Ok(res) => return Ok(res),
                Err(err) if is_recoverable_error(&err) => self.try_reconnect(&mut attempts).await?,
                Err(err) => return Err(err)
            };
        }
    }
}

impl<D, E, Err, F, Fut, ReqItem, ResItem> KeepAliveReqRep<Replier<E, D, F, ReqItem, ResItem>>
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
                Err(err) if !is_recoverable_error(&err) => return Err(err),
                _ => self.try_reconnect(&mut attempts).await?
            };
        }
    }
}