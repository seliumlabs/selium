use std::time::Duration;
use std::pin::Pin;
use futures::Future;
use selium_protocol::BiStream;
use anyhow::Result;
use super::BackoffStrategy;

pub type AttemptsIterator = Box<dyn Iterator<Item = Duration> + Send>;
pub type AttemptFut = Pin<Box<dyn Future<Output = Result<BiStream>> + Send>>;

pub enum ConnectionStatus {
    Connected,
    Disconnected(ReconnectState),
    Exhausted,
}

impl Default for ConnectionStatus {
    fn default() -> Self {
        Self::Connected
    }
}

impl ConnectionStatus {
    pub fn disconnected(backoff_strategy: BackoffStrategy) -> Self {
        let reconnect_state = ReconnectState::from(backoff_strategy);
        ConnectionStatus::Disconnected(reconnect_state)
    }
}

pub struct ReconnectState {
    pub attempts: AttemptsIterator,
    pub current_attempt: AttemptFut,
}

impl From<BackoffStrategy> for ReconnectState {
    fn from(strategy: BackoffStrategy) -> Self {
        let attempts = Box::new(strategy.into_iter());
        let current_attempt = Box::pin(async { unreachable!() });
        Self { attempts, current_attempt }
    }
}
