use bytes::Bytes;
use selium_log::{
    config::{LogConfig, SharedLogConfig},
    data::LogIterator,
    message::{Headers, Message},
    MessageLog,
};
use std::sync::Arc;
use tokio::fs;

pub struct TestWrapper {
    log: MessageLog,
    config: SharedLogConfig,
}

impl TestWrapper {
    pub async fn build(config: LogConfig) -> Self {
        let config = Arc::new(config);
        let log = MessageLog::open(config.clone()).await.unwrap();

        Self { log, config }
    }

    pub async fn number_of_segments(&self) -> u64 {
        let mut segments_count = 0;
        let mut dir = fs::read_dir(&self.config.segments_path).await.unwrap();

        while let Some(entry) = dir.next_entry().await.unwrap() {
            let path = entry.path();

            if path.is_file() && path.extension() == Some("index".as_ref()) {
                segments_count += 1;
            }
        }

        segments_count
    }

    pub async fn write_batch(&mut self, message: &str) {
        let batch = Bytes::from(message.to_owned());
        let headers = Headers::new(batch.len(), 1, 1);
        let message = Message::new(headers, &batch);
        self.log.write(message).await.unwrap();
    }

    pub async fn read_records(&mut self, offset: u64, limit: Option<u64>) -> Option<LogIterator> {
        let slice = self.log.read_slice(offset, limit).await.unwrap();
        slice.messages()
    }

    pub async fn flush(&mut self) {
        self.log.flush().await.unwrap();
    }
}
