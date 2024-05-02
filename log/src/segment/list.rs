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

/// Wrapper type over a [BTreeMap] of [Segment] instances, indexed by their base offset.
///
/// The SegmentList maintains all active segments in the log, and manages writing, reading and removing
/// segments from the log.
///
/// Upon construction, the SegmentList will contain at least one "hot" segment.
///
/// Segments will eventually become full, depending on the configured [LogConfig::max_index_entries](crate::config::LogConfig::max_index_entries)
/// setting, so new segments will be created to keep write buffers at reasonable sizes, allow efficient seeking, and
/// reduce overhead when synchronizing the memory-mapped index with the filesystem.
#[derive(Debug)]
pub struct SegmentList {
    config: SharedLogConfig,
    segments: BTreeMap<u64, Segment>,
    number_of_entries: u64,
    writes_since_last_flush: u64,
}

impl SegmentList {
    /// Constructs a new SegmentList instance.
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

    /// Constructs a SegmentList instance, creating an initial hot segment, beginning at
    /// base offset 0.
    ///
    /// # Errors
    /// - Returns Err if the hot segment fails to be created.
    pub async fn create(config: SharedLogConfig) -> Result<Self> {
        let hot_segment = Segment::create(0, config.clone()).await?;
        let segments = BTreeMap::from([(0, hot_segment)]);
        Ok(Self::new(segments, config))
    }

    /// Constructs a SegmentList instance, opening each index/data file pair from the provided
    /// slice of base offsets.
    ///
    /// # Errors
    /// - Returns Err if any segments fail to open.
    pub async fn from_offsets(offsets: &[u64], config: SharedLogConfig) -> Result<Self> {
        let mut segments = BTreeMap::new();

        for offset in offsets {
            let segment = Segment::open(*offset, config.clone()).await?;
            segments.insert(*offset, segment);
        }

        Ok(Self::new(segments, config))
    }

    /// Reads a range of messages from a segment identified by the provided offset.
    ///
    /// Returns an empty [MessageSlice] if the provided offset is greater than the total
    /// amount of entries in the log.
    ///
    /// # Params
    /// * `offset` - The starting offset, used to locate the segment and relative offset.
    /// * `limit` - An optional message read limit.
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

    /// Writes the provided [Message] to the current hot segment.
    ///
    /// If the hot segment is at full capacity following the write, the current hot segment
    /// will be flushed, and a new segment will be created and designated as the hot segment in
    /// its place. Otherwise, the `writes_since_last_flush` field is incremented by 1.
    ///
    /// # Errors
    /// - Returns [LogError::SegmentListEmpty] if there are no segments in the list yet.
    /// - Returns Err if writing to the hot segment fails.
    /// - Returns Err if the segment is full, and the current hot segment fails to flush.
    /// - Returns Err if the segment is full, and the new hot segment fails to be created.
    pub async fn write(&mut self, message: Message) -> Result<()> {
        if self.segments.is_empty() {
            let hot_segment = Segment::create(0, self.config.clone()).await?;
            self.segments.insert(0, hot_segment);
        }

        let (_, segment) = self
            .segments
            .iter_mut()
            .last()
            .ok_or(LogError::SegmentListEmpty)?;

        segment.write(message).await;

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

    /// Flushes the hot segment to the filesystem.
    /// This function is a no-op if no writes have occurred prior to calling flush.
    ///
    /// # Errors
    /// - Returns Err if the hot segment fails to flush.
    pub async fn flush(&mut self) -> Result<()> {
        if self.writes_since_last_flush > 0 {
            if let Some((_, segment)) = self.segments.iter_mut().last() {
                segment.flush().await?;
                self.on_flush();
            }
        }

        Ok(())
    }

    /// Identifies any stale segments in the list, and returns a [Vec] of base offsets corresponding
    /// to those segments.
    ///
    /// # Errors
    /// - Returns Err if the data file metadata cannot be accessed for any of the segments.
    pub async fn find_stale_segments(&self, stale_duration: Duration) -> Result<Vec<u64>> {
        let mut stale_segments = vec![];

        for (offset, segment) in self.segments.iter() {
            if segment.is_stale(stale_duration).await? {
                stale_segments.push(*offset);
            }
        }

        Ok(stale_segments)
    }

    /// Removes one or more segments, based on a provided collection of base offsets.
    ///
    /// # Errors
    /// - Returns Err if any of the identified segments fail to be removed.
    pub async fn remove_segments(&mut self, offsets: &[u64]) -> Result<()> {
        let mut tasks = Vec::with_capacity(offsets.len());

        for offset in offsets {
            if let Some(segment) = self.segments.remove(offset) {
                tasks.push(segment.remove());
            }
        }

        try_join_all(tasks).await?;

        Ok(())
    }

    /// The total number of entries in the log, summed across all segments.
    pub fn number_of_entries(&self) -> u64 {
        self.number_of_entries
    }

    /// The number of writes since the last flush.
    /// Used for determining whether a hot segment should be flushed based on a provided
    /// [FlushPolicy](crate::config::FlushPolicy).
    pub fn writes_since_last_flush(&self) -> u64 {
        self.writes_since_last_flush
    }

    fn on_flush(&mut self) {
        self.number_of_entries += self.writes_since_last_flush;
        self.writes_since_last_flush = 0;
    }
}
