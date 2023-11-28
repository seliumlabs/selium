use super::TryIntoU64;
use async_trait::async_trait;
use selium_std::errors::Result;

/// Provides an `open` method for [StreamBuilder](crate::StreamBuilder) implementations to
/// construct and spawn a new stream.
#[async_trait]
pub trait Open {
    type Output;

    /// Constructs headers to register a stream with the `Selium` server, and constructs the output
    /// type corresponding to the type of stream.
    ///
    /// For example, when constructing a [Publisher](crate::Publisher) stream via a
    /// [StreamBuilder](crate::StreamBuilder), the open method will construct publisher headers,
    /// and then spawn a new [Publisher](crate::Publisher) stream.
    ///
    /// # Errors
    ///
    /// Returns [Err] if a failure occurs while spawning the stream.
    async fn open(self) -> Result<Self::Output>;
}

#[doc(hidden)]
pub trait Retain {
    fn retain<T: TryIntoU64>(self, policy: T) -> Result<Self>
    where
        Self: Sized;
}

#[doc(hidden)]
pub trait Operations {
    fn map(self, module_path: &str) -> Self;
    fn filter(self, module_path: &str) -> Self;
}
