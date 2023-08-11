use anyhow::Result;
use quinn::Connection;
use selium_common::protocol::{PublisherPayload, SubscriberPayload};
use selium_common::types::BiStream;
use tokio_stream::StreamNotifyClose;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use crate::sink::{FanoutChannel, FanoutChannelHandle};
use crate::stream::{MergeChannel, MergeChannelHandle};

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct SockHash(String);

impl Deref for SockHash {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct Topic {
    spawned: AtomicBool,
    fanout: FanoutChannel<BiStream>,
    merge: Option<MergeChannel<BiStream>>,
    fanout_handle: FanoutChannelHandle<BiStream>,
    merge_handle: MergeChannelHandle<BiStream>,
}

impl Default for Topic {
    fn default() -> Self {
        let (fanout, fanout_handle) = FanoutChannel::pair();
        let (merge, merge_handle) = MergeChannel::pair();

        Topic {
            spawned: AtomicBool::new(false),
            fanout,
            merge: Some(merge),
            fanout_handle,
            merge_handle,
        }
    }
}

impl Topic {
    pub async fn add_publisher(
        &mut self,
        _header: PublisherPayload,
        conn_handle: Connection,
        stream: BiStream,
    ) -> Result<()> {
        // @TODO - Support Operations struct, compose stream.
        let stream = stream;

        self.merge_handle.add_stream(StreamNotifyClose::new(stream)).await.unwrap();

        if !self.spawned.swap(true, Ordering::Acquire) {
            self.spawn_pipeline();
        }

        let _ = conn_handle.closed().await;

        Ok(())
    }

    pub async fn add_subscriber(
        &mut self,
        _header: SubscriberPayload,
        conn_handle: Connection,
        sink: BiStream,
    ) -> Result<()> {
        // @TODO - Support Operations struct, compose sink.
        let sink = sink;

        self.fanout_handle.add_sink(sink).await.unwrap();

        if !self.spawned.swap(true, Ordering::Acquire) {
            self.spawn_pipeline();
        }

        let _ = conn_handle.closed().await;

        Ok(())
    }

    fn spawn_pipeline(&mut self) {
        // tokio::spawn({
        //     let merge = self.merge.take().unwrap();
        //
        //     async move {
        //         if let Err(e) = merge.forward(&mut self.fanout).await {
        //             //
        //         }
        //     }
        // });
    }
}
