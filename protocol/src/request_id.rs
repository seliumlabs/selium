use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug)]
pub struct RequestId(AtomicU32);

impl RequestId {
    pub fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    pub fn next_id(&self) -> u32 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}
