use anyhow::{Context, Result};

pub trait IntoTimestamp {
    fn into_timestamp(self) -> Result<u64>;
}

impl IntoTimestamp for u64 {
    fn into_timestamp(self) -> Result<u64> {
        Ok(self)
    }
}

impl IntoTimestamp for std::time::Duration {
    fn into_timestamp(self) -> Result<u64> {
        Ok(self.as_secs())
    }
}

impl IntoTimestamp for chrono::Duration {
    fn into_timestamp(self) -> Result<u64> {
        let timestamp = self
            .num_seconds()
            .try_into()
            .context("Timestamp must be a non-negative integer")?;

        Ok(timestamp)
    }
}
