use crate::BiStream;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Connect {
    type Output;

    async fn register(self, streams: &mut BiStream) -> Result<()>;
    async fn connect(self, host: &str) -> Result<Self::Output>;
}
