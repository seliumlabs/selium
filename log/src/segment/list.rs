use super::Segment;
use crate::config::SharedLogConfig;
use crate::error::{LogError, Result};
use crate::message::{Message, MessageSlice};
use futures::future::try_join_all;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

pub type SharedSegmentList = Arc<RwLock<SegmentList>>;

#[derive(Debug)]
pub struct SegmentList {
    config: SharedLogConfig,
    segments: BTreeMap<u64, Segment>,
    number_of_entries: u64,
    writes_since_last_flush: u64,
}

impl SegmentList {
    pub fn new(segments: BTreeMap<u64, Segment>, config: SharedLogConfig) -> Self {
        let number_of_entries = segments
            .iter()
            .last()
            .map_or(0, |(_, segment)| segment.end_offset);

        Self {
            segments,
            config,
            number_of_entries,
            writes_since_last_flush: 0,
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
        if offset > self.number_of_entries {
            return Ok(MessageSlice::empty(offset));
        }

        let found = self
            .segments
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
        let (_, segment) = self
            .segments
            .iter_mut()
            .last()
            .ok_or(LogError::SegmentListEmpty)?;

        segment.write(message).await?;

        if segment.is_full() {
            segment.flush().await?;
            let new_offset = segment.end_offset() + 1;
            let new_segment = Segment::create(new_offset, self.config.clone()).await?;
            self.segments.insert(new_offset, new_segment);
            self.on_flush();
        } else {
            self.writes_since_last_flush += 1;
        }

        Ok(())
    }

    pub async fn flush(&mut self) -> Result<()> {
        if self.writes_since_last_flush > 0 {
            let (_, segment) = self
                .segments
                .iter_mut()
                .last()
                .ok_or(LogError::SegmentListEmpty)?;

            segment.flush().await?;
            self.on_flush();
        }

        Ok(())
    }

    pub async fn find_stale_segments(&self, stale_duration: Duration) -> Result<Vec<u64>> {
        let mut stale_segments = vec![];

        for (offset, segment) in self.segments.iter() {
            if segment.is_stale(stale_duration).await? {
                stale_segments.push(*offset);
            }
        }

        Ok(stale_segments)
    }

    pub async fn remove_segments(&mut self, offsets: Vec<u64>) -> Result<()> {
        let mut tasks = Vec::with_capacity(offsets.len());

        for offset in offsets {
            if let Some(segment) = self.segments.remove(&offset) {
                tasks.push(segment.remove());
            }
        }

        try_join_all(tasks).await?;

        Ok(())
    }

    pub fn number_of_entries(&self) -> u64 {
        self.number_of_entries
    }

    pub fn writes_since_last_flush(&self) -> u64 {
        self.writes_since_last_flush
    }

    fn on_flush(&mut self) {
        self.number_of_entries += self.writes_since_last_flush;
        self.writes_since_last_flush = 0;
    }
}
