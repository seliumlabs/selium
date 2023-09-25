use super::BatchConfig;
use bytes::Bytes;
use std::time::Instant;

#[derive(Debug)]
pub struct MessageBatch {
    batch: Vec<Bytes>,
    config: BatchConfig,
    last_run: Instant,
}

impl MessageBatch {
    pub fn push(&mut self, value: Bytes) {
        self.batch.push(value);
    }

    pub fn drain(&mut self) -> Vec<Bytes> {
        let batch = self.batch.drain(..);
        batch.collect()
    }

    pub fn is_empty(&self) -> bool {
        self.batch.is_empty()
    }

    pub fn update_last_run(&mut self, instant: Instant) {
        self.last_run = instant;
    }

    pub fn exceeded_interval(&self, now: Instant) -> bool {
        now >= self.last_run + self.config.interval
    }

    pub fn exceeded_batch_size(&self) -> bool {
        self.batch.len() >= self.config.batch_size as usize
    }

    pub fn is_ready(&self, now: Instant) -> bool {
        self.exceeded_interval(now) || self.exceeded_batch_size()
    }
}

impl From<BatchConfig> for MessageBatch {
    fn from(config: BatchConfig) -> Self {
        let batch = Vec::with_capacity(config.batch_size as usize);
        let last_run = Instant::now();

        Self {
            batch,
            config,
            last_run,
        }
    }
}
