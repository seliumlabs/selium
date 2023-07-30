use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use anyhow::{anyhow, Result};
use futures::{
    channel::mpsc,
    future::{self, select, Either},
    FutureExt, StreamExt, TryStreamExt,
};
use quinn::StreamId;
use selium::{
    protocol::{Frame, PublisherPayload, SubscriberPayload},
    BiStream,
};
use tokio::sync::Notify;

use crate::{ordered_sink::OrderedExt, pipeline::Pipeline};

pub struct Topic {
    pipeline: Pipeline,
    sequence: Arc<AtomicUsize>,
}

impl Topic {
    pub fn new(pipeline: Pipeline) -> Self {
        Self {
            pipeline,
            sequence: Arc::new(AtomicUsize::new(1)),
        }
    }

    pub async fn add_publisher(
        &self,
        header: PublisherPayload,
        conn_addr: SocketAddr,
        conn_notify: Arc<Notify>,
        stream: BiStream,
    ) -> Result<()> {
        let stream_hash = sock_key(conn_addr, stream.read.get_ref().id());

        self.pipeline.add_publisher(&stream_hash, header).await?;

        let messages = stream.try_for_each(|frame| match frame {
            Frame::Message(bytes) => {
                let seq = self.sequence.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(self.pipeline.traverse(&stream_hash, bytes, seq));
                future::ok(())
            }
            _ => future::err(anyhow!("Expected Message frame")),
        });

        if let Either::Left((r, _)) = select(messages, conn_notify.notified().boxed()).await {
            r?;
        }

        self.pipeline.rm_publisher(&stream_hash).await?;

        Ok(())
    }

    pub async fn add_subscriber(
        &self,
        header: SubscriberPayload,
        conn_addr: SocketAddr,
        conn_notify: Arc<Notify>,
        sink: BiStream,
    ) -> Result<()> {
        let stream_hash = sock_key(conn_addr, sink.write.get_ref().id());

        let (tx_chan, rx_chan) = mpsc::unbounded();
        self.pipeline
            .add_subscriber(&stream_hash, header, tx_chan)
            .await?;

        let forward = rx_chan
            .map(|(seq, bytes)| Ok((seq, Frame::Message(bytes))))
            .forward(sink.ordered(self.sequence.load(Ordering::SeqCst) - 1));

        if let Either::Left((r, _)) = select(forward, conn_notify.notified().boxed()).await {
            r?;
        }

        self.pipeline.rm_subscriber(&stream_hash).await?;

        Ok(())
    }
}

fn sock_key(conn_addr: SocketAddr, stream_id: StreamId) -> String {
    format!("{conn_addr}:{stream_id}")
}
