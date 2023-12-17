use std::time::Duration;

/// Configuration type used for tuning the [message batching](crate::batching) algorithm parameters.
///
/// The `BatchConfig` type includes high-level associated functions for constructing preset batching
/// configurations, which suit most use cases.
///
/// ```
/// use selium::batching::BatchConfig;
/// use std::time::Duration;
///
/// let high_throughput = BatchConfig::high_throughput();
/// let balanced = BatchConfig::balanced();
/// let minimal = BatchConfig::minimal_payload();
/// ```
///
/// However, if the presets are not to your liking, you can use the standard `new` constructor to
/// specify your own `batch_size` and `interval` properties.
///
/// ```
/// use selium::batching::BatchConfig;
/// use std::time::Duration;
///
/// let custom = BatchConfig::new(1_000, Duration::from_millis(10));
/// ```
///
/// It's also possible to tweak an existing preset via the `BatchConfig::batch_size` and
/// `BatchConfig::interval` methods.
///
/// ```
/// use selium::batching::BatchConfig;
/// use std::time::Duration;
///
/// let tweaked_high_throughput = BatchConfig::high_throughput()
///     .interval(Duration::from_millis(10));
/// ```
#[derive(Debug, Clone)]
pub struct BatchConfig {
    pub(crate) batch_size: u32,
    pub(crate) interval: Duration,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self::balanced()
    }
}

impl BatchConfig {
    /// Constructs a new `BatchConfig` instance with the provided `batch_size` and `interval`
    /// arguments.
    pub fn new(batch_size: u32, interval: Duration) -> Self {
        Self {
            batch_size,
            interval,
        }
    }

    /// Constructs a new `BatchConfig` instance with a high throughput preset.
    ///
    /// Typically used when a [Publisher](crate::streams::pubsub::Publisher) sends small messages at a high rate.
    pub fn high_throughput() -> Self {
        Self::new(250, Duration::from_millis(100))
    }

    /// Constructs a new `BatchConfig` instance with a balanced preset.
    ///
    /// Typically used to acheive a balance between throughput and payload size.
    /// [Publisher](crate::streams::pubsub::Publisher) streams that send mid-sized messages at a high rate
    /// will get the most benefit out of this preset.
    pub fn balanced() -> Self {
        Self::new(100, Duration::from_millis(100))
    }

    /// Constructs a new `BatchConfig` instance with a preset that favours minimal payloads.
    ///
    /// Typically used when a [Publisher](crate::streams::pubsub::Publisher) sends large messages, and wants to
    /// remain mindful of the wire protocol message size limits while still optimizing network
    /// calls and compression.
    pub fn minimal_payload() -> Self {
        Self::new(10, Duration::from_millis(100))
    }

    /// Updates the `batch_size` of a `BatchConfig` instance. Helpful for tweaking existing presets.
    pub fn batch_size(mut self, size: u32) -> Self {
        self.batch_size = size;
        self
    }

    /// Updates the `interval` of a `BatchConfig` instance. Helpful for tweaking existing presets.
    pub fn interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
}
