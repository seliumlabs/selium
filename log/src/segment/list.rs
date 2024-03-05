use super::Segment;
use crate::config::SharedLogConfig;
use crate::message::{Message, MessageSlice};
use anyhow::{Context, Result};
use futures::future::try_join_all;
use futures::StreamExt;
use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct SegmentList(Arc<RwLock<BTreeMap<u64, Segment>>>);

impl Default for SegmentList {
    fn default() -> Self {
        Self::new(BTreeMap::new())
    }
}

impl SegmentList {
    pub fn new(segments: BTreeMap<u64, Segment>) -> Self {
        Self(Arc::new(RwLock::new(segments)))
    }

    pub async fn create(config: SharedLogConfig) -> Result<Self> {
        let hot_segment = Segment::create(0, config).await?;
        let segments = BTreeMap::from([(0, hot_segment)]);
        Ok(Self::new(segments))
    }

    pub async fn from_offsets(offsets: &[u64], config: SharedLogConfig) -> Result<Self> {
        let segments_futs = futures::stream::iter(offsets.iter())
            .map(|&offset| {
                let config = config.clone();
                async move {
                    Segment::open(offset, config.clone())
                        .await
                        .map(|segment| (offset, segment))
                }
            })
            .buffer_unordered(10)
            .collect::<Vec<_>>();

        let segments = segments_futs
            .await
            .into_iter()
            .collect::<Result<BTreeMap<_, _>>>()?;

        Ok(Self::new(segments))
    }

    pub async fn read_slice(&self, offset_range: Range<u64>) -> Result<MessageSlice> {
        let list = self.0.read().await;

        let found = list
            .iter()
            .find(|(&base_offset, _)| offset_range.start >= base_offset);

        if let Some((_, segment)) = found {
            let slice = segment.read_slice(offset_range).await?;
            Ok(slice)
        } else {
            Ok(MessageSlice::default())
        }
    }

    pub async fn push(&self, segment: Segment) {
        let mut list = self.0.write().await;
        list.insert(segment.base_offset(), segment);
    }

    pub async fn remove(&self, offset: u64) {
        let mut list = self.0.write().await;
        list.remove(&offset);
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        let mut list = self.0.write().await;
        let (_, segment) = list.iter_mut().last().context("Segment list is empty")?;
        segment.write(message).await?;
        Ok(())
    }

    pub async fn find_stale_segments(&self, stale_duration: Duration) -> Vec<u64> {
        let segments = self.0.read().await;

        segments
            .iter()
            .filter(|(_, segment)| segment.is_stale(stale_duration))
            .map(|(offset, _)| *offset)
            .collect()
    }

    pub async fn remove_segments(&self, offsets: Vec<u64>) -> Result<()> {
        let mut list = self.0.write().await;
        let mut tasks = Vec::with_capacity(offsets.len());

        for offset in offsets {
            if let Some(segment) = list.remove(&offset) {
                tasks.push(segment.remove());
            }
        }

        try_join_all(tasks).await?;

        Ok(())
    }
}
