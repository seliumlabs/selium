use std::time::Duration;

#[derive(Debug, Clone)]
pub struct FlushPolicy {
    pub(crate) number_of_writes: Option<u64>,
    pub(crate) interval: Duration,
}

impl Default for FlushPolicy {
    fn default() -> Self {
        Self {
            number_of_writes: None,
            interval: Duration::from_secs(3),
        }
    }
}

impl FlushPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn every_write(mut self) -> Self {
        self.number_of_writes = Some(1);
        self
    }

    pub fn number_of_writes(mut self, num: u64) -> Self {
        self.number_of_writes = Some(num);
        self
    }

    pub fn interval(mut self, interval: Duration) -> Self {
        self.interval = interval;
        self
    }
}
