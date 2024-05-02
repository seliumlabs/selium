use bytes::Bytes;
use fake::Fake;
use selium_log::{
    config::{LogConfig, SharedLogConfig},
    message::Message,
    MessageLog,
};
use std::sync::Arc;
use tokio::fs;

fn generate_dummy_message() -> String {
    (16..32).fake::<String>()
}

pub fn generate_dummy_messages(count: usize) -> Vec<String> {
    (0..count)
        .map(|_| generate_dummy_message())
        .collect::<Vec<_>>()
}

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

    pub async fn write_records(&mut self, records: &[String]) {
        for record in records {
            self.write(&record).await;
        }
    }

    pub async fn write_dummy_records(&mut self, count: usize) {
        for _ in 0..count {
            let message = generate_dummy_message();
            self.write(&message).await;
        }
    }

    pub async fn read_records(&mut self, offset: u64, limit: Option<u64>) -> Vec<String> {
        let mut slice = self
            .log
            .read_slice(offset, limit)
            .await
            .unwrap()
            .messages()
            .unwrap();

        let mut messages = vec![];

        while let Ok(Some(message)) = slice.next().await {
            let message = String::from_utf8(message.records().to_vec()).unwrap();
            messages.push(message);
        }

        messages
    }

    pub async fn flush(&mut self) {
        self.log.flush().await.unwrap();
    }

    async fn write(&mut self, message: &str) {
        let batch = Bytes::from(message.to_owned());
        let message = Message::single(&batch, 1);
        self.log.write(message).await.unwrap();
    }
}
