use selium_std::errors::{Result, SeliumError};

/// Provides a `try_into_u64` method to allow implementors to fallibly convert a suitable type to a
/// [u64]
pub trait TryIntoU64 {
    /// Consumes the input and fallibly converts it to a [u64].
    fn try_into_u64(self) -> Result<u64>;
}

impl TryIntoU64 for u64 {
    fn try_into_u64(self) -> Result<u64> {
        Ok(self)
    }
}

impl TryIntoU64 for std::time::Duration {
    /// Converts a std [Duration](std::time::Duration) value into a [u64] by representing the
    /// duration in milliseconds.
    ///
    /// # Errors
    ///
    /// Because the [as_millis](std::time::Duration::as_millis) method
    /// returns a [u128], this conversion may fail due to potential data loss in the demotion of
    /// the integer.
    fn try_into_u64(self) -> Result<u64> {
        self.as_millis()
            .try_into()
            .map_err(|_| SeliumError::ParseDurationMillis)
    }
}

#[cfg(feature = "chrono")]
impl TryIntoU64 for chrono::Duration {
    /// Converts a [chrono::Duration] value into a [u64] by representing the duration in
    /// milliseconds.
    ///
    /// # Errors
    ///
    /// Because the [num_milliseconds](chrono::Duration::num_milliseconds) method
    /// returns an [i64], this conversion may fail due to negative values.
    fn try_into_u64(self) -> Result<u64> {
        self.num_milliseconds()
            .try_into()
            .map_err(|_| SeliumError::ParseDurationMillis)
    }
}
