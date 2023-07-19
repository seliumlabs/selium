use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Connect {
    type Output;

    async fn connect(self, host: &str) -> Result<Self::Output>;
}
