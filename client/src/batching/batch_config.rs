use std::time::Duration;

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
    pub fn new(batch_size: u32, interval: Duration) -> Self {
        Self {
            batch_size,
            interval,
        }
    }

    pub fn high_throughput() -> Self {
        Self::new(250, Duration::from_millis(100))
    }

    pub fn balanced() -> Self {
        Self::new(100, Duration::from_millis(100))
    }

    pub fn minimal_payload() -> Self {
        Self::new(10, Duration::from_millis(100))
    }

    pub fn batch_size(mut self, size: u32) -> Self {
        self.batch_size = size;
        self
    }

    pub fn interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
}
