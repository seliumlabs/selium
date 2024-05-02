use std::time::Duration;

/// Defines the flushing policy for a message log.
/// Flushing is triggered by a defined interval, and optionally, a write threshold.
#[derive(Debug, Clone)]
pub struct FlushPolicy {
    /// An optional write-count threshold. When the threshold is exceeded, a flush will be triggered.
    pub(crate) number_of_writes: Option<u64>,
    /// The flushing interval for the log. Triggers a flush when the interval elapses.
    pub(crate) interval: Duration,
}

impl Default for FlushPolicy {
    /// By default, no write-count threshold is defined, and is left as a user-provided optimization
    /// depending on the throughput and durability requirements of the log.
    fn default() -> Self {
        Self {
            number_of_writes: None,
            interval: Duration::from_secs(3),
        }
    }
}

impl FlushPolicy {
    /// Creates a new FlushPolicy with suitable defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Opts-in to flushing based on a write-count threshold, and specifies that the flush should be
    /// triggered on every write.
    pub fn every_write(mut self) -> Self {
        self.number_of_writes = Some(1);
        self
    }

    /// Opts-in to flushing based on a write-count threshhold, and specifies that the flush should be
    /// triggered after the provided number of writes.
    pub fn number_of_writes(mut self, num: u64) -> Self {
        self.number_of_writes = Some(num);
        self
    }

    /// Overrides the default flushing interval.
    pub fn interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
}
