use crate::sink::{FanoutChannel, FanoutChannelHandle};
use crate::stream::{MergeChannel, MergeChannelHandle};
use anyhow::Result;
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_stream::StreamNotifyClose;

pub struct Channels<St, Si> {
    spawned: AtomicBool,
    fanout: Option<FanoutChannel<Si>>,
    merge: Option<MergeChannel<St>>,
    fanout_handle: FanoutChannelHandle<Si>,
    merge_handle: MergeChannelHandle<St>,
}

impl<St, Si> Channels<St, Si> {
    pub fn new() -> Self {
        let (fanout, fanout_handle) = FanoutChannel::pair();
        let (merge, merge_handle) = MergeChannel::pair();

        Self {
            spawned: AtomicBool::new(false),
            fanout: Some(fanout),
            merge: Some(merge),
            fanout_handle,
            merge_handle,
        }
    }

    pub fn spawn(&mut self) {
        if !self.spawned.swap(true, Ordering::Acquire) {
            let merge = self.merge.take().unwrap();
            let fanout = self.fanout.take().unwrap();

            tokio::spawn(async move { merge.forward(fanout) });
        }
    }

    pub async fn add_stream(&self, stream: St) -> Result<()> {
        let stream = StreamNotifyClose::new(stream);

        if self.spawned.load(Ordering::Acquire) {
            self.merge_handle.add_stream(stream).await
        } else {
            self.merge.add_stream(stream);
            Ok(())
        }
    }

    pub async fn add_sink(&self, sink: Si) -> Result<()> {
        self.fanout_handle.add_sink(sink).await
    }
}
