use crate::sink::{FanoutChannel, FanoutChannelHandle};
use crate::stream::{MergeChannel, MergeChannelHandle};
use anyhow::Result;
use futures::StreamExt;
use selium_common::types::BiStream;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_stream::StreamNotifyClose;
use super::exclusive::Exclusive;

pub struct Channels<'a> {
    spawned: AtomicBool,
    fanout: Option<FanoutChannel<Exclusive<'a, BiStream>>>,
    merge: Option<MergeChannel<Exclusive<'a, BiStream>>>,
    fanout_handle: FanoutChannelHandle<Exclusive<'a, BiStream>>,
    merge_handle: MergeChannelHandle<Exclusive<'a, BiStream>>,
}

impl<'a> Channels {
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

    pub async fn add_stream(&self, stream: Exclusive<'a, BiStream>) -> Result<()> {
        let stream = StreamNotifyClose::new(stream);

        if self.spawned.load(Ordering::Acquire) {
            self.merge_handle.add_stream(stream).await
        } else {
            // self.merge.add_stream(stream);
            Ok(())
        }
    }

    pub async fn add_sink(&self, sink: Exclusive<'a, BiStream>) -> Result<()> {
        self.fanout_handle.add_sink(sink).await
    }
}
