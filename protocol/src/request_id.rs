use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug)]
pub struct RequestId(AtomicU32);

impl Default for RequestId {
    fn default() -> Self {
        Self(AtomicU32::new(0))
    }
}

impl RequestId {
    pub fn next_id(&self) -> u32 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}
