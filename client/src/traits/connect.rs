use anyhow::Result;
use async_trait::async_trait;

use crate::aliases::Streams;

#[async_trait]
pub trait Connect {
    type Output;

    async fn register(self, streams: &mut Streams) -> Result<()>;
    async fn connect(self, host: &str) -> Result<Self::Output>;
}
