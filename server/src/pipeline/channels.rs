use std::sync::atomic::{AtomicBool, Ordering};
use anyhow::Result;
use futures::StreamExt;
use tokio_stream::StreamNotifyClose;
use crate::sink::{FanoutChannelHandle, FanoutChannel};
use crate::stream::{MergeChannel, MergeChannelHandle};
use super::{publisher::Publisher, subscriber::Subscriber};

pub struct Channels {
    spawned: AtomicBool,
    fanout: Option<FanoutChannel<Subscriber>>,
    merge: Option<MergeChannel<Publisher>>,
    fanout_handle: FanoutChannelHandle<Subscriber>,
    merge_handle: MergeChannelHandle<Publisher>,
}

impl Channels {
    pub fn new() -> Self {
        let (fanout, fanout_handle) = FanoutChannel::pair();
        let (merge, merge_handle) = MergeChannel::pair();

        Self {
            spawned: AtomicBool::new(false),
            fanout: Some(fanout),
            merge: Some(merge),
            fanout_handle,
            merge_handle
        }
    }

    pub fn spawn(&mut self) {
        if !self.spawned.swap(true, Ordering::Acquire) {
            let merge = self.merge.take().unwrap();
            let fanout = self.fanout.take().unwrap();

            tokio::spawn(async move { merge.forward(fanout) });
        }
    }

    pub async fn add_stream(&self, stream: Publisher) -> Result<()> {
        self.merge_handle.add_stream(StreamNotifyClose::new(stream)).await
    }

    pub async fn add_sink(&self, sink: Subscriber) -> Result<()> {
        self.fanout_handle.add_sink(sink).await
    }
}
