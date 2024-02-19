use super::Segment;
use crate::config::SharedLogConfig;
use anyhow::Result;
use futures::StreamExt;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Debug)]
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

    pub async fn push(&self, segment: Segment) {
        let mut list = self.0.write().await;
        list.insert(segment.base_offset(), segment);
    }

    pub async fn remove(&self, offset: u64) {
        let mut list = self.0.write().await;
        list.remove(&offset);
    }

    pub async fn from_offsets(
        path: impl AsRef<Path>,
        offsets: &[u64],
        config: SharedLogConfig,
    ) -> Result<Self> {
        let path = path.as_ref();

        let segments_futs = futures::stream::iter(offsets.iter())
            .map(|&offset| {
                let config = config.clone();
                async move {
                    Segment::open(path, offset, config.clone())
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
}
