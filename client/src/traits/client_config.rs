use super::TryIntoU64;
use anyhow::Result;
use std::path::PathBuf;

pub trait ClientConfig {
    type NextState;

    fn map(self, module_path: &str) -> Self;
    fn filter(self, module_path: &str) -> Self;

    fn keep_alive<T: TryIntoU64>(self, interval: T) -> Result<Self>
    where
        Self: Sized;

    fn retain<T: TryIntoU64>(self, policy: T) -> Result<Self>
    where
        Self: Sized;

    fn with_certificate_authority<T: Into<PathBuf>>(self, ca_path: T) -> Result<Self::NextState>;
}
