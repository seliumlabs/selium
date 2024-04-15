use crate::{config::SharedLogConfig, error::Result, segment::SegmentList};
use std::sync::Arc;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct FlusherTask {
    segments: SegmentList,
    config: SharedLogConfig,
    cancellation_token: CancellationToken,
}

impl FlusherTask {
    pub fn start(config: SharedLogConfig, segments: SegmentList) -> (Arc<Self>, Sender<()>) {
        let cancellation_token = CancellationToken::new();
        let (tx, rx) = mpsc::channel(1);

        let task = Arc::new(Self {
            segments,
            config,
            cancellation_token,
        });

        tokio::spawn({
            let task = task.clone();
            async move {
                task.run(rx).await.unwrap();
            }
        });

        (task, tx)
    }

    async fn run(&self, mut rx: Receiver<()>) -> Result<()> {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(self.config.flush_policy.interval) => {
                    self.segments.flush().await?;
                },
                _ = rx.recv() => {
                    continue;
                }
                _ = self.cancellation_token.cancelled() => {
                    break Ok(());
                }
            }
        }
    }
}

impl Drop for FlusherTask {
    fn drop(&mut self) {
        self.cancellation_token.cancel();
    }
}
