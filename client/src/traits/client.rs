use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Client {
    async fn finish(self) -> Result<()>;
}
