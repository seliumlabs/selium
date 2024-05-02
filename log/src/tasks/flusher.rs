use crate::{config::SharedLogConfig, error::Result, segment::SharedSegmentList};
use std::sync::Arc;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio_util::sync::CancellationToken;

/// Task container for the asynchronous flusher task.
///
/// The FlusherTask container spawns an asynchronous background task that polls the flushing interval
/// of the log's FlushPolicy and triggers a flush once elapsed.
#[derive(Debug)]
pub struct FlusherTask {
    segments: SharedSegmentList,
    config: SharedLogConfig,
    cancellation_token: CancellationToken,
}

impl FlusherTask {
    /// Starts the background task and returns a reference to the task container.
    pub fn start(config: SharedLogConfig, segments: SharedSegmentList) -> (Arc<Self>, Sender<()>) {
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
                    self.segments.write().await.flush().await?;
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
    /// When the task container is dropped, a cancel signal will be dispatched in order to gracefully
    /// terminate the background task.
    fn drop(&mut self) {
        self.cancellation_token.cancel();
    }
}
