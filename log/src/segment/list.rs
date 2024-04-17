use super::Segment;
use crate::config::SharedLogConfig;
use crate::error::{LogError, Result};
use crate::message::{Message, MessageSlice};
use futures::future::try_join_all;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
pub struct SegmentList {
    config: SharedLogConfig,
    segments: Arc<RwLock<BTreeMap<u64, Segment>>>,
}

impl SegmentList {
    pub fn new(segments: BTreeMap<u64, Segment>, config: SharedLogConfig) -> Self {
        Self {
            segments: Arc::new(RwLock::new(segments)),
            config,
        }
    }

    pub async fn create(config: SharedLogConfig) -> Result<Self> {
        let hot_segment = Segment::create(0, config.clone()).await?;
        let segments = BTreeMap::from([(0, hot_segment)]);
        Ok(Self::new(segments, config))
    }

    pub async fn from_offsets(offsets: &[u64], config: SharedLogConfig) -> Result<Self> {
        let mut segments = BTreeMap::new();

        for offset in offsets {
            let segment = Segment::open(*offset, config.clone()).await?;
            segments.insert(*offset, segment);
        }

        Ok(Self::new(segments, config))
    }

    pub async fn read_slice(&self, offset: u64, limit: Option<u64>) -> Result<MessageSlice> {
        let list = self.segments.read().await;

        let found = list
            .iter()
            .rev()
            .find(|(&base_offset, _)| offset >= base_offset);

        if let Some((_, segment)) = found {
            let slice = segment.read_slice(offset, limit).await?;
            Ok(slice)
        } else {
            Ok(MessageSlice::default())
        }
    }

    pub async fn write(&mut self, message: Message) -> Result<()> {
        let mut list = self.segments.write().await;
        let (_, segment) = list.iter_mut().last().ok_or(LogError::SegmentListEmpty)?;
        segment.write(message).await?;

        if segment.is_full() {
            segment.flush().await?;
            let new_offset = segment.end_offset() + 1;
            let new_segment = Segment::create(new_offset, self.config.clone()).await?;
            list.insert(new_offset, new_segment);
        }

        Ok(())
    }

    pub async fn flush(&self) -> Result<()> {
        let mut list = self.segments.write().await;
        let (_, segment) = list.iter_mut().last().ok_or(LogError::SegmentListEmpty)?;
        segment.flush().await?;

        Ok(())
    }

    pub async fn find_stale_segments(&self, stale_duration: Duration) -> Result<Vec<u64>> {
        let segments = self.segments.read().await;
        let mut stale_segments = vec![];

        for (offset, segment) in segments.iter() {
            if segment.is_stale(stale_duration).await? {
                stale_segments.push(*offset);
            }
        }

        Ok(stale_segments)
    }

    pub async fn number_of_entries(&self) -> u64 {
        let list = self.segments.read().await;

        list.iter()
            .fold(0, |acc, (_, segment)| acc + segment.end_offset())
    }

    pub async fn remove_segments(&self, offsets: Vec<u64>) -> Result<()> {
        let mut list = self.segments.write().await;
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
