use anyhow::Result;

pub trait TryIntoU64 {
    fn try_into_u64(self) -> Result<u64>;
}

impl TryIntoU64 for u64 {
    fn try_into_u64(self) -> Result<u64> {
        Ok(self)
    }
}

impl TryIntoU64 for std::time::Duration {
    fn try_into_u64(self) -> Result<u64> {
        Ok(self.as_secs())
    }
}

#[cfg(feature = "chrono")] 
impl TryIntoU64 for chrono::Duration {
    fn try_into_u64(self) -> Result<u64> {
        use anyhow::Context;

        let seconds = self
            .num_seconds()
            .try_into()
            .context("Timestamp must be a non-negative integer")?;

        Ok(seconds)
    }
}
