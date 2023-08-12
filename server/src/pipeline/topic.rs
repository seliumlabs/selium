use crate::sink::{FanoutChannel, FanoutChannelHandle};
use crate::stream::{MergeChannel, MergeChannelHandle};
use anyhow::Result;
use futures::StreamExt;
use quinn::Connection;
use selium_common::protocol::{PublisherPayload, SubscriberPayload};
use selium_common::types::BiStream;
use std::ops::Deref;
use tokio_stream::StreamNotifyClose;

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct SockHash(String);

impl Deref for SockHash {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct Topic {
    fanout: Option<FanoutChannelHandle<BiStream>>,
    merge: Option<MergeChannelHandle<BiStream>>,
}

impl Default for Topic {
    fn default() -> Self {
        let (fanout, fanout_handle) = FanoutChannel::pair();
        let (merge, merge_handle) = MergeChannel::pair();

        // Spawn the topic stream, which will remain paused until at least one stream and
        // sink are added.
        tokio::spawn(merge.forward(fanout));

        Topic {
            fanout: Some(fanout_handle),
            merge: Some(merge_handle),
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

        self.merge
            .as_ref()
            .unwrap()
            .add_stream(StreamNotifyClose::new(stream))
            .await
            .unwrap();

        println!("Add Pub");

        // let _ = conn_handle.closed().await;

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

        self.fanout.as_ref().unwrap().add_sink(sink).await.unwrap();

        println!("Add Sub");

        // let _ = conn_handle.closed().await;

        Ok(())
    }
}
