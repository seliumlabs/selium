use super::TryIntoU64;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Open {
    type Output;

    async fn open(self) -> Result<Self::Output>;
}

#[doc(hidden)]
pub trait StreamConfig {
    fn map(self, module_path: &str) -> Self;
    fn filter(self, module_path: &str) -> Self;
    fn retain<T: TryIntoU64>(self, policy: T) -> Result<Self>
    where
        Self: Sized;
}
