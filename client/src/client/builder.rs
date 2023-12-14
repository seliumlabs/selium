use crate::constants::KEEP_ALIVE_DEFAULT;
use crate::keep_alive::BackoffStrategy;
use crate::traits::TryIntoU64;
use selium_std::errors::Result;

/// A convenient builder struct used to build a [Client](crate::Client) instance.
///
/// The [ClientBuilder] uses a type-level Finite State Machine to assure that a
/// [Client](crate::Client) cannot be constructed with an invalid state. For example, the
/// [connect](ClientBuilder::connect) method will not be in-scope unless the [ClientBuilder] is in
/// a pre-connection state, which is achieved by first configuring the root store and keypair.
///
/// **NOTE:** The [ClientBuilder] type is not intended to be used directly. Use the
/// [cloud](crate::cloud) or [custom](crate::custom) functions to construct a [ClientBuilder] in
/// its initial state.
#[derive(Debug)]
pub struct ClientBuilder<T> {
    pub(crate) state: T,
}

impl<T> ClientBuilder<T> {
    pub fn new(state: T) -> Self {
        Self { state }
    }
}

/// Common state for all client types.
#[derive(Debug)]
pub struct ClientCommon {
    pub(crate) keep_alive: u64,
    pub(crate) backoff_strategy: BackoffStrategy,
}

impl Default for ClientCommon {
    fn default() -> Self {
        Self {
            keep_alive: KEEP_ALIVE_DEFAULT,
            backoff_strategy: BackoffStrategy::default(),
        }
    }
}

impl ClientCommon {
    /// Overrides the `keep_alive` interval for the client connection in milliseconds.
    ///
    /// Accepts any `interval` argument that can be *fallibly* converted into a [u64] via the
    /// [TryIntoU64](crate::traits::TryIntoU64) trait.
    ///
    /// **NOTE:** `Selium` already provides a reasonable default for the `keep_alive` interval (see
    /// [KEEP_ALIVE_DEFAULT]), so this setting should only be overridden if it's not suitable for
    /// your use-case.
    ///
    /// # Errors
    ///
    /// Returns [Err] if the provided interval fails to be convert to a [u64].
    ///
    /// # Examples
    ///
    /// Overriding the default `keep_alive` interval as 6 seconds (represented in milliseconds).
    ///  
    /// ```
    /// let client = selium::custom()
    ///     .keep_alive(6_000).unwrap();
    /// ```
    ///
    /// You can even use any other type that can be converted to a [u64] via the
    /// [TryIntoU64](crate::traits::TryIntoU64) trait, such as the standard library's Duration
    /// type.
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// let client = selium::custom()
    ///     .keep_alive(Duration::from_secs(6)).unwrap();
    /// ```
    pub fn keep_alive<T: TryIntoU64>(&mut self, interval: T) -> Result<()> {
        self.keep_alive = interval.try_into_u64()?;
        Ok(())
    }

    /// Overrides the `backoff_strategy` used to recover a connection and streams when transient
    /// errors occur.
    ///
    /// See the [keep_alive](crate::keep_alive) module for more information.
    ///
    /// # Examples
    ///
    /// Overriding the default `backoff_strategy` to use a linear strategy with 5 attempts, and a
    /// duration step of 2 seconds.
    ///
    /// ```
    /// use selium::keep_alive::BackoffStrategy;
    /// use std::time::Duration;
    ///
    /// let strategy = BackoffStrategy::linear()
    ///     .with_max_attempts(5)
    ///     .with_step(Duration::from_secs(2));
    ///
    /// let client = selium::custom()
    ///     .backoff_strategy(strategy);
    /// ```
    pub fn backoff_strategy(&mut self, strategy: BackoffStrategy) {
        self.backoff_strategy = strategy;
    }
}
